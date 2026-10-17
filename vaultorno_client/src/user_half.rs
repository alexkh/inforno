//! The user's half of an app key (design doc §6): 32 random bytes in
//! `~/.local/share/vaultorno/<app>_<key id>/user_half`, mode `0600`, in a
//! `0700` folder. The file permissions are the boundary that keeps the
//! daemon out.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::Error;
use crate::files::{check_owner_and_mode, ensure_private_dir, random_bytes, sync_dir};
use crate::names::{is_valid_name, is_valid_vault_name};
use crate::protocol::HALF_LEN;

const FILE_NAME: &str = "user_half";

/// `<app>_<key id>` for a signed vault, `<app>_pkg` for a package manifest,
/// `<app>_test` for Milestone 1's test identity.
pub(crate) fn dir_name(app: &str, vault: &str) -> Option<String> {
    if !is_valid_name(app) || !is_valid_vault_name(vault) {
        return None;
    }
    let suffix = if vault.len() == 64 {
        &vault[..16]
    } else if vault.starts_with("pkg_") {
        "pkg"
    } else {
        "test"
    };
    Some(format!("{app}_{suffix}"))
}

/// Where the user's half of `vault` lives under `home`.
pub(crate) fn path(home: &Path, app: &str, vault: &str) -> Result<PathBuf, Error> {
    let dir =
        dir_name(app, vault).ok_or_else(|| Error::Protocol(format!("bad vault name {vault:?}")))?;
    Ok(home
        .join(".local/share/vaultorno")
        .join(dir)
        .join(FILE_NAME))
}

/// Reads the user's half; if it's missing and `create` is set, creates it.
/// Two binaries creating it at once end up with one file.
pub(crate) fn load(
    home: &Path,
    app: &str,
    vault: &str,
    create: bool,
) -> Result<Option<Zeroizing<[u8; HALF_LEN]>>, Error> {
    let file = path(home, app, vault)?;
    let dir = file.parent().expect("the half has a folder");
    if create {
        ensure_private_dir(dir.parent().expect("the folder has a parent"))?;
        ensure_private_dir(dir)?;
    } else {
        match fs::symlink_metadata(dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(Error::Private(format!("{} isn't a folder", dir.display())));
            }
            Ok(meta) => check_owner_and_mode(dir, &meta)?,
        }
    }
    if let Some(half) = read(&file)? {
        return Ok(Some(half));
    }
    if !create {
        return Ok(None);
    }

    // Publish atomically: write a temp file, then hard-link it into place,
    // which fails if another binary got there first.
    let half = Zeroizing::new(random_bytes::<HALF_LEN>()?);
    let tmp = dir.join(format!("{FILE_NAME}.{}.tmp", hex(&random_bytes::<8>()?)));
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&half[..])?;
        f.sync_all()?;
    }
    let linked = fs::hard_link(&tmp, &file);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(dir)?;
            Ok(Some(half))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match read(&file)? {
            Some(theirs) => Ok(Some(theirs)),
            None => Err(Error::Private(format!(
                "{} vanished while being created",
                file.display()
            ))),
        },
        Err(e) => Err(e.into()),
    }
}

fn read(file: &Path) -> Result<Option<Zeroizing<[u8; HALF_LEN]>>, Error> {
    let meta = match fs::symlink_metadata(file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(meta) => meta,
    };
    if !meta.file_type().is_file() {
        return Err(Error::Private(format!(
            "{} isn't a regular file",
            file.display()
        )));
    }
    check_owner_and_mode(file, &meta)?;
    if meta.len() != HALF_LEN as u64 {
        return Err(Error::Private(format!(
            "{} is {} bytes, not {HALF_LEN}",
            file.display(),
            meta.len()
        )));
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file)?;
    let mut half = Zeroizing::new([0u8; HALF_LEN]);
    f.read_exact(&mut half[..])?;
    Ok(Some(half))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn dir_names() {
        let fp = "0123456789abcdef".repeat(4);
        assert_eq!(
            dir_name("inforno", &fp).unwrap(),
            "inforno_0123456789abcdef"
        );
        assert_eq!(dir_name("inforno", "pkg_inforno").unwrap(), "inforno_pkg");
        assert_eq!(dir_name("inforno", "test_inforno").unwrap(), "inforno_test");
        assert!(dir_name("inforno", "../../etc").is_none());
        assert!(dir_name("In-forno", "test_x").is_none());
    }

    #[test]
    fn creates_once_and_reads_back() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            load(home.path(), "app", "test_app", false)
                .unwrap()
                .is_none()
        );
        let a = load(home.path(), "app", "test_app", true).unwrap().unwrap();
        let b = load(home.path(), "app", "test_app", false)
            .unwrap()
            .unwrap();
        assert_eq!(*a, *b);
        let file = path(home.path(), "app", "test_app").unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn racing_creators_agree() {
        let home = Arc::new(tempfile::tempdir().unwrap());
        let halves: Vec<_> = (0..8)
            .map(|_| {
                let home = home.clone();
                thread::spawn(move || *load(home.path(), "app", "test_app", true).unwrap().unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert!(halves.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn refuses_bad_files() {
        let home = tempfile::tempdir().unwrap();
        load(home.path(), "app", "test_app", true).unwrap();
        let file = path(home.path(), "app", "test_app").unwrap();

        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load(home.path(), "app", "test_app", false),
            Err(Error::Private(_))
        ));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

        fs::write(&file, b"short").unwrap();
        assert!(matches!(
            load(home.path(), "app", "test_app", false),
            Err(Error::Private(_))
        ));

        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &file).unwrap();
        assert!(matches!(
            load(home.path(), "app", "test_app", false),
            Err(Error::Private(_))
        ));

        let dir = file.parent().unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            load(home.path(), "app", "test_app", false),
            Err(Error::Private(_))
        ));
    }
}
