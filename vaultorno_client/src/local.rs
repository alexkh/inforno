//! The `Local` backend (design doc §3): used when the daemon isn't
//! installed. Each locker is one file, sealed per user by the OS
//! (`systemd-creds --user`; DPAPI on Windows, later). Any program running as
//! the user can open these files, so there is no per-app protection, and the
//! security report says so. Semantics match the daemon: lockers named by the
//! statement, revisions, compare-and-swap, expiry, limits.
//!
//! File layout, `<dir>/local_<app>/<locker>.cred`:
//!
//! | Bytes | Field |
//! |---|---|
//! | 4 | magic `VLTL` |
//! | 1 | layout version, `1` |
//! | 8 | revision, big-endian |
//! | 8 | expiry, Unix seconds, big-endian |
//! | rest | the OS sealer's blob of `header ‖ data` |
//!
//! The header is readable without unsealing, and checked against the sealed
//! copy after.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use secrecy::SecretBox;
use zeroize::Zeroizing;

use crate::Error;
use crate::files::{ensure_private_dir, lock_file, new_revision, random_bytes, write_atomic};
use crate::names::is_valid_name;
use crate::protocol::{Grant, LockerInfo, MAX_DATA};
use crate::time::Clock;
use crate::vault::{Locker, Stored};

const MAGIC: [u8; 4] = *b"VLTL";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 21;

/// Seals data so that only this user, on this machine, can unseal it.
pub trait OsSealer: Send + Sync {
    /// For `describe()`.
    fn name(&self) -> &str;
    /// Seals `plain`, bound to `label`.
    fn seal(&self, label: &str, plain: &[u8]) -> Result<Vec<u8>, Error>;
    /// Unseals a blob made by [`OsSealer::seal`] with the same `label`.
    fn unseal(&self, label: &str, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error>;
}

/// This platform's sealer for the `Local` backend: `systemd-creds --user` on
/// Linux, DPAPI on Windows.
pub fn default_sealer() -> Arc<dyn OsSealer> {
    #[cfg(target_os = "linux")]
    let sealer = Arc::new(SystemdCredsUser);
    #[cfg(windows)]
    let sealer = Arc::new(Dpapi);
    #[cfg(not(any(target_os = "linux", windows)))]
    let sealer = Arc::new(Unsupported);
    sealer
}

/// `systemd-creds --user`, run by absolute path with an empty environment,
/// because the app's own environment is attacker-controlled (§13).
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemdCredsUser;

#[cfg(target_os = "linux")]
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";

#[cfg(target_os = "linux")]
impl SystemdCredsUser {
    fn run(&self, action: &str, label: &str, input: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::thread;

        let mut child = Command::new(SYSTEMD_CREDS)
            .env_clear()
            .args(["--user", &format!("--name={label}"), action, "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Sealer(format!("can't run {SYSTEMD_CREDS}: {e}")))?;
        let mut stdin = child.stdin.take().expect("piped");
        let input = Zeroizing::new(input.to_vec());
        let writer = thread::spawn(move || stdin.write_all(&input));
        let out = child.wait_with_output()?;
        let written = writer
            .join()
            .map_err(|_| Error::Sealer("writer thread panicked".into()))?;
        if !out.status.success() {
            return Err(Error::Sealer(format!(
                "systemd-creds {action} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        written?;
        Ok(Zeroizing::new(out.stdout))
    }
}

#[cfg(target_os = "linux")]
impl OsSealer for SystemdCredsUser {
    fn name(&self) -> &str {
        "systemd-creds --user"
    }

    fn seal(&self, label: &str, plain: &[u8]) -> Result<Vec<u8>, Error> {
        Ok(self.run("encrypt", label, plain)?.to_vec())
    }

    fn unseal(&self, label: &str, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.run("decrypt", label, blob)
    }
}

/// Windows DPAPI (`CryptProtectData`): bound to this Windows user account,
/// without a prompt. The label is DPAPI's optional entropy, so a blob only
/// unseals under the same label.
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Dpapi;

#[cfg(windows)]
impl Dpapi {
    /// Calls `CryptProtectData` (`protect`) or `CryptUnprotectData`, copies
    /// the output, wipes DPAPI's buffer (it may hold plain text) and frees it.
    #[allow(unsafe_code)]
    fn call(protect: bool, label: &str, data: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        };
        use zeroize::Zeroize;

        fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, Error> {
            Ok(CRYPT_INTEGER_BLOB {
                cbData: u32::try_from(data.len())
                    .map_err(|_| Error::Sealer("too large for DPAPI".into()))?,
                // DPAPI only reads input blobs; the C API just isn't const-correct.
                pbData: data.as_ptr().cast_mut(),
            })
        }

        let input = blob(data)?;
        let entropy = blob(label.as_bytes())?;
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: every pointer is valid for the duration of the call; the
        // optional description, reserved and prompt arguments are null.
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
        };
        if ok == 0 || out.pbData.is_null() {
            let what = if protect {
                "CryptProtectData"
            } else {
                "CryptUnprotectData"
            };
            return Err(Error::Sealer(format!(
                "{what} failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: on success DPAPI returns a LocalAlloc'd buffer of cbData
        // bytes, to be released with LocalFree.
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
impl OsSealer for Dpapi {
    fn name(&self) -> &str {
        "Windows DPAPI"
    }

    fn seal(&self, label: &str, plain: &[u8]) -> Result<Vec<u8>, Error> {
        Ok(Self::call(true, label, plain)?.to_vec())
    }

    fn unseal(&self, label: &str, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        Self::call(false, label, blob)
    }
}

/// Where no OS sealer exists yet (macOS: the Keychain, Milestone 8).
#[cfg(not(any(target_os = "linux", windows)))]
struct Unsupported;

#[cfg(not(any(target_os = "linux", windows)))]
impl OsSealer for Unsupported {
    fn name(&self) -> &str {
        "none on this platform"
    }

    fn seal(&self, _label: &str, _plain: &[u8]) -> Result<Vec<u8>, Error> {
        Err(Error::Sealer("no OS sealer on this platform yet".into()))
    }

    fn unseal(&self, _label: &str, _blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        Err(Error::Sealer("no OS sealer on this platform yet".into()))
    }
}

/// A sealer for tests: XChaCha20-Poly1305 under a key that lives only in
/// this object.
pub struct FakeSealer {
    key: Zeroizing<[u8; 32]>,
}

impl FakeSealer {
    pub fn new() -> FakeSealer {
        FakeSealer {
            key: Zeroizing::new(random_bytes::<32>().expect("randomness")),
        }
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(&self.key[..]))
    }
}

impl Default for FakeSealer {
    fn default() -> Self {
        FakeSealer::new()
    }
}

impl OsSealer for FakeSealer {
    fn name(&self) -> &str {
        "fake sealer (tests only)"
    }

    fn seal(&self, label: &str, plain: &[u8]) -> Result<Vec<u8>, Error> {
        let nonce = random_bytes::<24>()?;
        let sealed = self
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: label.as_bytes(),
                },
            )
            .map_err(|_| Error::Corrupt)?;
        Ok([&nonce[..], &sealed].concat())
    }

    fn unseal(&self, label: &str, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        if blob.len() < 24 + 16 {
            return Err(Error::Corrupt);
        }
        let plain = self
            .cipher()
            .decrypt(
                XNonce::from_slice(&blob[..24]),
                Payload {
                    msg: &blob[24..],
                    aad: label.as_bytes(),
                },
            )
            .map_err(|_| Error::Corrupt)?;
        Ok(Zeroizing::new(plain))
    }
}

/// One app's lockers in the `Local` backend.
pub(crate) struct LocalStore {
    dir: PathBuf,
    app: String,
    sealer: Arc<dyn OsSealer>,
    clock: Arc<dyn Clock>,
    lockers: BTreeMap<String, Grant>,
}

struct Header {
    revision: u64,
    expires: u64,
}

impl Header {
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[..4].copy_from_slice(&MAGIC);
        h[4] = VERSION;
        h[5..13].copy_from_slice(&self.revision.to_be_bytes());
        h[13..21].copy_from_slice(&self.expires.to_be_bytes());
        h
    }

    fn decode(bytes: &[u8]) -> Result<Header, Error> {
        if bytes.len() < HEADER_LEN || bytes[..4] != MAGIC || bytes[4] != VERSION {
            return Err(Error::Corrupt);
        }
        Ok(Header {
            revision: u64::from_be_bytes(bytes[5..13].try_into().expect("8 bytes")),
            expires: u64::from_be_bytes(bytes[13..21].try_into().expect("8 bytes")),
        })
    }
}

impl LocalStore {
    /// `base` is usually `~/.local/share/vaultorno`.
    pub(crate) fn open(
        base: &Path,
        app: &str,
        lockers: BTreeMap<String, Grant>,
        sealer: Arc<dyn OsSealer>,
        clock: Arc<dyn Clock>,
    ) -> Result<LocalStore, Error> {
        ensure_private_dir(base)?;
        let dir = base.join(format!("local_{app}"));
        ensure_private_dir(&dir)?;
        let store = LocalStore {
            dir,
            app: app.to_owned(),
            sealer,
            clock,
            lockers,
        };
        store.sweep()?;
        Ok(store)
    }

    pub(crate) fn sealer_name(&self) -> &str {
        self.sealer.name()
    }

    fn grant(&self, locker: &str) -> Result<Grant, Error> {
        if !is_valid_name(locker) {
            return Err(Error::InvalidName(locker.to_owned()));
        }
        self.lockers.get(locker).copied().ok_or_else(|| {
            Error::UnknownLocker(format!("{locker} isn't in this binary's statement"))
        })
    }

    fn file_name(locker: &str) -> String {
        format!("{locker}.cred")
    }

    fn label(&self, locker: &str) -> String {
        format!("vaultorno_{}_{locker}", self.app)
    }

    /// Serialises writers across processes.
    fn lock(&self) -> Result<File, Error> {
        Ok(lock_file(&self.dir.join("lock"))?)
    }

    /// Reads a locker file, deleting it if it has expired.
    fn read(&self, locker: &str) -> Result<Option<(Header, Vec<u8>)>, Error> {
        let path = self.dir.join(Self::file_name(locker));
        let mut bytes = Vec::new();
        match File::open(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(mut f) => {
                f.read_to_end(&mut bytes)?;
            }
        }
        let header = Header::decode(&bytes)?;
        if header.expires != 0 && self.clock.now() >= header.expires {
            fs::remove_file(&path)?;
            return Ok(None);
        }
        Ok(Some((header, bytes)))
    }

    pub(crate) fn get(&self, locker: &str) -> Result<Locker, Error> {
        self.grant(locker)?;
        let _lock = self.lock()?;
        let Some((header, bytes)) = self.read(locker)? else {
            return Ok(Locker::empty());
        };
        let plain = self
            .sealer
            .unseal(&self.label(locker), &bytes[HEADER_LEN..])?;
        if plain.len() < HEADER_LEN || plain[..HEADER_LEN] != bytes[..HEADER_LEN] {
            return Err(Error::Corrupt);
        }
        Ok(Locker {
            data: SecretBox::new(plain[HEADER_LEN..].to_vec().into_boxed_slice()),
            revision: header.revision,
            expires: Some(header.expires),
        })
    }

    pub(crate) fn put(&self, locker: &str, data: &[u8], expected: u64) -> Result<Stored, Error> {
        let grant = self.grant(locker)?;
        if !grant.write {
            return Err(Error::NotAllowed(format!(
                "{locker} is read-only for this binary"
            )));
        }
        if data.len() > MAX_DATA {
            return Err(Error::TooLarge(format!(
                "{} bytes; a locker holds at most {MAX_DATA}",
                data.len()
            )));
        }
        let _lock = self.lock()?;
        let current = self.read(locker)?;
        let current_revision = current.as_ref().map_or(0, |(h, _)| h.revision);
        if current_revision != expected {
            return Err(Error::Conflict);
        }
        if data.is_empty() {
            if current.is_some() {
                fs::remove_file(self.dir.join(Self::file_name(locker)))?;
            }
            return Ok(Stored {
                revision: 0,
                expires: None,
            });
        }
        let expires = match &current {
            Some((h, _)) => h.expires,
            None => self.clock.now() + grant.lifetime_secs,
        };
        let header = Header {
            revision: new_revision(current_revision)?,
            expires,
        }
        .encode();
        let mut plain = Zeroizing::new(Vec::with_capacity(HEADER_LEN + data.len()));
        plain.extend_from_slice(&header);
        plain.extend_from_slice(data);
        let blob = self.sealer.seal(&self.label(locker), &plain)?;
        write_atomic(
            &self.dir,
            &Self::file_name(locker),
            &[&header[..], &blob].concat(),
        )?;
        Ok(Stored {
            revision: u64::from_be_bytes(header[5..13].try_into().expect("8 bytes")),
            expires: Some(expires),
        })
    }

    pub(crate) fn list(&self) -> Result<Vec<LockerInfo>, Error> {
        let _lock = self.lock()?;
        let mut out = Vec::new();
        for (name, grant) in &self.lockers {
            let (revision, size, expires) = match self.read(name)? {
                Some((h, bytes)) => (h.revision, bytes.len() as u32, Some(h.expires)),
                None => (0, 0, None),
            };
            out.push(LockerInfo {
                name: name.clone(),
                revision,
                size,
                expires,
                read: true,
                write: grant.write,
            });
        }
        Ok(out)
    }

    /// Deletes expired lockers and leftover temp files.
    fn sweep(&self) -> Result<(), Error> {
        let _lock = self.lock()?;
        let now = self.clock.now();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.ends_with(".tmp") {
                fs::remove_file(entry.path())?;
            } else if name.ends_with(".cred") {
                let mut head = [0u8; HEADER_LEN];
                let readable = File::open(entry.path())
                    .and_then(|mut f| f.read_exact(&mut head))
                    .is_ok();
                if let (true, Ok(h)) = (readable, Header::decode(&head)) {
                    if h.expires != 0 && now >= h.expires {
                        fs::remove_file(entry.path())?;
                    }
                }
            }
        }
        Ok(())
    }
}
