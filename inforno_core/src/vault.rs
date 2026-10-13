//! API keys on disk, encrypted (the "vault").
//!
//! All keys (provider name → key) are stored as one blob sealed to this
//! machine and user, so the file is useless anywhere else (e.g. if it ends
//! up in a backup or a git repository), and needs no password: it unseals
//! silently, on this machine, for this user. The sealing is per platform
//! (see [`platform_sealer`]):
//!
//! - Linux: `systemd-creds --user`, bound to the TPM2 when the machine has
//!   one (otherwise to its root-only host key) and to the user;
//! - Windows: DPAPI, bound to the Windows user account.
//!
//! There is deliberately no recovery path. If the TPM is reset or the
//! machine replaced, the file can't be decrypted any more and the keys are
//! entered again (API keys are easy to re-issue).

use std::collections::BTreeMap;
use std::path::Path;

use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};

/// Bound into the sealed file: it can't be unsealed as any other credential.
pub const CREDENTIAL_NAME: &str = "inforno-api-keys";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("could not read or write the key file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not decrypt or encrypt the keys: {0}")]
    Seal(String),
    #[error("the decrypted key file is damaged: {0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;

// ---------------------------------------------------------------------------
// Sealing
// ---------------------------------------------------------------------------

/// Seals data to this machine and user. `name` is bound into the sealed
/// blob, so unsealing it under another name fails.
pub trait Sealer {
    /// What protects the file, in words (for settings and security reports).
    fn description(&self) -> &'static str;
    fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>>;
    fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}

/// The sealer for this platform, or None where there is none yet (the
/// caller then keeps keys elsewhere, e.g. in memory only).
pub fn platform_sealer() -> Option<Box<dyn Sealer>> {
    #[cfg(target_os = "linux")]
    return Some(Box::new(SystemdCreds));
    #[cfg(windows)]
    return Some(Box::new(Dpapi));
    #[allow(unreachable_code)]
    None
}

/// Seals with `systemd-creds --user` (systemd 256 or newer).
#[cfg(target_os = "linux")]
pub struct SystemdCreds;

#[cfg(target_os = "linux")]
impl SystemdCreds {
    fn run(&self, action: &str, name: &str, input: &[u8]) -> Result<Vec<u8>> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("systemd-creds")
            .args(["--user", action, &format!("--name={name}"), "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VaultError::Seal(format!("could not run systemd-creds: {e}")))?;
        // Write stdin from another thread: with many keys the input could
        // exceed the pipe buffer while systemd-creds is already writing.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input = input.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output()?;
        writer.join().expect("stdin writer panicked")?;
        if !output.status.success() {
            return Err(VaultError::Seal(format!(
                "systemd-creds {action} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }
}

#[cfg(target_os = "linux")]
impl Sealer for SystemdCreds {
    fn description(&self) -> &'static str {
        "systemd-creds: bound to this machine's TPM2 (or root-only host key) and this user"
    }

    fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        self.run("encrypt", name, plain)
    }

    fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        self.run("decrypt", name, sealed).map(Zeroizing::new)
    }
}

/// Seals with Windows DPAPI (`CryptProtectData`): encrypted for the
/// logged-in Windows user, without any prompt, and unreadable for other
/// users and on other machines. `name` is passed as DPAPI's optional
/// entropy, so a blob only unseals under the same name.
#[cfg(windows)]
pub struct Dpapi;

#[cfg(windows)]
impl Dpapi {
    fn blob(data: &[u8]) -> windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
        windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).expect("a key file is far below 4 GiB"),
            // DPAPI only reads input blobs; the C API just isn't const-correct.
            pbData: data.as_ptr().cast_mut(),
        }
    }

    /// Calls `CryptProtectData` (`protect`) or `CryptUnprotectData`, then
    /// copies the output, wipes DPAPI's buffer (it may hold plain text) and
    /// frees it.
    fn call(protect: bool, name: &str, data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        use secrecy::zeroize::Zeroize;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };

        let input = Self::blob(data);
        let entropy = Self::blob(name.as_bytes());
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: every pointer is valid for the duration of the call; the
        // optional description/reserved/prompt arguments are null.
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.pbData.is_null() {
            let what = if protect { "CryptProtectData" } else { "CryptUnprotectData" };
            return Err(VaultError::Seal(format!("{what} failed: {}", std::io::Error::last_os_error())));
        }
        // SAFETY: on success DPAPI returns a LocalAlloc'd buffer of cbData bytes,
        // to be released with LocalFree.
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
            let copy = Zeroizing::new(bytes.to_vec());
            bytes.zeroize();
            LocalFree(out.pbData.cast());
            Ok(copy)
        }
    }
}

#[cfg(windows)]
impl Sealer for Dpapi {
    fn description(&self) -> &'static str {
        "Windows DPAPI: bound to this Windows user account"
    }

    fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        Self::call(true, name, plain).map(|sealed| sealed.to_vec())
    }

    fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        Self::call(false, name, sealed)
    }
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

/// The API keys, decrypted in memory (wiped on drop).
#[derive(Default)]
pub struct Vault {
    keys: BTreeMap<String, SecretString>,
}

impl Vault {
    /// Reads and decrypts the key file. A missing file is an empty vault.
    pub fn load(path: &Path, sealer: &dyn Sealer) -> Result<Self> {
        let sealed = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let plain = sealer.unseal(CREDENTIAL_NAME, &sealed)?;
        let map: BTreeMap<String, String> =
            serde_json::from_slice(&plain).map_err(|e| VaultError::Format(e.to_string()))?;
        // Each String moves (buffer and all) into its SecretString, which
        // wipes it on drop.
        let keys = map.into_iter().map(|(k, v)| (k, SecretString::from(v))).collect();
        Ok(Vault { keys })
    }

    /// Encrypts and writes the key file atomically (temp file + rename),
    /// readable by the owner only. With no keys left, removes the file.
    pub fn save(&self, path: &Path, sealer: &dyn Sealer) -> Result<()> {
        if self.keys.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let exposed: BTreeMap<&str, &str> = self
            .keys
            .iter()
            .map(|(k, v)| {
                let value: &str = v.expose_secret(); // &str or &String, depending on the secrecy version
                (k.as_str(), value)
            })
            .collect();
        let plain = Zeroizing::new(serde_json::to_vec(&exposed).expect("a string map always serializes"));
        let sealed = sealer.seal(CREDENTIAL_NAME, &plain)?;

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        write_private(&tmp, &sealed)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn get(&self, provider: &str) -> Option<&SecretString> {
        self.keys.get(provider)
    }

    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    pub fn set(&mut self, provider: &str, key: SecretString) {
        self.keys.insert(provider.to_string(), key);
    }

    pub fn remove(&mut self, provider: &str) -> bool {
        self.keys.remove(provider).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for systemd-creds: scrambles the data with a per-machine
    /// byte and records the machine and name, which unsealing checks.
    struct FakeSealer {
        machine: u8,
    }

    impl Sealer for FakeSealer {
        fn description(&self) -> &'static str {
            "test"
        }
        fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
            let mut out = format!("{}:{name}:", self.machine).into_bytes();
            out.extend(plain.iter().map(|b| b ^ self.machine));
            Ok(out)
        }
        fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
            let header = format!("{}:{name}:", self.machine).into_bytes();
            let body = sealed.strip_prefix(header.as_slice()).ok_or_else(|| VaultError::Seal("cannot unseal".into()))?;
            Ok(Zeroizing::new(body.iter().map(|b| b ^ self.machine).collect()))
        }
    }

    const THIS_MACHINE: FakeSealer = FakeSealer { machine: 0x5a };
    const OTHER_MACHINE: FakeSealer = FakeSealer { machine: 0x33 };

    fn sample() -> Vault {
        let mut v = Vault::default();
        v.set("openrouter", SecretString::from("sk-or-v1-abc".to_string()));
        v.set("anthropic", SecretString::from("sk-ant-xyz".to_string()));
        v
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.cred");
        sample().save(&path, &THIS_MACHINE).unwrap();

        let v = Vault::load(&path, &THIS_MACHINE).unwrap();
        assert_eq!(v.get("openrouter").unwrap().expose_secret(), "sk-or-v1-abc");
        assert_eq!(v.providers().collect::<Vec<_>>(), ["anthropic", "openrouter"]);
    }

    #[test]
    fn missing_file_is_empty_and_emptying_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/keys.cred"); // parent created on save
        assert!(Vault::load(&path, &THIS_MACHINE).unwrap().is_empty());

        sample().save(&path, &THIS_MACHINE).unwrap();
        let mut v = Vault::load(&path, &THIS_MACHINE).unwrap();
        v.remove("openrouter");
        v.remove("anthropic");
        v.save(&path, &THIS_MACHINE).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn other_machine_cannot_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.cred");
        sample().save(&path, &THIS_MACHINE).unwrap();
        assert!(matches!(Vault::load(&path, &OTHER_MACHINE), Err(VaultError::Seal(_))));
    }

    #[test]
    fn file_is_private_and_holds_no_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.cred");
        sample().save(&path, &THIS_MACHINE).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("sk-or-v1-abc") && !text.contains("openrouter"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    /// Uses the real systemd-creds (systemd 256+ with `--user`).
    /// Run with: cargo test --release -p inforno_core vault -- --include-ignored
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn real_systemd_creds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.cred");
        sample().save(&path, &SystemdCreds).unwrap();

        let v = Vault::load(&path, &SystemdCreds).unwrap();
        assert_eq!(v.get("anthropic").unwrap().expose_secret(), "sk-ant-xyz");

        // No plain text on disk, and a damaged file is rejected
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sk-ant-xyz"));
        let mid = bytes.len() / 2;
        bytes[mid] = if bytes[mid] == b'A' { b'B' } else { b'A' };
        std::fs::write(&path, &bytes).unwrap();
        assert!(Vault::load(&path, &SystemdCreds).is_err());
    }

    /// Uses the real DPAPI. Run on Windows with:
    /// cargo test --release -p inforno_core vault -- --include-ignored
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn real_dpapi() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.cred");
        sample().save(&path, &Dpapi).unwrap();

        let v = Vault::load(&path, &Dpapi).unwrap();
        assert_eq!(v.get("anthropic").unwrap().expose_secret(), "sk-ant-xyz");

        let mut bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sk-ant-xyz"));
        let mid = bytes.len() / 2;
        bytes[mid] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(Vault::load(&path, &Dpapi).is_err());

        // Bound to the name, like systemd-creds
        let sealed = Dpapi.seal("one-name", b"data").unwrap();
        assert!(Dpapi.unseal("another-name", &sealed).is_err());
    }
}
