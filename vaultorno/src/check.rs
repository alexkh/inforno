//! `vaultorno check <executable>` (design doc §12): a dry run of the
//! daemon's identity check for an executable on disk. It runs the daemon's
//! own [`SignedIdentity`] on what the inspector would report for the file,
//! so the two can't drift apart.
//!
//! It can't check what only a running process shows:
//! - the environment its client names;
//! - the code it has loaded besides its executable;
//! - that it still runs, as the user asking.
//!
//! It also follows a succession only once it's recorded. `revoked_keys`
//! and `successions` are private to the `vaultorno` user, so only root's
//! check applies them.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::{BUILTIN_REJECT_ENV, Statement};
use vaultorno_client::time::{Clock, DAY, SystemClock, format_date};
use vaultorno_inspect::Inspected;

use crate::admin::DEFAULT_STATE_DIR;
use crate::identity::{
    Identify, Identity, Inspector, ManifestIdentity, Peer, SignedIdentity, WATERTIGHT_LEVEL,
};
use crate::log;
use crate::signing::{KeyListFiles, STATEMENT_NAMESPACE, verify_signature};
use crate::store::running_as_root;

/// Where packages put statements (§5), one folder per app.
pub const PACKAGE_STATEMENTS_DIR: &str = "/usr/share/vaultorno/statements";

/// The inspector's report for a file rather than a process.
struct FileInspector(Inspected);

impl Inspector for FileInspector {
    fn inspect(&self, _peer: &Peer) -> Result<Inspected, String> {
        Ok(self.0.clone())
    }
}

/// A statement for the executable, as the client would send it.
struct Found {
    path: PathBuf,
    text: String,
    signature: Option<String>,
}

/// Runs the dry run. `Ok(true)` if the daemon would open a vault for the
/// executable, as far as a file can show.
pub fn check(args: &[String]) -> Result<bool, String> {
    let mut state_dir = PathBuf::from(DEFAULT_STATE_DIR);
    let mut given: Option<PathBuf> = None;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => state_dir = it.next().ok_or("--state-dir needs a folder")?.into(),
            "--statement" => given = Some(it.next().ok_or("--statement needs a file")?.into()),
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            word => words.push(word),
        }
    }
    let [target] = words.as_slice() else {
        return Err("check needs one executable".into());
    };
    let exe = fs::canonicalize(target).map_err(|e| format!("{target}: {e}"))?;
    let inspected = inspect_file(&exe)?;
    let now = SystemClock.now();
    let found = match &given {
        Some(path) => Some(read_found(path)?),
        None => find_statement(&exe, now),
    };

    let mut unchecked = vec![
        format!(
            "its environment: none of {} may be set, nor a variable its statement or \
             manifest rejects",
            BUILTIN_REJECT_ENV.join(" ")
        ),
        "code it loads besides the executable: only root's files, not replaced since".to_owned(),
        "that it still runs, as the user asking".to_owned(),
    ];
    let lists_dir = match KeyListFiles::new(&state_dir).load() {
        Ok(_) => state_dir.clone(),
        Err(e) if running_as_root() => {
            println!("refused     {e}");
            return Ok(false);
        }
        Err(_) => {
            unchecked.push("revoked keys and successions: only root can read them".to_owned());
            // `/` holds neither list, so both read as empty.
            PathBuf::from("/")
        }
    };

    println!("executable  {}", exe.display());
    println!("sha256      {}", inspected.sha256_hex());
    let (text, signature) = match &found {
        None => {
            println!("statement   none next to it or in {PACKAGE_STATEMENTS_DIR}/");
            if given.is_none() {
                unchecked.push(
                    "statements in your home folder: give one with --statement".to_owned(),
                );
            }
            (None, None)
        }
        Some(f) => {
            println!("statement   {}", f.path.display());
            match &f.signature {
                None => println!("            unsigned, so only a package manifest can vouch"),
                Some(sig) => match verify_signature(STATEMENT_NAMESPACE, f.text.as_bytes(), sig) {
                    Ok(by) => println!(
                        "            signed by {} (key id {})",
                        by.fingerprint,
                        by.fingerprint.key_id()
                    ),
                    Err(e) => println!("            signature: {e}"),
                },
            }
            (Some(f.text.as_str()), f.signature.as_deref())
        }
    };
    if signature.is_some() {
        unchecked.push(
            "a succession the app would send with it, which can carry an older key's vault"
                .to_owned(),
        );
    }

    // The daemon's own check, on the file.
    let inspector: Arc<dyn Inspector> = Arc::new(FileInspector(inspected.clone()));
    let peer = Peer::without_pidfd(inspected.uid, None);
    let full = SignedIdentity::new(ManifestIdentity::new(inspector.clone(), false), &lists_dir);
    let mut accepted = match full.identify(&peer, text, signature, None, now) {
        Ok(ident) => {
            // A manifest vouched although there was a signed statement:
            // say why the statement alone didn't pass.
            if signature.is_some() && ident.level == WATERTIGHT_LEVEL {
                let alone = SignedIdentity::new(
                    ManifestIdentity::new(inspector, false).without_manifests(),
                    &lists_dir,
                );
                if let Err(r) = alone.identify(&peer, text, signature, None, now) {
                    println!(
                        "            refused: {}; a package manifest vouches instead",
                        r.message
                    );
                }
            }
            print_identity(&ident);
            true
        }
        Err(r) => {
            println!("verdict     refused: {}", r.message);
            false
        }
    };
    if fs::metadata(&exe).is_ok_and(|m| m.mode() & 0o004 == 0) {
        println!("refused     it isn't world-readable, so the inspector can't hash it");
        accepted = false;
    }
    println!("not checked, because only a running process shows it:");
    for item in &unchecked {
        println!("  - {item}");
    }
    Ok(accepted)
}

fn print_identity(ident: &Identity) {
    println!(
        "verdict     opens {} as {}, {}",
        log::vault_label(Some(&ident.app), &ident.vault),
        ident.binary,
        ident.level
    );
    if ident.vault.len() == 64 {
        println!("            vault {}", ident.vault);
    }
    let t = ident.statement_expires;
    if t != 0 {
        println!("            statement valid until {}", format_date(t));
    }
    for (name, grant) in &ident.lockers {
        let access = match (grant.write, grant.lifetime_secs) {
            (false, _) => "read".to_owned(),
            (true, 0) => "write, never expires once filled".to_owned(),
            (true, secs) => format!("write, filled for {} days", secs / DAY),
        };
        println!("locker      {name}: {access}");
    }
}

/// What the inspector would report for a process running `exe` as this
/// user. No loaded code is reported: only a process has any.
fn inspect_file(exe: &Path) -> Result<Inspected, String> {
    let fail = |e: std::io::Error| format!("can't read {}: {e}", exe.display());
    let meta = fs::metadata(exe).map_err(fail)?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file", exe.display()));
    }
    let mut file = File::open(exe).map_err(fail)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(fail)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Inspected {
        pid: 0,
        uid: rustix::process::geteuid().as_raw(),
        exe_path: exe.to_path_buf(),
        dev: meta.dev(),
        ino: meta.ino(),
        sha256: hasher.finalize().into(),
        code: Vec::new(),
    })
}

fn read_found(path: &Path) -> Result<Found, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut sig = path.as_os_str().to_owned();
    sig.push(".sig");
    Ok(Found {
        path: path.to_path_buf(),
        text,
        signature: fs::read_to_string(PathBuf::from(sig)).ok(),
    })
}

/// The statement the client would send (§5): `<binary>_statement.json`
/// next to the executable, or in a package's folder under
/// [`PACKAGE_STATEMENTS_DIR`]; the newest valid one. A user's own folder
/// isn't searched: give that one with `--statement`.
fn find_statement(exe: &Path, now: u64) -> Option<Found> {
    let stem = exe.file_stem()?.to_str()?;
    if !is_valid_name(stem) {
        return None;
    }
    let file = format!("{stem}_statement.json");
    let mut dirs: Vec<(PathBuf, Option<String>)> = exe
        .parent()
        .map(|d| (d.to_path_buf(), None))
        .into_iter()
        .collect();
    if let Ok(rd) = fs::read_dir(PACKAGE_STATEMENTS_DIR) {
        let mut apps: Vec<String> = rd
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|a| is_valid_name(a))
            .collect();
        apps.sort();
        for app in apps {
            dirs.push((Path::new(PACKAGE_STATEMENTS_DIR).join(&app), Some(app)));
        }
    }
    let mut best: Option<(u64, Found)> = None;
    for (dir, app) in dirs {
        let path = dir.join(&file);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(checked) = Statement::parse(&text).and_then(|s| s.check(now)) else {
            continue;
        };
        // In a package's folder, the client looks only for its own app.
        if checked.binary != stem || app.as_ref().is_some_and(|a| *a != checked.app) {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(issued, _)| checked.issued > *issued)
        {
            let signature = fs::read_to_string(path.with_extension("json.sig")).ok();
            best = Some((
                checked.issued,
                Found {
                    path,
                    text,
                    signature,
                },
            ));
        }
    }
    best.map(|(_, f)| f)
}
