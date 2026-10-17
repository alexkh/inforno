//! Small file helpers: private folders, atomic writes, randomness.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use crate::Error;

/// `OpenOptions` for files private to this user: mode `0600` on Unix. On
/// Windows a new file inherits its folder's ACL, which under
/// `%LocalAppData%` grants only the user, SYSTEM and Administrators
/// (checking that ACL is Milestone 7).
pub(crate) fn private_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Opens (creating) `path` and locks it exclusively until the returned file
/// is dropped: `flock` on Unix, `LockFileEx` on Windows (std, Rust 1.89).
pub(crate) fn lock_file(path: &Path) -> io::Result<File> {
    let file = private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

/// Creates `path` (mode `0700`) if missing, its parents as usual, and checks
/// that it is a real folder owned by this user with no group or other
/// access.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_dir() {
        return Err(Error::Private(format!("{} isn't a folder", path.display())));
    }
    #[cfg(unix)]
    check_owner_and_mode(path, &meta)?;
    Ok(())
}

/// Checks a private file or folder: owned by this user, no group or other
/// access.
#[cfg(unix)]
pub(crate) fn check_owner_and_mode(path: &Path, meta: &fs::Metadata) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    let euid = rustix::process::geteuid().as_raw();
    if meta.uid() != euid {
        return Err(Error::Private(format!(
            "{} is owned by uid {}, not {euid}",
            path.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(Error::Private(format!(
            "{} has mode {:o}; group and others must have no access",
            path.display(),
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// Replaces `dir/name` atomically: temp file, sync, rename, sync the folder.
/// Callers serialise writers, so one temp name per file is enough.
pub(crate) fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

/// Makes a rename in `dir` durable. Windows can't open a folder as a file,
/// and has nothing to sync after its rename; there this does nothing.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

pub(crate) fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
    Ok(out)
}

/// A random revision: never 0 (empty) and never `previous`.
pub(crate) fn new_revision(previous: u64) -> io::Result<u64> {
    loop {
        let r = u64::from_be_bytes(random_bytes::<8>()?);
        if r != 0 && r != previous {
            return Ok(r);
        }
    }
}
