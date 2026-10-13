//! Encrypted on-disk store for API keys (the "vault").
//!
//! The secrets (provider name → API key) are encrypted with a random 32-byte
//! *data key*. Each way of unlocking is a *slot* holding its own encrypted
//! copy of that data key:
//!
//! - a sealed slot: the data key sealed by the machine, by default with
//!   `systemd-creds` (TPM2 when the machine has one, otherwise the root-only
//!   host key). Unlocks without any user interaction, but only on this
//!   machine and for this user;
//! - a passphrase slot: the data key wrapped with a key derived from a
//!   passphrase by Argon2id. For recovery (a passphrase kept on paper),
//!   e.g. after a TPM reset, a motherboard swap or a move to a new machine.
//!
//! Any slot unlocks the vault; slots can be added and removed without
//! re-encrypting the secrets. The sealing mechanism is behind the [`Sealer`]
//! trait; [`SystemdCreds`] is the real one.
//!
//! Encryption is XChaCha20-Poly1305 with random nonces. The file is JSON;
//! nothing in it is readable without one of the slots.

use std::collections::BTreeMap;
use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};

const VERSION: u32 = 1;
/// Associated data: binds every ciphertext to this format and its role.
const AAD_SECRETS: &[u8] = b"inforno-vault-v1 secrets";
const AAD_SLOT: &[u8] = b"inforno-vault-v1 slot";

/// Argon2id cost for passphrase slots: 64 MiB, 3 passes (~0.3–0.5 s on a
/// desktop). Stored per slot, so it can be raised later without breaking
/// existing vaults.
const ARGON2_M_KIB: u32 = 64 * 1024;
const ARGON2_T: u32 = 3;
const ARGON2_P: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("could not read or write the vault file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the vault file is damaged or not a vault: {0}")]
    Format(String),
    #[error("unsupported vault version {0}")]
    Version(u32),
    #[error("wrong passphrase, or the vault was tampered with")]
    WrongKey,
    #[error("sealing or unsealing failed: {0}")]
    Seal(String),
    #[error("no such slot")]
    NoSuchSlot,
    #[error("the last slot can't be removed: the vault could never be opened again")]
    LastSlot,
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;

// ---------------------------------------------------------------------------
// Sealing
// ---------------------------------------------------------------------------

/// Seals small secrets to this machine (and user), so only it can unseal
/// them. `name` is bound into the sealed blob; unsealing with another name
/// fails, so a blob can't be passed off as a different credential.
pub trait Sealer {
    /// Identifies the mechanism in the vault file, e.g. "systemd-creds".
    fn method(&self) -> &'static str;
    fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>>;
    fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}

/// Seals with `systemd-creds --user` (systemd 256 or newer): bound to the
/// TPM2 when the machine has one, and to the calling user. Runs the
/// `systemd-creds` command, which talks to systemd's credential service.
pub struct SystemdCreds;

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
        // A few hundred bytes at most: fits the pipe, no deadlock risk.
        child.stdin.take().expect("stdin is piped").write_all(input)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(VaultError::Seal(format!(
                "systemd-creds {action} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }
}

impl Sealer for SystemdCreds {
    fn method(&self) -> &'static str {
        "systemd-creds"
    }

    fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        self.run("encrypt", name, plain)
    }

    fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        self.run("decrypt", name, sealed).map(Zeroizing::new)
    }
}

// ---------------------------------------------------------------------------
// File format
// ---------------------------------------------------------------------------

/// A vault as stored on disk. Load it with [`VaultFile::load`], then unlock
/// it with one of its slots.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VaultFile {
    version: u32,
    slots: Vec<Slot>,
    /// Hex: the secrets map (JSON), encrypted with the data key.
    nonce: String,
    ciphertext: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Slot {
    /// Shown to the user, e.g. "This computer" or "Paper backup".
    pub label: String,
    pub kind: SlotKind,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlotKind {
    /// The data key (hex) sealed by `method` under the credential `name`.
    Sealed {
        method: String,
        name: String,
        #[serde(with = "hex_bytes")]
        blob: Vec<u8>,
    },
    /// The data key encrypted with an Argon2id-derived key.
    Passphrase {
        #[serde(with = "hex_array")]
        salt: [u8; 16],
        m_kib: u32,
        t: u32,
        p: u32,
        nonce: String,
        wrapped_key: String,
    },
}

impl VaultFile {
    pub fn load(path: &Path) -> Result<Self> {
        let file: VaultFile = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|e| VaultError::Format(e.to_string()))?;
        if file.version != VERSION {
            return Err(VaultError::Version(file.version));
        }
        Ok(file)
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn has_sealed_slot(&self, sealer: &dyn Sealer) -> bool {
        self.slots.iter().any(|s| matches!(&s.kind, SlotKind::Sealed { method, .. } if method == sealer.method()))
    }

    /// Unlocks with this machine's sealed slot, without user interaction.
    pub fn unlock_sealed(&self, sealer: &dyn Sealer) -> Result<Vault> {
        let mut last_error = VaultError::NoSuchSlot;
        for slot in &self.slots {
            let SlotKind::Sealed { method, name, blob } = &slot.kind else { continue };
            if method != sealer.method() {
                continue;
            }
            // A vault copied from another machine has a slot this one can't
            // unseal: try the next one.
            match sealer.unseal(name, blob).and_then(|hex_key| parse_data_key(&hex_key)) {
                Ok(data_key) => return self.open(data_key),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    /// Tries every passphrase slot. Slow by design (Argon2id per slot): call
    /// it off the UI thread.
    pub fn unlock_with_passphrase(&self, passphrase: &str) -> Result<Vault> {
        for slot in &self.slots {
            let SlotKind::Passphrase { salt, m_kib, t, p, nonce, wrapped_key } = &slot.kind else { continue };
            let key = passphrase_key(passphrase, salt, *m_kib, *t, *p)?;
            match decrypt(&key, nonce, wrapped_key, AAD_SLOT) {
                Ok(raw) => {
                    let data_key = raw.as_slice().try_into().map_err(|_| VaultError::Format("data key length".into()))?;
                    return self.open(Zeroizing::new(data_key));
                }
                Err(VaultError::WrongKey) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(VaultError::WrongKey)
    }

    fn open(&self, data_key: Zeroizing<[u8; 32]>) -> Result<Vault> {
        let plain = decrypt(&data_key, &self.nonce, &self.ciphertext, AAD_SECRETS)?;
        let secrets = parse_secrets(&plain)?;
        Ok(Vault { file: self.clone(), data_key, secrets })
    }
}

// ---------------------------------------------------------------------------
// Unlocked vault
// ---------------------------------------------------------------------------

/// An unlocked vault: the secrets in memory, plus what's needed to save
/// changes. The data key and secrets are wiped from memory on drop.
pub struct Vault {
    file: VaultFile,
    data_key: Zeroizing<[u8; 32]>,
    secrets: BTreeMap<String, SecretString>,
}

impl Vault {
    /// A new, empty vault. It needs at least one slot before [`Vault::save`].
    pub fn create() -> Self {
        Vault {
            file: VaultFile { version: VERSION, slots: Vec::new(), nonce: String::new(), ciphertext: String::new() },
            data_key: Zeroizing::new(random()),
            secrets: BTreeMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&SecretString> {
        self.secrets.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.secrets.keys().map(String::as_str)
    }

    pub fn set(&mut self, name: &str, value: SecretString) {
        self.secrets.insert(name.to_string(), value);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.secrets.remove(name).is_some()
    }

    pub fn slots(&self) -> &[Slot] {
        &self.file.slots
    }

    /// Adds a slot sealed to this machine. `credential_name` is bound into
    /// the sealed blob (e.g. "inforno-vault").
    pub fn add_sealed_slot(&mut self, label: &str, sealer: &dyn Sealer, credential_name: &str) -> Result<()> {
        let hex_key = Zeroizing::new(hex::encode(self.data_key.as_slice()));
        let blob = sealer.seal(credential_name, hex_key.as_bytes())?;
        self.file.slots.push(Slot {
            label: label.to_string(),
            kind: SlotKind::Sealed { method: sealer.method().to_string(), name: credential_name.to_string(), blob },
        });
        Ok(())
    }

    /// Adds a passphrase slot. Slow by design (Argon2id).
    pub fn add_passphrase_slot(&mut self, label: &str, passphrase: &str) -> Result<()> {
        let salt: [u8; 16] = random();
        let key = passphrase_key(passphrase, &salt, ARGON2_M_KIB, ARGON2_T, ARGON2_P)?;
        let (nonce, wrapped_key) = encrypt(&key, self.data_key.as_slice(), AAD_SLOT);
        self.file.slots.push(Slot {
            label: label.to_string(),
            kind: SlotKind::Passphrase { salt, m_kib: ARGON2_M_KIB, t: ARGON2_T, p: ARGON2_P, nonce, wrapped_key },
        });
        Ok(())
    }

    pub fn remove_slot(&mut self, index: usize) -> Result<()> {
        if index >= self.file.slots.len() {
            return Err(VaultError::NoSuchSlot);
        }
        if self.file.slots.len() == 1 {
            return Err(VaultError::LastSlot);
        }
        self.file.slots.remove(index);
        Ok(())
    }

    /// Re-encrypts the secrets and writes the vault atomically (temp file +
    /// rename), readable by the owner only on Unix.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        if self.file.slots.is_empty() {
            return Err(VaultError::LastSlot);
        }
        let plain = Zeroizing::new(serialize_secrets(&self.secrets));
        let (nonce, ciphertext) = encrypt(&self.data_key, &plain, AAD_SECRETS);
        self.file.nonce = nonce;
        self.file.ciphertext = ciphertext;

        let json = serde_json::to_string_pretty(&self.file).map_err(|e| VaultError::Format(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        write_private(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

/// The data key is sealed as hex text, so the sealing tool only ever handles
/// printable ASCII (no binary or newline quirks).
fn parse_data_key(hex_key: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    let text = std::str::from_utf8(hex_key).map_err(|_| VaultError::Format("sealed key is not text".into()))?;
    hex::decode_to_slice(text.trim(), key.as_mut_slice())
        .map_err(|_| VaultError::Format("sealed key is not a 32-byte hex key".into()))?;
    Ok(key)
}

fn passphrase_key(passphrase: &str, salt: &[u8; 16], m_kib: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; 32]>> {
    let params = argon2::Params::new(m_kib, t, p, Some(32)).map_err(|e| VaultError::Kdf(e.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut_slice())
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    Ok(key)
}

/// Returns (nonce, ciphertext) as hex.
fn encrypt(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> (String, String) {
    let nonce: [u8; 24] = random();
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory data");
    (hex::encode(nonce), hex::encode(ciphertext))
}

fn decrypt(key: &[u8; 32], nonce_hex: &str, ciphertext_hex: &str, aad: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut nonce = [0u8; 24];
    hex::decode_to_slice(nonce_hex, &mut nonce).map_err(|_| VaultError::Format("nonce".into()))?;
    let ciphertext = hex::decode(ciphertext_hex).map_err(|e| VaultError::Format(e.to_string()))?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad })
        .map(Zeroizing::new)
        .map_err(|_| VaultError::WrongKey)
}

fn serialize_secrets(secrets: &BTreeMap<String, SecretString>) -> Vec<u8> {
    let exposed: BTreeMap<&str, &str> = secrets
        .iter()
        .map(|(k, v)| {
            let value: &str = v.expose_secret(); // &str or &String, depending on the secrecy version
            (k.as_str(), value)
        })
        .collect();
    serde_json::to_vec(&exposed).expect("a string map always serializes")
}

fn parse_secrets(plain: &[u8]) -> Result<BTreeMap<String, SecretString>> {
    let map: BTreeMap<String, String> =
        serde_json::from_slice(plain).map_err(|e| VaultError::Format(e.to_string()))?;
    // Each value String moves (buffer and all) into its SecretString,
    // which wipes it on drop; nothing is left behind to zeroize.
    Ok(map.into_iter().map(|(k, v)| (k, SecretString::from(v))).collect())
}

fn random<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("the OS random generator failed");
    buf
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

mod hex_bytes {
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s: String = serde::Deserialize::deserialize(d)?;
        hex::decode(s).map_err(serde::de::Error::custom)
    }
}

mod hex_array {
    pub fn serialize<S: serde::Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>, const N: usize>(d: D) -> Result<[u8; N], D::Error> {
        let s: String = serde::Deserialize::deserialize(d)?;
        hex::decode(s)
            .map_err(serde::de::Error::custom)?
            .try_into()
            .map_err(|_| serde::de::Error::custom(format!("expected {N} bytes")))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for systemd-creds: "seals" by encrypting with a fixed
    /// machine key, binding the name like the real thing does.
    struct FakeSealer {
        machine_key: [u8; 32],
    }

    impl Sealer for FakeSealer {
        fn method(&self) -> &'static str {
            "systemd-creds"
        }
        fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
            let (n, c) = encrypt(&self.machine_key, plain, name.as_bytes());
            Ok(format!("{n}:{c}").into_bytes())
        }
        fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
            let text = String::from_utf8(sealed.to_vec()).unwrap();
            let (n, c) = text.split_once(':').unwrap();
            decrypt(&self.machine_key, n, c, name.as_bytes()).map_err(|_| VaultError::Seal("cannot unseal".into()))
        }
    }

    const THIS_MACHINE: FakeSealer = FakeSealer { machine_key: [1u8; 32] };
    const OTHER_MACHINE: FakeSealer = FakeSealer { machine_key: [2u8; 32] };

    fn sample_vault() -> Vault {
        let mut v = Vault::create();
        v.set("openrouter", SecretString::from("sk-or-v1-abc".to_string()));
        v.set("anthropic", SecretString::from("sk-ant-xyz".to_string()));
        v
    }

    fn saved(v: &mut Vault) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.vault");
        v.save(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn sealed_round_trip() {
        let mut v = sample_vault();
        v.add_sealed_slot("This computer", &THIS_MACHINE, "inforno-vault").unwrap();
        let (_dir, path) = saved(&mut v);

        let file = VaultFile::load(&path).unwrap();
        assert!(file.has_sealed_slot(&THIS_MACHINE));
        let v = file.unlock_sealed(&THIS_MACHINE).unwrap();
        assert_eq!(v.get("openrouter").unwrap().expose_secret(), "sk-or-v1-abc");
        assert_eq!(v.names().collect::<Vec<_>>(), ["anthropic", "openrouter"]);
    }

    #[test]
    fn other_machine_needs_the_passphrase() {
        let mut v = sample_vault();
        v.add_sealed_slot("This computer", &THIS_MACHINE, "inforno-vault").unwrap();
        v.add_passphrase_slot("Paper backup", "correct horse battery staple").unwrap();
        let (_dir, path) = saved(&mut v);

        let file = VaultFile::load(&path).unwrap();
        assert!(matches!(file.unlock_sealed(&OTHER_MACHINE), Err(VaultError::Seal(_))));

        // Recover on the new machine: unlock with the passphrase, seal to it.
        let mut v = file.unlock_with_passphrase("correct horse battery staple").unwrap();
        v.add_sealed_slot("New computer", &OTHER_MACHINE, "inforno-vault").unwrap();
        v.save(&path).unwrap();
        let v = VaultFile::load(&path).unwrap().unlock_sealed(&OTHER_MACHINE).unwrap();
        assert_eq!(v.get("anthropic").unwrap().expose_secret(), "sk-ant-xyz");
    }

    #[test]
    fn changes_are_seen_through_every_slot() {
        let mut v = sample_vault();
        v.add_sealed_slot("This computer", &THIS_MACHINE, "inforno-vault").unwrap();
        v.add_passphrase_slot("Paper backup", "pw").unwrap();
        let (_dir, path) = saved(&mut v);

        let mut v = VaultFile::load(&path).unwrap().unlock_sealed(&THIS_MACHINE).unwrap();
        v.set("openrouter", SecretString::from("sk-or-v1-new".to_string()));
        v.remove("anthropic");
        v.save(&path).unwrap();

        let v = VaultFile::load(&path).unwrap().unlock_with_passphrase("pw").unwrap();
        assert_eq!(v.get("openrouter").unwrap().expose_secret(), "sk-or-v1-new");
        assert!(v.get("anthropic").is_none());
    }

    #[test]
    fn credential_name_is_bound() {
        let mut v = sample_vault();
        v.add_sealed_slot("This computer", &THIS_MACHINE, "inforno-vault").unwrap();
        let mut file = v.file.clone();
        if let SlotKind::Sealed { name, .. } = &mut file.slots[0].kind {
            *name = "something-else".into();
        }
        assert!(matches!(file.unlock_sealed(&THIS_MACHINE), Err(VaultError::Seal(_))));
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let mut v = sample_vault();
        v.add_passphrase_slot("Paper backup", "right").unwrap();
        let (_dir, path) = saved(&mut v);
        let file = VaultFile::load(&path).unwrap();
        assert!(matches!(file.unlock_with_passphrase("wrong"), Err(VaultError::WrongKey)));
        assert!(matches!(file.unlock_sealed(&THIS_MACHINE), Err(VaultError::NoSuchSlot)));
    }

    #[test]
    fn slot_rules() {
        let mut v = sample_vault();
        let dir = tempfile::tempdir().unwrap();
        // No slot yet: refusing to save avoids an unopenable vault.
        assert!(matches!(v.save(&dir.path().join("v")), Err(VaultError::LastSlot)));
        v.add_sealed_slot("a", &THIS_MACHINE, "inforno-vault").unwrap();
        assert!(matches!(v.remove_slot(0), Err(VaultError::LastSlot)));
        v.add_passphrase_slot("b", "pw").unwrap();
        v.remove_slot(0).unwrap();
        assert_eq!(v.slots()[0].label, "b");
        assert!(matches!(v.remove_slot(5), Err(VaultError::NoSuchSlot)));
    }

    #[test]
    fn tampered_secrets_are_rejected() {
        let mut v = sample_vault();
        v.add_sealed_slot("a", &THIS_MACHINE, "inforno-vault").unwrap();
        let (_dir, path) = saved(&mut v);

        let mut file = VaultFile::load(&path).unwrap();
        let mut ct = hex::decode(&file.ciphertext).unwrap();
        ct[0] ^= 1;
        file.ciphertext = hex::encode(ct);
        assert!(matches!(file.unlock_sealed(&THIS_MACHINE), Err(VaultError::WrongKey)));
    }

    #[test]
    fn file_is_private_and_holds_no_plaintext() {
        let mut v = sample_vault();
        v.add_sealed_slot("a", &THIS_MACHINE, "inforno-vault").unwrap();
        let (_dir, path) = saved(&mut v);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sk-or-v1-abc") && !text.contains("openrouter"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    /// Uses the real systemd-creds (systemd 256+ with `--user` support).
    /// Run with: cargo test --release -p inforno_core vault -- --ignored
    #[test]
    #[ignore]
    fn real_systemd_creds_round_trip() {
        let mut v = sample_vault();
        v.add_sealed_slot("This computer", &SystemdCreds, "inforno-vault-test").unwrap();
        let (_dir, path) = saved(&mut v);
        let v = VaultFile::load(&path).unwrap().unlock_sealed(&SystemdCreds).unwrap();
        assert_eq!(v.get("openrouter").unwrap().expose_secret(), "sk-or-v1-abc");
    }
}
