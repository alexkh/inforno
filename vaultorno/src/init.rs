//! `vaultorno init` (design doc §6, §12): creates the state folder with its
//! `CACHEDIR.TAG`, and seals a new master key into `master_key.cred` with
//! `systemd-creds`. Run once, as root, by the package's install script. It
//! never replaces a master key: that would make every vault unreadable.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use zeroize::Zeroizing;

use crate::master_key::{CREDENTIAL_FILE, CREDENTIAL_NAME, MasterKey, Sealing};

/// Backup tools' convention (§14); the name is fixed by it.
pub const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_TAG_TEXT: &str = "\
Signature: 8a477f597d28d172789f06886806bc55
# This file is a cache directory tag created by vaultorno.
# Its lockers are disposable and bound to this machine: leave them out of backups.
# For information about cache directory tags, see https://bford.info/cachedir/
";
/// Run by absolute path with an empty environment (§13).
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";

/// Creates the state folder and seals a new master key into it. Returns how
/// the key was sealed.
pub fn init(state_dir: &Path) -> Result<Sealing, String> {
    let cred = state_dir.join(CREDENTIAL_FILE);
    match fs::symlink_metadata(&cred) {
        Ok(_) => {
            return Err(format!(
                "{} already exists; replacing the master key would make every vault unreadable",
                cred.display()
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("can't check {}: {e}", cred.display())),
    }
    make_state_dir(state_dir)?;
    write_cachedir_tag(state_dir)?;

    // TPM2 when it's fully usable, otherwise the host key (§6). Chosen here
    // rather than with `--with-key=auto`, so the credential can say which.
    let sealing = if has_tpm2() {
        Sealing::HostAndTpm2
    } else {
        Sealing::Host
    };
    let key = MasterKey::generate(sealing)?;
    let plain = key.to_credential();
    let name = format!("--name={CREDENTIAL_NAME}");
    let with_key = format!("--with-key={}", sealing.label());
    let sealed = systemd_creds(
        &["encrypt", name.as_str(), with_key.as_str(), "-", "-"],
        &plain,
    )?;
    // Kept only once it unseals to the same bytes on this machine.
    let back = systemd_creds(&["decrypt", name.as_str(), "-", "-"], &sealed)?;
    if *back != *plain {
        return Err("the sealed master key doesn't unseal to the same bytes".into());
    }
    publish(state_dir, &sealed).map_err(|e| format!("can't write {}: {e}", cred.display()))?;
    Ok(sealing)
}

fn make_state_dir(dir: &Path) -> Result<(), String> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_dir() => Ok(()),
            Ok(_) => Err(format!("{} isn't a folder", dir.display())),
            Err(e) => Err(format!("can't check {}: {e}", dir.display())),
        },
        Err(e) => Err(format!("can't create {}: {e}", dir.display())),
    }
}

/// Writes `CACHEDIR.TAG` unless it's there already. `create_new` never
/// follows a planted link.
fn write_cachedir_tag(dir: &Path) -> Result<(), String> {
    let path = dir.join(CACHEDIR_TAG);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&path)
    {
        Ok(mut f) => f
            .write_all(CACHEDIR_TAG_TEXT.as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("can't write {}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("can't create {}: {e}", path.display())),
    }
}

/// Writes the credential under a temp name, then links it into place, which
/// fails if another `init` got there first.
fn publish(dir: &Path, sealed: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{CREDENTIAL_FILE}.{}.tmp", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(sealed)?;
            f.sync_all()
        });
    let linked = written.and_then(|()| fs::hard_link(&tmp, dir.join(CREDENTIAL_FILE)));
    let _ = fs::remove_file(&tmp);
    linked?;
    File::open(dir)?.sync_all()
}

/// Whether `systemd-creds has-tpm2` reports full TPM2 support.
fn has_tpm2() -> bool {
    Command::new(SYSTEMD_CREDS)
        .env_clear()
        .arg("has-tpm2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn systemd_creds(args: &[&str], input: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let what = args[0];
    let mut child = Command::new(SYSTEMD_CREDS)
        .env_clear()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run {SYSTEMD_CREDS}: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = Zeroizing::new(input.to_vec());
    let writer = thread::spawn(move || stdin.write_all(&input));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("systemd-creds {what}: {e}"))?;
    let written = writer
        .join()
        .map_err(|_| "the writer thread panicked".to_owned())?;
    if !out.status.success() {
        return Err(format!(
            "systemd-creds {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    written.map_err(|e| format!("systemd-creds {what}: {e}"))?;
    Ok(Zeroizing::new(out.stdout))
}
