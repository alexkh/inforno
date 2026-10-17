//! The daemon's state in `~vaultorno` (design doc §6):
//!
//! ```text
//! <state>/users/<uid>/<vault>/
//!     lock                 serialises the daemon's threads and the sweep
//!     daemon_half          the daemon's half of the app key
//!     lockers/<locker>     one locker: the client's envelope
//! ```
//!
//! Every `daemon_half` and locker file has a fixed layout:
//!
//! | Bytes | Field |
//! |---|---|
//! | 4 | magic `VLTD` |
//! | 1 | layout version |
//! | 8 | revision, big-endian (`0` for `daemon_half`) |
//! | 8 | expiry, Unix seconds, big-endian (`0` for none) |
//! | rest | the contents |
//!
//! Milestone 1 writes layout `0`: the contents as they are, without the
//! daemon's outer encryption. Layout `1` (Milestone 4) adds a nonce, the
//! outer encryption under the master key, and the tag.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};
use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::HALF_LEN;
use zeroize::Zeroizing;

pub const MAGIC: [u8; 4] = *b"VLTD";
/// The layout Milestone 1 writes and reads.
pub const LAYOUT: u8 = 0;
pub const HEADER_LEN: usize = 21;

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
}

impl Store {
    /// Opens (and creates) the state folder.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Store> {
        let root = root.into();
        mkdir_private(&root)?;
        mkdir_private(&root.join("users"))?;
        Ok(Store { root })
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

    /// The daemon's half of the vault's app key, created on first use.
    pub fn daemon_half(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
    ) -> io::Result<Zeroizing<[u8; HALF_LEN]>> {
        let dir = self.vault_dir(uid, vault);
        if let Some((_, body)) = read_file(&dir.join(HALF_FILE))? {
            let body = Zeroizing::new(body);
            let half: [u8; HALF_LEN] = body
                .as_slice()
                .try_into()
                .map_err(|_| invalid("daemon_half has the wrong size"))?;
            return Ok(Zeroizing::new(half));
        }
        let mut half = Zeroizing::new([0u8; HALF_LEN]);
        getrandom::fill(&mut half[..])
            .map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
        let mut file = Zeroizing::new(
            Header {
                revision: 0,
                expires: 0,
            }
            .encode()
            .to_vec(),
        );
        file.extend_from_slice(&half[..]);
        write_atomic(&dir, HALF_FILE, &file)?;
        Ok(half)
    }

    pub fn read_locker(
        &self,
        _lock: &VaultLock,
        uid: u32,
        vault: &str,
        locker: &str,
    ) -> io::Result<Option<(Header, Vec<u8>)>> {
        read_file(&self.locker_path(uid, vault, locker))
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
        let dir = self.lockers_dir(uid, vault);
        mkdir_private(&dir)?;
        let mut file = Vec::with_capacity(HEADER_LEN + body.len());
        file.extend_from_slice(&header.encode());
        file.extend_from_slice(body);
        write_atomic(&dir, locker, &file)
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
}

/// Reads a whole file; `None` if it doesn't exist.
pub fn read_file(path: &Path) -> io::Result<Option<(Header, Vec<u8>)>> {
    let mut bytes = Vec::new();
    match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(mut f) => {
            f.read_to_end(&mut bytes)?;
        }
    }
    let header = Header::decode(&bytes)?;
    bytes.drain(..HEADER_LEN);
    Ok(Some((header, bytes)))
}

/// Reads a file's header and the size of what follows; `None` if it doesn't
/// exist.
pub fn read_header(path: &Path) -> io::Result<Option<(Header, u64)>> {
    let mut f = match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(f) => f,
    };
    let mut head = [0u8; HEADER_LEN];
    f.read_exact(&mut head)?;
    let len = f.metadata()?.len();
    Ok(Some((Header::decode(&head)?, len - HEADER_LEN as u64)))
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

    #[test]
    fn header_round_trip_and_refusals() {
        let h = Header {
            revision: 0xdead_beef,
            expires: 1_792_108_800,
        };
        let bytes = h.encode();
        assert_eq!(&bytes[..4], b"VLTD");
        assert_eq!(Header::decode(&bytes).unwrap(), h);
        assert!(Header::decode(&bytes[..20]).is_err());
        let mut other_layout = bytes;
        other_layout[4] = 1;
        assert!(Header::decode(&other_layout).is_err());
        let mut bad_magic = bytes;
        bad_magic[0] = b'X';
        assert!(Header::decode(&bad_magic).is_err());
    }

    #[test]
    fn daemon_half_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        let lock = store.lock_vault(1000, "test_app").unwrap();
        let a = store.daemon_half(&lock, 1000, "test_app").unwrap();
        let b = store.daemon_half(&lock, 1000, "test_app").unwrap();
        assert_eq!(*a, *b);
        let (h, body) = read_file(&store.vault_dir(1000, "test_app").join("daemon_half"))
            .unwrap()
            .unwrap();
        assert_eq!(
            h,
            Header {
                revision: 0,
                expires: 0
            }
        );
        assert_eq!(body.len(), 32);
    }
}
