//! The daemon's state in `~vaultorno` (design doc §6):
//!
//! ```text
//! <state>/users/<uid>/<vault>/
//!     lock                 serialises the daemon's threads and the sweep
//!     daemon_half          the daemon's half of the app key
//!     lockers/<locker>     one locker: the client's envelope
//! ```
//!
//! Every `daemon_half` and locker file has a fixed layout (layout `1`):
//!
//! | Bytes | Field |
//! |---|---|
//! | 4 | magic `VLTD` |
//! | 1 | layout version, `1` |
//! | 8 | revision, big-endian (`0` for `daemon_half`) |
//! | 8 | expiry, Unix seconds, big-endian (`0` for none) |
//! | 24 | nonce, random per write |
//! | rest | XChaCha20-Poly1305 ciphertext of the contents, then the 16-byte tag |
//!
//! The key is derived from the master key for this user, vault and file
//! (`daemon_half` or `lockers/<locker>`); the associated data is the header,
//! then the same three. A file moved to another user, vault or name fails to
//! decrypt, and so does an edited revision or expiry. The header is readable
//! without any key, for `List` and the sweep. Milestone 1's layout `0` is
//! refused.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, Payload};
use rustix::fs::{FlockOperation, flock};
use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::HALF_LEN;
use zeroize::Zeroizing;

use crate::master_key::MasterKey;

pub const MAGIC: [u8; 4] = *b"VLTD";
/// The layout this build writes and reads (§6).
pub const LAYOUT: u8 = 1;
pub const HEADER_LEN: usize = 21;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
/// What a file holds beyond its header and contents: the nonce and the tag.
pub const SEAL_OVERHEAD: usize = NONCE_LEN + TAG_LEN;

const HALF_FILE: &str = "daemon_half";
const LOCK_FILE: &str = "lock";
const LOCKERS_DIR: &str = "lockers";

/// The part of a file that is readable without any key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub revision: u64,
    /// Unix seconds; 0 for none.
    pub expires: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[..4].copy_from_slice(&MAGIC);
        h[4] = LAYOUT;
        h[5..13].copy_from_slice(&self.revision.to_be_bytes());
        h[13..21].copy_from_slice(&self.expires.to_be_bytes());
        h
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Header> {
        if bytes.len() < HEADER_LEN || bytes[..4] != MAGIC {
            return Err(invalid("not a vaultorno file"));
        }
        if bytes[4] != LAYOUT {
            return Err(invalid(&format!(
                "layout {} isn't supported by this build",
                bytes[4]
            )));
        }
        Ok(Header {
            revision: u64::from_be_bytes(bytes[5..13].try_into().expect("8 bytes")),
            expires: u64::from_be_bytes(bytes[13..21].try_into().expect("8 bytes")),
        })
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.expires != 0 && now >= self.expires
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

/// Holding this means no other thread or process (the sweep) changes the
/// vault. Released on drop.
pub struct VaultLock(#[allow(dead_code)] File);

/// The daemon's state folder.
pub struct Store {
    root: PathBuf,
    /// Seals every file's contents (§6); `None` for the sweep, which reads
    /// only headers.
    key: Option<MasterKey>,
}

impl Store {
    /// Opens (and creates) the state folder, with the master key that seals
    /// its files.
    pub fn open(root: impl Into<PathBuf>, key: MasterKey) -> io::Result<Store> {
        Self::open_with(root.into(), Some(key))
    }

    /// Opens the state folder without the master key: paths, locks and
    /// headers only, which is all the sweep needs (§6). Reading or writing
    /// any file's contents fails.
    pub fn open_without_key(root: impl Into<PathBuf>) -> io::Result<Store> {
        Self::open_with(root.into(), None)
    }

    fn open_with(root: PathBuf, key: Option<MasterKey>) -> io::Result<Store> {
        mkdir_private(&root)?;
        mkdir_private(&root.join("users"))?;
        Ok(Store { root, key })
    }

    /// How the master key is sealed, for `Welcome.sealer` (§9).
    pub fn sealer(&self) -> &'static str {
        self.key
            .as_ref()
            .map_or("none: opened without the master key", |k| {
                k.sealing().describe()
            })
    }

    fn key(&self) -> io::Result<&MasterKey> {
        self.key
            .as_ref()
            .ok_or_else(|| io::Error::other("this store was opened without the master key"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn users_dir(&self) -> PathBuf {
        self.root.join("users")
    }

    pub fn user_dir(&self, uid: u32) -> PathBuf {
        self.users_dir().join(uid.to_string())
    }

    pub fn vault_dir(&self, uid: u32, vault: &str) -> PathBuf {
        self.user_dir(uid).join(vault)
    }

    pub fn lockers_dir(&self, uid: u32, vault: &str) -> PathBuf {
        self.vault_dir(uid, vault).join(LOCKERS_DIR)
    }

    pub fn locker_path(&self, uid: u32, vault: &str, locker: &str) -> PathBuf {
        self.lockers_dir(uid, vault).join(locker)
    }

    pub fn vault_exists(&self, uid: u32, vault: &str) -> bool {
        self.vault_dir(uid, vault).is_dir()
    }

    /// How many vaults `uid` has.
    pub fn vault_count(&self, uid: u32) -> io::Result<usize> {
        Ok(entries(&self.user_dir(uid))?
            .iter()
            .filter(|e| e.path().is_dir())
            .count())
    }

    /// Locks one vault, creating its folder if needed.
    pub fn lock_vault(&self, uid: u32, vault: &str) -> io::Result<VaultLock> {
        mkdir_private(&self.user_dir(uid))?;
        let dir = self.vault_dir(uid, vault);
        mkdir_private(&dir)?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(LOCK_FILE))?;
        flock(&f, FlockOperation::LockExclusive).map_err(io::Error::from)?;
        Ok(VaultLock(f))
    }

    /// The daemon's half of the vault's app key, created on first use. A
    /// half that doesn't decrypt is an error, never replaced (§6).
    pub fn daemon_half(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
    ) -> io::Result<Zeroizing<[u8; HALF_LEN]>> {
        let mut half = Zeroizing::new([0u8; HALF_LEN]);
        if let Some((_, body)) = self.read_sealed(uid, vault, HALF_FILE)? {
            if body.len() != HALF_LEN {
                return Err(invalid("daemon_half has the wrong size"));
            }
            half.copy_from_slice(&body);
            return Ok(half);
        }
        getrandom::fill(&mut half[..])
            .map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
        self.write_sealed(
            uid,
            vault,
            HALF_FILE,
            &Header {
                revision: 0,
                expires: 0,
            },
            &half[..],
        )?;
        Ok(half)
    }

    /// A locker's header and contents (the client's envelope), with the
    /// daemon's layer removed.
    pub fn read_locker(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
        locker: &str,
    ) -> io::Result<Option<(Header, Vec<u8>)>> {
        Ok(self
            .read_sealed(uid, vault, &locker_name(locker))?
            .map(|(header, mut body)| (header, std::mem::take(&mut *body))))
    }

    /// The header and stored size of a locker, without reading the rest.
    pub fn locker_header(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
        locker: &str,
    ) -> io::Result<Option<(Header, u64)>> {
        read_header(&self.locker_path(uid, vault, locker))
    }

    pub fn write_locker(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
        locker: &str,
        header: &Header,
        body: &[u8],
    ) -> io::Result<()> {
        mkdir_private(&self.lockers_dir(uid, vault))?;
        self.write_sealed(uid, vault, &locker_name(locker), header, body)
    }

    pub fn delete_locker(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
        locker: &str,
    ) -> io::Result<()> {
        match fs::remove_file(self.locker_path(uid, vault, locker)) {
            Ok(()) => sync_dir(&self.lockers_dir(uid, vault)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Names of the lockers that have a file, expired or not.
    pub fn filled_lockers(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
    ) -> io::Result<Vec<String>> {
        Ok(entries(&self.lockers_dir(uid, vault))?
            .iter()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| is_valid_name(n))
            .collect())
    }

    /// Reads `name` (`daemon_half` or `lockers/<locker>`) in a vault and
    /// removes the daemon's layer. `None` if it doesn't exist.
    fn read_sealed(
        &self,
        uid: u32,
        vault: &str,
        name: &str,
    ) -> io::Result<Option<(Header, Zeroizing<Vec<u8>>)>> {
        let key = self.key()?;
        let bytes = match fs::read(self.vault_dir(uid, vault).join(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(b) => b,
        };
        let header = Header::decode(&bytes)?;
        if bytes.len() < HEADER_LEN + SEAL_OVERHEAD {
            return Err(invalid(&format!("{name} is too short")));
        }
        let (head, rest) = bytes.split_at(HEADER_LEN);
        let (nonce, sealed) = rest.split_at(NONCE_LEN);
        let contents = key
            .file_cipher(uid, vault, name)
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: &associated_data(head, uid, vault, name),
                },
            )
            .map_err(|_| {
                invalid(&format!(
                    "{name} doesn't decrypt: it was edited, moved from another user, vault \
                     or file, or sealed under another master key"
                ))
            })?;
        Ok(Some((header, Zeroizing::new(contents))))
    }

    /// Seals `contents` as `name` in a vault under the master key, and
    /// replaces the file atomically.
    fn write_sealed(
        &self,
        uid: u32,
        vault: &str,
        name: &str,
        header: &Header,
        contents: &[u8],
    ) -> io::Result<()> {
        let key = self.key()?;
        let head = header.encode();
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
        let sealed = key
            .file_cipher(uid, vault, name)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: contents,
                    aad: &associated_data(&head, uid, vault, name),
                },
            )
            .map_err(|_| io::Error::other(format!("can't seal {name}")))?;
        let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + sealed.len());
        out.extend_from_slice(&head);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        let path = self.vault_dir(uid, vault).join(name);
        let (Some(dir), Some(file)) = (path.parent(), path.file_name().and_then(|f| f.to_str()))
        else {
            return Err(invalid(&format!("bad file name {name:?}")));
        };
        write_atomic(dir, file, &out)
    }
}

/// A locker file's name in its vault, as bound into its key and associated
/// data: `lockers/<locker>`, which no other file in a vault can share, not
/// even for a locker named `daemon_half`.
fn locker_name(locker: &str) -> String {
    format!("{LOCKERS_DIR}/{locker}")
}

/// The header, then `<uid> ‖ 0 ‖ <vault> ‖ 0 ‖ <name>` (§6).
fn associated_data(head: &[u8], uid: u32, vault: &str, name: &str) -> Vec<u8> {
    let uid = uid.to_string();
    let mut ad = Vec::with_capacity(head.len() + uid.len() + vault.len() + name.len() + 2);
    ad.extend_from_slice(head);
    ad.extend_from_slice(uid.as_bytes());
    ad.push(0);
    ad.extend_from_slice(vault.as_bytes());
    ad.push(0);
    ad.extend_from_slice(name.as_bytes());
    ad
}

/// Reads a file's header and the size of its contents, without any key;
/// `None` if it doesn't exist. The header is verified only when the file is
/// decrypted.
pub fn read_header(path: &Path) -> io::Result<Option<(Header, u64)>> {
    let mut f = match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(f) => f,
    };
    let mut head = [0u8; HEADER_LEN];
    f.read_exact(&mut head)?;
    let header = Header::decode(&head)?;
    let size = f
        .metadata()?
        .len()
        .checked_sub((HEADER_LEN + SEAL_OVERHEAD) as u64)
        .ok_or_else(|| invalid("too short to hold its contents"))?;
    Ok(Some((header, size)))
}

/// Entries of a folder; none if it doesn't exist.
pub fn entries(dir: &Path) -> io::Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(rd) => rd.collect(),
    }
}

/// Temp file, sync, rename, sync the folder. Callers hold the vault lock, so
/// one temp name per file is enough.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn mkdir_private(path: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_key::Sealing;

    const UID: u32 = 1000;
    const VAULT: &str = "test_app";

    fn open(dir: &Path, key_byte: u8) -> Store {
        Store::open(
            dir.join("state"),
            MasterKey::new(&[key_byte; 32], Sealing::Unsealed),
        )
        .unwrap()
    }

    /// Fills `api_keys` in uid 1000's `test_app`.
    fn fill(store: &Store) -> Header {
        let lock = store.lock_vault(UID, VAULT).unwrap();
        let header = Header {
            revision: 0xdead_beef,
            expires: 1_792_108_800,
        };
        store
            .write_locker(&lock, UID, VAULT, "api_keys", &header, b"envelope")
            .unwrap();
        header
    }

    #[test]
    fn header_round_trip_and_refusals() {
        let h = Header {
            revision: 0xdead_beef,
            expires: 1_792_108_800,
        };
        let bytes = h.encode();
        assert_eq!(&bytes[..4], b"VLTD");
        assert_eq!(bytes[4], 1);
        assert_eq!(Header::decode(&bytes).unwrap(), h);
        assert!(Header::decode(&bytes[..20]).is_err());
        let mut milestone_1 = bytes;
        milestone_1[4] = 0;
        assert!(Header::decode(&milestone_1).is_err(), "layout 0 is refused");
        let mut bad_magic = bytes;
        bad_magic[0] = b'X';
        assert!(Header::decode(&bad_magic).is_err());
    }

    #[test]
    fn lockers_are_sealed_under_the_master_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), 1);
        let header = fill(&store);
        let bytes = fs::read(store.locker_path(UID, VAULT, "api_keys")).unwrap();
        assert_eq!(Header::decode(&bytes).unwrap(), header);
        assert_eq!(bytes.len(), HEADER_LEN + SEAL_OVERHEAD + b"envelope".len());
        assert!(!bytes.windows(8).any(|w| w == b"envelope"));
        let lock = store.lock_vault(UID, VAULT).unwrap();
        assert_eq!(
            store.read_locker(&lock, UID, VAULT, "api_keys").unwrap(),
            Some((header, b"envelope".to_vec()))
        );
        assert_eq!(
            store.locker_header(&lock, UID, VAULT, "api_keys").unwrap(),
            Some((header, 8)),
            "the size is the contents'"
        );
    }

    #[test]
    fn daemon_half_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), 1);
        let lock = store.lock_vault(UID, VAULT).unwrap();
        let a = store.daemon_half(&lock, UID, VAULT).unwrap();
        let b = store.daemon_half(&lock, UID, VAULT).unwrap();
        assert_eq!(*a, *b);
        let bytes = fs::read(store.vault_dir(UID, VAULT).join(HALF_FILE)).unwrap();
        assert_eq!(
            Header::decode(&bytes).unwrap(),
            Header {
                revision: 0,
                expires: 0
            }
        );
        assert_eq!(bytes.len(), HEADER_LEN + SEAL_OVERHEAD + HALF_LEN);
        assert!(!bytes.windows(HALF_LEN).any(|w| w == &a[..]));
    }

    #[test]
    fn a_file_moved_to_another_user_vault_or_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), 1);
        fill(&store);
        let source = store.locker_path(UID, VAULT, "api_keys");
        for (uid, vault, locker) in [
            (UID + 1, VAULT, "api_keys"),
            (UID, "test_other", "api_keys"),
            (UID, VAULT, "short"),
        ] {
            let lock = store.lock_vault(uid, vault).unwrap();
            let target = store.locker_path(uid, vault, locker);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(&source, &target).unwrap();
            let err = store.read_locker(&lock, uid, vault, locker).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidData,
                "{uid} {vault} {locker}"
            );
        }

        // A locker can't pass for the daemon's half, nor the half for a locker.
        let lock = store.lock_vault(UID, VAULT).unwrap();
        store.daemon_half(&lock, UID, VAULT).unwrap();
        let half = store.vault_dir(UID, VAULT).join(HALF_FILE);
        fs::copy(&half, store.locker_path(UID, VAULT, HALF_FILE)).unwrap();
        assert!(store.read_locker(&lock, UID, VAULT, HALF_FILE).is_err());
        fs::copy(&source, &half).unwrap();
        assert!(store.daemon_half(&lock, UID, VAULT).is_err());
    }

    #[test]
    fn an_edited_header_or_body_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), 1);
        fill(&store);
        let path = store.locker_path(UID, VAULT, "api_keys");
        let original = fs::read(&path).unwrap();
        let lock = store.lock_vault(UID, VAULT).unwrap();
        // The revision, the expiry, the nonce and the ciphertext.
        for at in [5, 12, 13, 20, 21, original.len() - 1] {
            let mut edited = original.clone();
            edited[at] ^= 1;
            fs::write(&path, &edited).unwrap();
            let err = store.read_locker(&lock, UID, VAULT, "api_keys").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "byte {at}");
        }
        fs::write(&path, &original).unwrap();
        assert!(
            store
                .read_locker(&lock, UID, VAULT, "api_keys")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn another_master_key_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let right = open(dir.path(), 1);
        let header = fill(&right);
        let lock = right.lock_vault(UID, VAULT).unwrap();
        let half = right.daemon_half(&lock, UID, VAULT).unwrap();

        let wrong = open(dir.path(), 2);
        assert!(wrong.read_locker(&lock, UID, VAULT, "api_keys").is_err());
        assert!(
            wrong.daemon_half(&lock, UID, VAULT).is_err(),
            "never replaced by a new half"
        );

        assert_eq!(*right.daemon_half(&lock, UID, VAULT).unwrap(), *half);
        assert!(
            right
                .read_locker(&lock, UID, VAULT, "api_keys")
                .unwrap()
                .is_some()
        );

        // Headers need no key: the sweep reads them (§6).
        let keyless = Store::open_without_key(right.root()).unwrap();
        assert_eq!(
            keyless
                .locker_header(&lock, UID, VAULT, "api_keys")
                .unwrap(),
            Some((header, 8))
        );
        assert!(keyless.read_locker(&lock, UID, VAULT, "api_keys").is_err());
    }
}
