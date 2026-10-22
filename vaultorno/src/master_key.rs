//! The daemon's master key (design doc §6). `vaultorno init` seals it into
//! `master_key.cred` with `systemd-creds`; at every start of
//! `vaultorno.service`, systemd unseals it into
//! `$CREDENTIALS_DIRECTORY/master_key` (`LoadCredentialEncrypted=`), and the
//! daemon reads it from there. The daemon derives every file's outer key
//! from it (`store.rs`); the sweep never needs it.
//!
//! The credential holds the 32 key bytes, then how `init` sealed them
//! (`host` or `host+tpm2`). Only someone who can seal credentials on this
//! machine, which is root, can write that label, so `Welcome.sealer` reports
//! it as a fact. Tests and development daemons point
//! `$CREDENTIALS_DIRECTORY` at a folder holding an unsealed key labelled
//! `unsealed`; nothing else about them differs.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use chacha20poly1305::aead::KeyInit;
use chacha20poly1305::{Key, XChaCha20Poly1305};
use hkdf::Hkdf;
use rustix::fs::{Mode, OFlags, open};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Length of the master key.
pub const KEY_LEN: usize = 32;
/// The credential's name: bound into it by `systemd-creds --name=`, and the
/// name `LoadCredentialEncrypted=` gives it (§14).
pub const CREDENTIAL_NAME: &str = "master_key";
/// The sealed credential, in the state folder.
pub const CREDENTIAL_FILE: &str = "master_key.cred";
/// The variable systemd sets to the folder of the service's unsealed
/// credentials.
pub const CREDENTIALS_DIRECTORY: &str = "CREDENTIALS_DIRECTORY";

/// HKDF info prefix of the per-file keys (§6).
const FILE_INFO: &str = "vaultorno/v1";
/// Longest credential plaintext read: the key and a label.
const MAX_CREDENTIAL: u64 = (KEY_LEN + 16) as u64;

/// How the master key is sealed at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sealing {
    /// `systemd-creds` with the host key (`/var/lib/systemd/credential.secret`).
    Host,
    /// `systemd-creds` with the TPM2 and the host key.
    HostAndTpm2,
    /// Not sealed: a plain file, for tests and development daemons.
    Unsealed,
}

const SEALINGS: [Sealing; 3] = [Sealing::Host, Sealing::HostAndTpm2, Sealing::Unsealed];

impl Sealing {
    /// The label after the key in the credential; for the sealed kinds, also
    /// the value of `systemd-creds encrypt --with-key=`.
    pub fn label(self) -> &'static str {
        match self {
            Sealing::Host => "host",
            Sealing::HostAndTpm2 => "host+tpm2",
            Sealing::Unsealed => "unsealed",
        }
    }

    /// What `Welcome.sealer` reports (§9).
    pub fn describe(self) -> &'static str {
        match self {
            Sealing::Host => "systemd-creds, host key",
            Sealing::HostAndTpm2 => "systemd-creds, TPM2 and host key",
            Sealing::Unsealed => "unsealed key file (development only)",
        }
    }

    fn from_label(label: &[u8]) -> Option<Sealing> {
        SEALINGS.into_iter().find(|s| s.label().as_bytes() == label)
    }
}

/// The daemon's master key. Never logged, never sent anywhere.
pub struct MasterKey {
    key: Zeroizing<[u8; KEY_LEN]>,
    sealing: Sealing,
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasterKey")
            .field("key", &"<redacted>")
            .field("sealing", &self.sealing)
            .finish()
    }
}

impl MasterKey {
    /// A master key from its bytes. Tests pass one straight to `Store::open`.
    pub fn new(key: &[u8; KEY_LEN], sealing: Sealing) -> MasterKey {
        MasterKey {
            key: Zeroizing::new(*key),
            sealing,
        }
    }

    /// A new random master key, for `vaultorno init`.
    pub fn generate(sealing: Sealing) -> Result<MasterKey, String> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(&mut key[..]).map_err(|e| format!("no randomness: {e}"))?;
        Ok(MasterKey { key, sealing })
    }

    pub fn sealing(&self) -> Sealing {
        self.sealing
    }

    /// The credential's plaintext: the key, then the sealing's label.
    pub fn to_credential(&self) -> Zeroizing<Vec<u8>> {
        let label = self.sealing.label().as_bytes();
        let mut out = Zeroizing::new(Vec::with_capacity(KEY_LEN + label.len()));
        out.extend_from_slice(&self.key[..]);
        out.extend_from_slice(label);
        out
    }

    /// Reads a credential's plaintext, as [`MasterKey::to_credential`] makes it.
    pub fn from_credential(bytes: &[u8]) -> Result<MasterKey, String> {
        if bytes.len() <= KEY_LEN {
            return Err(format!(
                "the master key credential is {} bytes; it should be {KEY_LEN} bytes and a label",
                bytes.len()
            ));
        }
        let (key, label) = bytes.split_at(KEY_LEN);
        let sealing = Sealing::from_label(label).ok_or_else(|| {
            "the master key credential doesn't end in a known label (host, host+tpm2 or \
             unsealed); make it with `vaultorno init`"
                .to_owned()
        })?;
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        out.copy_from_slice(key);
        Ok(MasterKey { key: out, sealing })
    }

    /// The unsealed key systemd placed in `$CREDENTIALS_DIRECTORY/master_key`
    /// (§6). Without it, the daemon refuses to start.
    pub fn from_credentials_directory() -> Result<MasterKey, String> {
        let dir = std::env::var_os(CREDENTIALS_DIRECTORY)
            .filter(|d| !d.is_empty())
            .ok_or_else(|| {
                format!(
                    "no master key: ${CREDENTIALS_DIRECTORY} isn't set. vaultorno.service has \
                     systemd unseal it there (LoadCredentialEncrypted={CREDENTIAL_NAME}:\
                     /var/lib/vaultorno/{CREDENTIAL_FILE}); a development daemon needs a \
                     folder holding an unsealed {CREDENTIAL_NAME} file"
                )
            })?;
        Self::read_file(&Path::new(&dir).join(CREDENTIAL_NAME))
    }

    /// Reads an unsealed credential file: a regular file, never through a
    /// symlink, that others can't read.
    pub fn read_file(path: &Path) -> Result<MasterKey, String> {
        let fail = |what: String| format!("master key {}: {what}", path.display());
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| fail(format!("can't open: {e}")))?;
        let file = File::from(fd);
        let meta = file
            .metadata()
            .map_err(|e| fail(format!("can't stat: {e}")))?;
        if !meta.file_type().is_file() {
            return Err(fail("isn't a regular file".into()));
        }
        if meta.mode() & 0o007 != 0 {
            return Err(fail(format!(
                "has mode {:o}; others must have no access",
                meta.mode() & 0o777
            )));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_CREDENTIAL as usize + 1));
        file.take(MAX_CREDENTIAL + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| fail(format!("can't read: {e}")))?;
        if bytes.len() as u64 > MAX_CREDENTIAL {
            return Err(fail("is too large".into()));
        }
        Self::from_credential(&bytes).map_err(fail)
    }

    /// The cipher for one file of the state (§6): HKDF-SHA256 over the master
    /// key, info `vaultorno/v1/<uid>/<vault>/<name>`, where `name` is the
    /// file's path in the vault (`daemon_half`, `lockers/<locker>`).
    pub(crate) fn file_cipher(&self, uid: u32, vault: &str, name: &str) -> XChaCha20Poly1305 {
        let info = format!("{FILE_INFO}/{uid}/{vault}/{name}");
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, &self.key[..])
            .expand(info.as_bytes(), &mut key[..])
            .expect("32 bytes is a valid HKDF output length");
        XChaCha20Poly1305::new(Key::from_slice(&key[..]))
    }
}
