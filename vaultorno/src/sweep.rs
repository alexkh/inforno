//! `vaultorno sweep` (design doc §6): deletes expired lockers even if no app
//! asks, and vaults with no filled locker for 90 days. It needs no key: it
//! reads expiries from the files' headers.

use std::fs;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use vaultorno_client::names::{is_valid_name, is_valid_vault_name};
use vaultorno_client::time::DAY;

use crate::store::{Store, entries, read_header};

/// A vault with no filled locker for this long is deleted.
pub const STALE_VAULT_SECS: u64 = 90 * DAY;
/// Temp files older than this are leftovers of a crash.
pub const TEMP_FILE_SECS: u64 = 3600;

/// What a sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub expired_lockers: usize,
    pub stale_vaults: usize,
    pub temp_files: usize,
}

/// Sweeps every user's vaults as of `now`.
pub fn sweep(store: &Store, now: u64) -> io::Result<SweepReport> {
    let mut report = SweepReport::default();
    for user in entries(&store.users_dir())? {
        let Some(uid) = user
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !user.path().is_dir() {
            continue;
        }
        for vault in entries(&user.path())? {
            let Some(name) = vault.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_valid_vault_name(&name) || !vault.path().is_dir() {
                continue;
            }
            sweep_vault(store, uid, &name, now, &mut report)?;
        }
    }
    Ok(report)
}

fn sweep_vault(
    store: &Store,
    uid: u32,
    vault: &str,
    now: u64,
    report: &mut SweepReport,
) -> io::Result<()> {
    let _lock = store.lock_vault(uid, vault)?;
    let lockers_dir = store.lockers_dir(uid, vault);
    let mut filled = 0;
    for entry in entries(&lockers_dir)? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.ends_with(".tmp") {
            if modified(&entry.path())? + TEMP_FILE_SECS <= now {
                fs::remove_file(entry.path())?;
                report.temp_files += 1;
            }
            continue;
        }
        if !is_valid_name(&name) {
            continue;
        }
        match read_header(&entry.path()) {
            Ok(Some((h, _))) if h.is_expired(now) => {
                fs::remove_file(entry.path())?;
                report.expired_lockers += 1;
            }
            // Live, or damaged: leave damaged files for the daemon to report.
            _ => filled += 1,
        }
    }
    if filled == 0 {
        let vault_dir = store.vault_dir(uid, vault);
        let since = if lockers_dir.is_dir() {
            modified(&lockers_dir)?
        } else {
            modified(&vault_dir)?
        };
        if since + STALE_VAULT_SECS <= now {
            fs::remove_dir_all(&vault_dir)?;
            report.stale_vaults += 1;
        }
    }
    Ok(())
}

fn modified(path: &Path) -> io::Result<u64> {
    Ok(fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()))
}
