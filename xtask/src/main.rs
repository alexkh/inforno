//! `cargo xtask`: build tasks for the Inforno workspace.
//!
//! `cargo xtask build` builds the release binaries, then writes a signed
//! vaultorno statement next to each (vaultorno design doc §5, §8):
//! `<binary>_statement.json`, naming the executable's SHA-256 and the
//! lockers it may use, and `<binary>_statement.json.sig`, made by
//! `ssh-keygen -Y sign` with namespace `vaultorno/statement`. Every
//! signature is checked with the daemon's own code before anything is
//! written. `cargo xtask sign` does the same for binaries already built.
//!
//! Build and sign out of reach of your other programs (§8): a binary
//! swapped after it was hashed only gets a mismatch, but one swapped before
//! gets a signed statement of its own.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use sha2::{Digest, Sha256};
use vaultorno::signing::{KeyFingerprint, STATEMENT_NAMESPACE, parse_key_line, verify_signature};
use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::{Access, LockerGrant, MAX_STATEMENT_DAYS, Statement};
use vaultorno_client::time::{Clock, DAY, SystemClock, format_date, format_rfc3339};

/// The app every statement is for.
const APP: &str = "inforno";

/// One locker in a statement: name, access, and for writers the lifetime in
/// days (default 30).
type LockerSpec = (&'static str, Access, Option<u32>);

/// What the binaries of each workspace member may do with Inforno's lockers
/// (§15), keyed by the member's folder; the package name comes from
/// `cargo metadata`.
const MEMBERS: &[(&str, &[LockerSpec])] = &[
    ("inforno_gui", &[("api_keys", Access::Write, Some(30))]),
    ("autorno", &[("api_keys", Access::Read, None)]),
];

/// Development statements live 7 days (§8).
const DEFAULT_DAYS: u64 = 7;
/// Statements are dated a minute back, so a clock slightly behind accepts them.
const BACKDATE_SECS: u64 = 60;
/// The development signing key, under the home folder (§8).
const DEFAULT_KEY: &str = ".ssh/vaultorno_sign_sk";

const USAGE: &str = "\
usage: cargo xtask <task> [options]

tasks:
  build   build the release binaries, then sign their statements
  sign    sign the statements of release binaries already built

options:
  --key PATH   key for ssh-keygen -Y sign (default ~/.ssh/vaultorno_sign_sk)
  --days N     how long the statements are valid (default 7, at most 180)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build") => parse(&args[1..]).and_then(|o| run(&o, true)),
        Some("sign") => parse(&args[1..]).and_then(|o| run(&o, false)),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    key: PathBuf,
    days: u64,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut key = None;
    let mut days = DEFAULT_DAYS;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--key" => key = Some(PathBuf::from(it.next().ok_or("--key needs a path")?)),
            "--days" => {
                days = it
                    .next()
                    .and_then(|d| d.parse::<u64>().ok())
                    .filter(|d| (1..=MAX_STATEMENT_DAYS).contains(d))
                    .ok_or_else(|| format!("--days needs a number from 1 to {MAX_STATEMENT_DAYS}"))?;
            }
            other => return Err(format!("unknown option {other:?}\n\n{USAGE}")),
        }
    }
    let key = match key {
        Some(k) => k,
        None => std::env::home_dir()
            .ok_or("no home folder; pass --key")?
            .join(DEFAULT_KEY),
    };
    Ok(Options { key, days })
}

fn run(opts: &Options, build: bool) -> Result<(), String> {
    if !opts.key.is_file() {
        return Err(format!(
            "no signing key at {}; pass --key, or make the development key (§8) with\n  \
             ssh-keygen -t ed25519-sk -O application=ssh:vaultorno_signing -f ~/.ssh/vaultorno_sign_sk",
            opts.key.display()
        ));
    }
    let expected = expected_signer(&opts.key)?;
    let root = workspace_root();
    let (target_dir, packages) = metadata(&root)?;
    // Everything is checked before anything is built.
    let mut members = Vec::with_capacity(MEMBERS.len());
    for (dir, lockers) in MEMBERS {
        let package = packages
            .get(Path::new(dir))
            .ok_or_else(|| format!("the workspace has no member in {dir}/"))?;
        if package.bins.is_empty() {
            return Err(format!("{} builds no binary", package.manifest.display()));
        }
        if let Some(bad) = package.bins.iter().find(|b| !is_valid_name(b)) {
            return Err(format!(
                "binary {bad:?} of package {} isn't a vaultorno name ([a-z0-9_]{{1,32}}), \
                 so no statement can name it; rename it in {}, e.g.\n\n\
                 [[bin]]\nname = \"{}\"\npath = \"src/main.rs\"",
                package.name,
                package.manifest.display(),
                bad.replace('-', "_")
            ));
        }
        members.push((package, *lockers));
    }
    if build {
        let mut cmd = cargo(&root);
        cmd.args(["build", "--release"]);
        for (package, _) in &members {
            cmd.args(["--package", package.name.as_str()]);
        }
        let status = cmd.status().map_err(|e| format!("can't run cargo: {e}"))?;
        if !status.success() {
            return Err("cargo build failed".into());
        }
    }
    let release = target_dir.join("release");
    let work = target_dir.join("xtask");
    fs::create_dir_all(&work).map_err(|e| format!("can't create {}: {e}", work.display()))?;

    let now = SystemClock.now();
    let mut signer = None;
    for (package, lockers) in &members {
        for bin in &package.bins {
            signer = Some(sign_binary(opts, &release, &work, bin, lockers, expected, now)?);
        }
    }
    if let Some(fp) = signer {
        println!(
            "\nsigned by {fp}, valid until {} ({} days)\n\
             vault {}, unless a succession carries an older key's\n\
             user's half: ~/.local/share/vaultorno/{APP}_{}/user_half",
            format_date(now - BACKDATE_SECS + opts.days * DAY),
            opts.days,
            fp.hex(),
            fp.key_id()
        );
    }
    Ok(())
}

/// Hashes `release/<bin>`, signs a statement for it, checks the signature as
/// the daemon would, and only then writes both files next to the binary.
fn sign_binary(
    opts: &Options,
    release: &Path,
    work: &Path,
    bin: &str,
    lockers: &[LockerSpec],
    expected: Option<KeyFingerprint>,
    now: u64,
) -> Result<KeyFingerprint, String> {
    if !is_valid_name(bin) {
        return Err(format!(
            "binary {bin:?} isn't a vaultorno name ([a-z0-9_]{{1,32}}), so no statement \
             can name it; set its [[bin]] name"
        ));
    }
    let exe = release.join(bin);
    let sha256 = sha256_of(&exe)?;
    let text = statement_text(bin, &sha256, lockers, now, opts.days)?;

    // ssh-keygen writes `<file>.sig`, and asks before replacing one.
    let name = format!("{bin}_statement.json");
    let sig_name = format!("{name}.sig");
    let draft = work.join(&name);
    let draft_sig = work.join(&sig_name);
    remove_if_exists(&draft_sig)?;
    fs::write(&draft, &text).map_err(|e| format!("can't write {}: {e}", draft.display()))?;
    println!(
        "signing {bin} ({}…); touch the security key when it blinks",
        &sha256[..16]
    );
    let status = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", STATEMENT_NAMESPACE, "-f"])
        .arg(&opts.key)
        .arg(&draft)
        .status()
        .map_err(|e| format!("can't run ssh-keygen: {e}"))?;
    if !status.success() {
        return Err(format!("ssh-keygen couldn't sign {}", draft.display()));
    }
    let signature = fs::read_to_string(&draft_sig)
        .map_err(|e| format!("can't read {}: {e}", draft_sig.display()))?;

    // The daemon's own check, over the exact bytes, before anything is published.
    let by = verify_signature(STATEMENT_NAMESPACE, text.as_bytes(), &signature)
        .map_err(|e| format!("{bin}: the new signature doesn't verify: {e}"))?;
    if expected.is_some_and(|want| want != by.fingerprint) {
        return Err(format!(
            "{bin}: signed by {}, not by the key in {}.pub",
            by.fingerprint,
            opts.key.display()
        ));
    }
    if sha256_of(&exe)? != sha256 {
        return Err(format!(
            "{} changed while it was being signed; nothing written",
            exe.display()
        ));
    }
    write_atomic(release, &name, text.as_bytes())?;
    write_atomic(release, &sig_name, signature.as_bytes())?;
    let grants: Vec<String> = lockers
        .iter()
        .map(|(locker, access, _)| match access {
            Access::Read => format!("{locker}:read"),
            Access::Write => format!("{locker}:write"),
        })
        .collect();
    println!(
        "  wrote {}, for {}",
        release.join(&name).display(),
        grants.join(", ")
    );
    Ok(by.fingerprint)
}

/// The statement's exact text (§5), refused here if the daemon would refuse it.
fn statement_text(
    bin: &str,
    sha256: &str,
    lockers: &[LockerSpec],
    now: u64,
    days: u64,
) -> Result<String, String> {
    let issued = now - BACKDATE_SECS;
    let statement = Statement {
        format: 1,
        app: APP.into(),
        binary: bin.into(),
        sha256: sha256.into(),
        issued: format_rfc3339(issued),
        expires: format_rfc3339(issued + days * DAY),
        level: None,
        reject_env: Vec::new(),
        lockers: lockers
            .iter()
            .map(|(name, access, lifetime)| {
                (
                    (*name).to_owned(),
                    LockerGrant {
                        access: *access,
                        lifetime_days: *lifetime,
                    },
                )
            })
            .collect(),
    };
    let mut text = serde_json::to_string_pretty(&statement).map_err(|e| e.to_string())?;
    text.push('\n');
    Statement::parse(&text)
        .and_then(|s| s.check(now))
        .map_err(|e| format!("{bin}: {e}"))?;
    Ok(text)
}

/// The key `ssh-keygen` should sign with, from `<key>.pub` if it's there.
fn expected_signer(key: &Path) -> Result<Option<KeyFingerprint>, String> {
    let mut path = key.as_os_str().to_owned();
    path.push(".pub");
    let path = PathBuf::from(path);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| format!("{} is empty", path.display()))?;
    parse_key_line(line)
        .map(|k| Some(KeyFingerprint::of(&k)))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A workspace package, as `cargo metadata` describes it.
struct Package {
    name: String,
    /// Its `Cargo.toml`.
    manifest: PathBuf,
    /// Its binaries that build without extra features.
    bins: Vec<String>,
}

/// The target folder, and the workspace's packages keyed by their folder
/// relative to the workspace root, from `cargo metadata`.
fn metadata(root: &Path) -> Result<(PathBuf, BTreeMap<PathBuf, Package>), String> {
    let out = cargo(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("can't run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err("cargo metadata failed".into());
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let target = meta["target_directory"]
        .as_str()
        .ok_or("cargo metadata gave no target_directory")?;
    let workspace = meta["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .ok_or("cargo metadata gave no workspace_root")?;
    let mut packages = BTreeMap::new();
    for package in meta["packages"].as_array().into_iter().flatten() {
        let (Some(name), Some(manifest)) = (
            package["name"].as_str(),
            package["manifest_path"].as_str(),
        ) else {
            continue;
        };
        let manifest = PathBuf::from(manifest);
        let Some(dir) = manifest
            .parent()
            .and_then(|d| d.strip_prefix(&workspace).ok())
            .map(Path::to_path_buf)
        else {
            continue;
        };
        let bins = package["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin")))
            .filter(|t| {
                t["required-features"]
                    .as_array()
                    .is_none_or(|f| f.is_empty())
            })
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        packages.insert(
            dir,
            Package {
                name: name.to_owned(),
                manifest,
                bins,
            },
        );
    }
    Ok((PathBuf::from(target), packages))
}

fn cargo(root: &Path) -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(root);
    cmd
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is inside the workspace")
        .to_path_buf()
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let fail = |e: io::Error| format!("can't read {}: {e}", path.display());
    let mut file = File::open(path).map_err(fail)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(fail)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(format!("can't remove {}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Temp file, then rename, so a reader never sees half a file.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_108_800; // 2026-10-16

    #[test]
    fn statements_pass_the_daemons_checks() {
        let sha = "ab".repeat(32);
        for (_, lockers) in MEMBERS {
            let text = statement_text("inforno_gui", &sha, lockers, NOW, DEFAULT_DAYS).unwrap();
            assert!(text.ends_with('\n'));
            let checked = Statement::parse(&text).unwrap().check(NOW).unwrap();
            assert_eq!(checked.app, APP);
            assert_eq!(checked.sha256, sha);
            assert_eq!(checked.expires - checked.issued, DEFAULT_DAYS * DAY);
            assert_eq!(checked.lockers.len(), lockers.len());
        }
        let longest = statement_text("x", &sha, MEMBERS[0].1, NOW, MAX_STATEMENT_DAYS);
        assert!(longest.is_ok());
        let too_long = statement_text("x", &sha, MEMBERS[0].1, NOW, MAX_STATEMENT_DAYS + 1);
        assert!(too_long.is_err());
    }

    #[test]
    fn writers_get_their_lifetime() {
        let text = statement_text("inforno_gui", &"0".repeat(64), MEMBERS[0].1, NOW, 7).unwrap();
        let checked = Statement::parse(&text).unwrap().check(NOW).unwrap();
        assert!(checked.lockers["api_keys"].write);
        assert_eq!(checked.lockers["api_keys"].lifetime_secs, 30 * DAY);
    }
}
