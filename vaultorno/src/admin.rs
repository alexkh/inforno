//! The admin commands (design doc §12): `status`, `vaults`, `log` and
//! `forget`. `check` is in `check.rs`; `revoke` is in `main.rs` and
//! `signing.rs`. None reads a user's half, so none can read contents, even
//! as root.
//!
//! Root runs them in a folder the `vaultorno` user owns. `forget`, the only
//! one here that changes anything:
//! - reaches the state only through folders opened without following a
//!   symlink;
//! - deletes through those folders;
//! - gives whatever it creates (a lock file, the log) its folder's owner.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustix::fs::{AtFlags, Mode, fsync, openat, unlinkat};
use rustix::io::Errno;
use vaultorno_client::names::{is_valid_name, is_valid_vault_name};
use vaultorno_client::protocol::{
    BUILTIN_REJECT_ENV, DEFAULT_LIFETIME_DAYS, DEFAULT_MANIFESTS_DIR, DEFAULT_SOCKET, MAX_DATA,
    MAX_FRAME, MAX_LIFETIME_DAYS, MAX_LOCKERS_PER_APP, MAX_STATEMENT_DAYS, MAX_STORED, MIN_STORED,
};
use vaultorno_client::time::{Clock, DAY, SystemClock, format_date};

use crate::check::PACKAGE_STATEMENTS_DIR;
use crate::identity::{AIRTIGHT_PTRACE_SCOPE, DEFAULT_PTRACE_SCOPE_PATH};
use crate::init::{has_tpm2, systemd_creds};
use crate::log::{self, MAX_LOG_BYTES};
use crate::master_key::{CREDENTIAL_FILE, CREDENTIAL_NAME, MasterKey};
use crate::server::{MAX_CONNECTIONS_PER_USER, MAX_VAULTS_PER_USER};
use crate::signing::{KeyFingerprint, KeyListFiles};
use crate::store::{
    dir_flags, entries, lock_vault_at, open_dir_beneath, read_header, running_as_root,
};
use crate::sweep::STALE_VAULT_SECS;

/// The `vaultorno` user's home, its state folder (§6).
pub const DEFAULT_STATE_DIR: &str = "/var/lib/vaultorno";
const LOCKERS: &str = "lockers";

const STATE_DIR: &str = "--state-dir";
const USER: &str = "--user";
const LOCKER: &str = "--locker";

// ---------------------------------------------------------------------------
// Arguments

struct Args {
    state_dir: PathBuf,
    user: Option<u32>,
    locker: Option<String>,
    words: Vec<String>,
}

/// Parses `args`, accepting only the options in `takes`.
fn parse(args: &[String], takes: &[&str]) -> Result<Args, String> {
    let mut out = Args {
        state_dir: PathBuf::from(DEFAULT_STATE_DIR),
        user: None,
        locker: None,
        words: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        if !flag.starts_with("--") {
            out.words.push(arg.clone());
            continue;
        }
        if !takes.contains(&flag) {
            return Err(format!("unknown option {flag:?}"));
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            STATE_DIR => out.state_dir = value.into(),
            USER => out.user = Some(parse_user(value)?),
            _ => out.locker = Some(value.clone()),
        }
    }
    Ok(out)
}

fn no_words(a: &Args, mode: &str) -> Result<(), String> {
    match a.words.first() {
        Some(word) => Err(format!("{mode} takes no argument {word:?}")),
        None => Ok(()),
    }
}

/// A user ID, or a name in `/etc/passwd`. Like the daemon, the admin
/// commands don't use NSS, so a user only a directory service knows is
/// given by ID.
fn parse_user(s: &str) -> Result<u32, String> {
    if let Ok(uid) = s.parse() {
        return Ok(uid);
    }
    let passwd =
        fs::read_to_string("/etc/passwd").map_err(|e| format!("can't read /etc/passwd: {e}"))?;
    passwd
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == s).then(|| fields.nth(1)?.parse().ok())?
        })
        .ok_or_else(|| format!("no user {s:?} in /etc/passwd; give the user ID instead"))
}

fn unreadable(path: &Path, e: &io::Error) -> String {
    let hint = if e.kind() == io::ErrorKind::PermissionDenied {
        "; it's private to the vaultorno user: run this as root"
    } else {
        ""
    };
    format!("can't read {}: {e}{hint}", path.display())
}

fn user_path(state: &Path, uid: u32) -> PathBuf {
    state.join("users").join(uid.to_string())
}

/// The users with a folder in the state, sorted.
fn user_ids(state: &Path) -> Result<Vec<u32>, String> {
    let users = state.join("users");
    let mut out: Vec<u32> = entries(&users)
        .map_err(|e| unreadable(&users, &e))?
        .into_iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// The vaults in a user's folder, sorted. A symlink is never a vault.
fn vault_names(user: &Path) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = entries(user)
        .map_err(|e| unreadable(user, &e))?
        .into_iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_vault_name(n))
        .collect();
    out.sort();
    Ok(out)
}

fn row(key: &str, value: &str) {
    println!("  {key:<22} {value}");
}

// ---------------------------------------------------------------------------
// `vaultorno status`

/// What vaultorno does on this machine (§12). Works without root; as root,
/// also whether the master key unseals, and the key lists.
pub fn status(args: &[String]) -> Result<(), String> {
    let a = parse(args, &[STATE_DIR])?;
    no_words(&a, "status")?;
    println!("vaultorno {}", env!("CARGO_PKG_VERSION"));

    println!("\nservice");
    let listening =
        fs::symlink_metadata(DEFAULT_SOCKET).is_ok_and(|m| m.file_type().is_socket());
    row(
        "socket",
        &if listening {
            format!("{DEFAULT_SOCKET}, listening")
        } else {
            format!("{DEFAULT_SOCKET} isn't there, so apps use their local fallback")
        },
    );
    for unit in ["vaultorno.socket", "vaultorno_sweep.timer"] {
        row(unit, &unit_state(unit));
    }
    row("state folder", &state_line(&a.state_dir));

    println!("\nplatform");
    let scope = fs::read_to_string(DEFAULT_PTRACE_SCOPE_PATH)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok());
    row("ptrace_scope", &scope_line(scope));
    if let Some(s) = scope.filter(|&s| s < AIRTIGHT_PTRACE_SCOPE) {
        if s == 0 {
            println!("    to serve package manifests, as root:");
            println!("      echo 'kernel.yama.ptrace_scope = 1' > /etc/sysctl.d/50-ptrace_scope.conf");
            println!("      sysctl --system");
            println!("    after which a program can be traced only by those that started it");
        }
        let also = if s == 0 { "as well" } else { "too" };
        println!("    to serve signed statements {also}, as root:");
        println!(
            "      echo 'kernel.yama.ptrace_scope = {AIRTIGHT_PTRACE_SCOPE}' \
             > /etc/sysctl.d/50-ptrace_scope.conf"
        );
        println!("      sysctl --system");
        println!(
            "    after which only root can trace or debug programs (gdb, strace, \
             crash reporters)"
        );
    }
    row(
        "TPM2",
        if has_tpm2() {
            "usable"
        } else {
            "not fully usable: a new master key is sealed with the host key only"
        },
    );
    row("master key", &master_key_line(&a.state_dir));
    if running_as_root() {
        key_lists(&a.state_dir);
    }

    println!("\napps");
    row("package manifests", &listing(DEFAULT_MANIFESTS_DIR, ".json"));
    row("package statements", &listing(PACKAGE_STATEMENTS_DIR, ""));

    println!("\nbuilt in");
    row("lockers per app", &MAX_LOCKERS_PER_APP.to_string());
    row(
        "locker size",
        &format!(
            "{MAX_DATA} bytes of data, stored in classes of {} to {} KiB",
            MIN_STORED / 1024,
            MAX_STORED / 1024
        ),
    );
    row(
        "locker lifetime",
        &format!("{DEFAULT_LIFETIME_DAYS} days by default, at most {MAX_LIFETIME_DAYS}"),
    );
    row("statement validity", &format!("at most {MAX_STATEMENT_DAYS} days"));
    row(
        "vaults per user",
        &format!(
            "{MAX_VAULTS_PER_USER}; deleted after {} days with no filled locker",
            STALE_VAULT_SECS / DAY
        ),
    );
    row("connections per user", &MAX_CONNECTIONS_PER_USER.to_string());
    row("message size", &format!("{} KiB", MAX_FRAME / 1024));
    row(
        "log",
        &format!("{} MiB per file; log.1 and log.2 kept", MAX_LOG_BYTES >> 20),
    );
    row("refused variables", &BUILTIN_REJECT_ENV.join(" "));
    Ok(())
}

fn scope_line(scope: Option<u32>) -> String {
    match scope {
        None => "unreadable (no Yama?): nothing is served".into(),
        Some(0) => format!(
            "0: nothing is served; package manifests (watertight) need 1, signed \
             statements (airtight) {AIRTIGHT_PTRACE_SCOPE}"
        ),
        Some(s) if s < AIRTIGHT_PTRACE_SCOPE => format!(
            "{s}: package manifests (watertight) only; signed statements (airtight) \
             need {AIRTIGHT_PTRACE_SCOPE}"
        ),
        Some(s) => format!("{s}: package manifests (watertight) and signed statements (airtight)"),
    }
}

/// `systemctl is-enabled`, by absolute path with an empty environment (§13).
fn unit_state(unit: &str) -> String {
    let state = Command::new("/usr/bin/systemctl")
        .env_clear()
        .args(["is-enabled", unit])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    match state.as_str() {
        "" => "unknown".into(),
        "enabled" => state,
        _ => format!("{state}: systemctl enable --now {unit}"),
    }
}

fn state_line(dir: &Path) -> String {
    match fs::symlink_metadata(dir) {
        Err(e) => format!("{}: {e}; run `vaultorno init` as root", dir.display()),
        Ok(m) if !m.is_dir() => format!("{} isn't a folder", dir.display()),
        Ok(m) if m.uid() == 0 => format!(
            "{}, root's until the daemon first starts",
            dir.display()
        ),
        Ok(m) => format!(
            "{}, owned by uid {}, mode {:o}",
            dir.display(),
            m.uid(),
            m.mode() & 0o7777
        ),
    }
}

/// As root, unseals `master_key.cred` the way systemd does at every start,
/// and reports its label (§6). That is also the check that the daemon will
/// start. The key itself is never printed, and is zeroized when dropped.
fn master_key_line(state: &Path) -> String {
    if !running_as_root() {
        return "how it's sealed is shown to root; apps see it in Welcome.sealer".into();
    }
    let cred = state.join(CREDENTIAL_FILE);
    if fs::symlink_metadata(&cred).is_err() {
        return "missing: run `vaultorno init`".into();
    }
    let Some(path) = cred.to_str() else {
        return format!("{} isn't a UTF-8 path", cred.display());
    };
    let name = format!("--name={CREDENTIAL_NAME}");
    match systemd_creds(&["decrypt", name.as_str(), path, "-"], &[])
        .and_then(|plain| MasterKey::from_credential(&plain))
    {
        Ok(key) => format!("unseals; {}", key.sealing().describe()),
        Err(e) => format!("doesn't unseal, so the daemon won't start: {e}"),
    }
}

/// `revoked_keys` and `successions`, as the daemon reads them (§6, §8).
fn key_lists(state: &Path) {
    match KeyListFiles::new(state).load() {
        Err(e) => row("key lists", &e),
        Ok(lists) => {
            let revoked = lists.revoked();
            row("revoked keys", &revoked.len().to_string());
            for key in revoked {
                println!("    {key}  (key id {})", key.key_id());
            }
            let successions = lists.successions();
            row("successions", &successions.len().to_string());
            for (from, to) in successions {
                println!("    {from} -> {to}");
            }
        }
    }
}

/// The app names in `dir`: files ending in `suffix`, or folders when
/// `suffix` is empty.
fn listing(dir: &str, suffix: &str) -> String {
    let Ok(rd) = fs::read_dir(dir) else {
        return "none".into();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(suffix).map(str::to_owned))
        .filter(|n| is_valid_name(n))
        .collect();
    names.sort();
    if names.is_empty() {
        "none".into()
    } else {
        format!("{} (in {dir})", names.join(", "))
    }
}

// ---------------------------------------------------------------------------
// `vaultorno vaults`

/// Each user's vaults and their lockers (§12): size classes, revisions and
/// expiries, read from the files' headers. Never contents or halves.
pub fn vaults(args: &[String]) -> Result<(), String> {
    let a = parse(args, &[STATE_DIR, USER])?;
    no_words(&a, "vaults")?;
    let uids = match a.user {
        Some(uid) => vec![uid],
        None => user_ids(&a.state_dir)?,
    };
    let now = SystemClock.now();
    for uid in uids {
        let user = user_path(&a.state_dir, uid);
        let vaults = vault_names(&user)?;
        let plural = if vaults.len() == 1 { "" } else { "s" };
        println!("uid {uid}: {} vault{plural}", vaults.len());
        let apps = app_labels(&a.state_dir, uid);
        for vault in vaults {
            println!("  {}", describe_vault(&vault, &apps));
            print_lockers(&user.join(&vault).join(LOCKERS), now)?;
        }
    }
    Ok(())
}

fn print_lockers(dir: &Path, now: u64) -> Result<(), String> {
    let mut found: Vec<(String, PathBuf)> = entries(dir)
        .map_err(|e| unreadable(dir, &e))?
        .into_iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .filter(|(name, _)| is_valid_name(name))
        .collect();
    found.sort();
    if found.is_empty() {
        println!("    no filled lockers");
    }
    for (name, path) in found {
        match read_header(&path) {
            Ok(Some((h, size))) => {
                let when = if h.expires == 0 {
                    "no expiry".to_owned()
                } else if h.is_expired(now) {
                    format!(
                        "expired {}, deleted at its next access or sweep",
                        format_date(h.expires)
                    )
                } else {
                    format!("expires {}", format_date(h.expires))
                };
                println!(
                    "    {name:<32} {:>3} KiB  revision {:016x}  {when}",
                    size / 1024,
                    h.revision
                );
            }
            Ok(None) => {}
            Err(e) => println!("    {name:<32} damaged: {e}"),
        }
    }
    Ok(())
}

fn describe_vault(vault: &str, apps: &HashMap<String, String>) -> String {
    if let Some(fp) = fingerprint_of(vault) {
        let key_id = fp.key_id();
        let app = apps
            .get(&key_id)
            .map_or("app not in the log", String::as_str);
        format!("{key_id}  {app}, signing key {fp}")
    } else if let Some(app) = vault.strip_prefix("pkg_") {
        format!("{vault}  {app}, package manifest")
    } else {
        format!("{vault}  development test identity")
    }
}

/// A key fingerprint from a vault's 64 hex digits.
fn fingerprint_of(vault: &str) -> Option<KeyFingerprint> {
    if vault.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(vault.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(KeyFingerprint(out))
}

/// The app each key id last opened as, from the user's log. The state
/// keeps no app names (§6); the log's `vault=<app>/<key id>` does.
fn app_labels(state: &Path, uid: u32) -> HashMap<String, String> {
    let Ok(bytes) = log::read(state, uid) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some(label) = line.split(' ').find_map(|f| f.strip_prefix("vault=")) else {
            continue;
        };
        if let Some((app, key_id)) = label.split_once('/') {
            if is_valid_name(app) {
                out.insert(key_id.to_owned(), app.to_owned());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// `vaultorno log`

/// Prints a user's log, oldest first (§11).
pub fn show_log(args: &[String]) -> Result<(), String> {
    let a = parse(args, &[STATE_DIR, USER])?;
    no_words(&a, "log")?;
    let uid = a.user.ok_or("log needs --user U")?;
    let bytes =
        log::read(&a.state_dir, uid).map_err(|e| unreadable(&user_path(&a.state_dir, uid), &e))?;
    print!("{}", printable(&bytes));
    Ok(())
}

/// The log as root's terminal can safely show it: anything but printable
/// ASCII and newlines becomes `?`. The writer escapes all of it, so a `?`
/// means the file was edited by hand.
fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b == b'\n' || (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// `vaultorno forget`

/// What `forget` was told to delete.
enum Target {
    /// A vault's name: 64 hex digits, `pkg_<app>` or `test_<app>`.
    Vault(String),
    /// A key id: the first 16 hex digits of a signing key's vault.
    KeyId(String),
}

/// Checks what `forget` was given before anything is touched: a vault's
/// name, a `SHA256:` fingerprint or a key id, never a path.
fn target(word: &str) -> Result<Target, String> {
    if word.starts_with("SHA256:") {
        return KeyFingerprint::parse(word)
            .map(|fp| Target::Vault(fp.hex()))
            .ok_or_else(|| format!("{word:?} isn't a SHA256: fingerprint"));
    }
    if is_valid_vault_name(word) {
        return Ok(Target::Vault(word.to_owned()));
    }
    if word.len() == 16 && word.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Ok(Target::KeyId(word.to_owned()));
    }
    Err(format!(
        "{word:?} isn't a vault: give its name, its SHA256: fingerprint or its key id"
    ))
}

/// The vault `target` names among `uid`'s, if it has it.
fn resolve(state: &Path, uid: u32, target: &Target) -> Result<Option<String>, String> {
    let vaults = vault_names(&user_path(state, uid))?;
    Ok(match target {
        Target::Vault(v) => vaults.contains(v).then(|| v.clone()),
        Target::KeyId(id) => {
            let mut hits = vaults
                .into_iter()
                .filter(|v| v.len() == 64 && v.starts_with(id.as_str()));
            let first = hits.next();
            if hits.next().is_some() {
                return Err(format!(
                    "key id {id} matches more than one of uid {uid}'s vaults; give the fingerprint"
                ));
            }
            first
        }
    })
}

/// Deletes a vault, the daemon's half included, or one of its lockers
/// (§12). Without `--user`, every user's.
pub fn forget(args: &[String]) -> Result<(), String> {
    let a = parse(args, &[STATE_DIR, USER, LOCKER])?;
    let [word] = a.words.as_slice() else {
        return Err("forget needs one vault: its name, SHA256: fingerprint or key id".into());
    };
    let target = target(word)?;
    if let Some(locker) = &a.locker {
        if !is_valid_name(locker) {
            return Err(format!("{locker:?} isn't a locker name ([a-z0-9_]{{1,32}})"));
        }
    }
    let uids = match a.user {
        Some(uid) => vec![uid],
        None => user_ids(&a.state_dir)?,
    };
    let now = SystemClock.now();
    let mut done = 0;
    for uid in uids {
        let Some(vault) = resolve(&a.state_dir, uid, &target)? else {
            continue;
        };
        forget_in(&a.state_dir, uid, &vault, a.locker.as_deref())
            .map_err(|e| format!("uid {uid}, vault {vault}: {e}"))?;
        log::record(
            &a.state_dir,
            uid,
            now,
            &[
                ("vault", log::vault_label(None, &vault).as_str()),
                ("request", "forget"),
                ("locker", a.locker.as_deref().unwrap_or("-")),
                ("result", "deleted"),
                ("by", "admin"),
            ],
        );
        match &a.locker {
            Some(locker) => println!("uid {uid}: emptied {locker} in {vault}"),
            None => println!("uid {uid}: deleted {vault}, the daemon's half included"),
        }
        done += 1;
    }
    if done == 0 {
        println!("nothing to forget: no such vault");
    }
    Ok(())
}

/// Deletes `locker` in a vault, or the whole vault, holding its lock.
/// - Every folder is opened from the state folder down, never through a
///   symlink.
/// - Every file is deleted through the folder it was found in.
/// - A vault holding anything unexpected is left whole.
fn forget_in(state: &Path, uid: u32, vault: &str, locker: Option<&str>) -> io::Result<()> {
    let uid_s = uid.to_string();
    let user = open_dir_beneath(state, &["users", uid_s.as_str()])?;
    let dir = openat(&user, vault, dir_flags(), Mode::empty())?;
    let _lock = lock_vault_at(&dir)?;
    let lockers = match openat(&dir, LOCKERS, dir_flags(), Mode::empty()) {
        Ok(fd) => Some(fd),
        Err(Errno::NOENT) => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(locker) = locker {
        if let Some(lockers) = &lockers {
            for name in [locker.to_owned(), format!("{locker}.tmp")] {
                remove_at(lockers, &name)?;
            }
            fsync(lockers)?;
        }
        return Ok(());
    }

    // Everything is checked before anything is deleted. The names are read
    // by path but deleted through the folders opened above, so a folder
    // swapped in between only makes a deletion or the final rmdir fail.
    let vault_path = user_path(state, uid).join(vault);
    let in_lockers = match &lockers {
        Some(_) => names(&vault_path.join(LOCKERS))?,
        None => Vec::new(),
    };
    if let Some(odd) = in_lockers.iter().find(|n| !is_locker_file(n)) {
        return Err(unexpected(odd));
    }
    let in_vault = names(&vault_path)?;
    if let Some(odd) = in_vault
        .iter()
        .find(|n| !matches!(n.as_str(), "lock" | "daemon_half" | "daemon_half.tmp" | LOCKERS))
    {
        return Err(unexpected(odd));
    }
    if let Some(lockers) = &lockers {
        for name in &in_lockers {
            remove_at(lockers, name)?;
        }
        unlinkat(&dir, LOCKERS, AtFlags::REMOVEDIR)?;
    }
    for name in ["daemon_half", "daemon_half.tmp", "lock"] {
        remove_at(&dir, name)?;
    }
    unlinkat(&user, vault, AtFlags::REMOVEDIR)?;
    fsync(&user)?;
    Ok(())
}

fn names(dir: &Path) -> io::Result<Vec<String>> {
    Ok(entries(dir)?
        .iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
}

fn is_locker_file(name: &str) -> bool {
    is_valid_name(name) || name.strip_suffix(".tmp").is_some_and(is_valid_name)
}

fn unexpected(name: &str) -> io::Error {
    io::Error::other(format!(
        "unexpected {name:?} in the vault; nothing deleted, look at it by hand"
    ))
}

/// Deletes `name` in `dir`; a file already gone is fine.
fn remove_at(dir: &OwnedFd, name: &str) -> io::Result<()> {
    match unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn forget_takes_only_vault_names() {
        assert!(matches!(target("pkg_inforno"), Ok(Target::Vault(v)) if v == "pkg_inforno"));
        assert!(matches!(target(&"ab".repeat(32)), Ok(Target::Vault(_))));
        assert!(matches!(target("0123456789abcdef"), Ok(Target::KeyId(_))));
        for bad in [
            "../../etc",
            "pkg_../x",
            "/etc",
            "",
            "users",
            "0123456789ABCDEF",
            "SHA256:nope",
        ] {
            assert!(target(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_log_reaches_the_terminal_as_printable_ascii() {
        assert_eq!(
            printable(b"ok\n\x1b[2Jx\xe2\x80\xae\n"),
            "ok\n?[2Jx???\n"
        );
    }

    #[test]
    fn options_belong_to_their_command() {
        assert!(parse(&args(&["--locker", "a"]), &[STATE_DIR]).is_err());
        assert!(parse(&args(&["--user"]), &[STATE_DIR, USER]).is_err());
        let a = parse(
            &args(&["--user", "1000", "v", "--locker", "a"]),
            &[STATE_DIR, USER, LOCKER],
        )
        .unwrap();
        assert_eq!(a.user, Some(1000));
        assert_eq!(a.locker.as_deref(), Some("a"));
        assert_eq!(a.words, ["v"]);
    }
}
