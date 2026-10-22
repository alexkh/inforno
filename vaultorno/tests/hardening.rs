//! The §18 hardening checks (Milestone 4), on a real systemd.
//!
//! Each probe runs in a transient service (`systemd-run --wait --pipe`) with
//! the `[Service]` settings of the unit files in `dist/`, as written, so
//! what's checked is what ships, with nothing installed. Settings that set a
//! service up rather than confine it (`ExecStart=`, the standard streams,
//! `StateDirectory=`, `LoadCredentialEncrypted=`) are left out, so the probes
//! never touch `/var/lib/vaultorno`. The probe is this test binary, run again
//! with `VAULTORNO_PROBE` set.
//!
//! The `#[ignore]` tests need root, `/home`, and the two system users, which
//! they create from `dist/vaultorno.conf`. Build as yourself, run as root:
//!
//! ```sh
//! cargo test --release -p vaultorno --test hardening --no-run
//! sudo target/release/deps/hardening-<hash> --ignored --test-threads=1 --nocapture
//! ```
//!
//! The services run copies of this binary and `vaultorno`, which each run
//! puts in `/var/lib/vaultorno_hardening`, so the workspace may be anywhere,
//! even under `/home`, which `ProtectHome=` hides. Remove that folder when
//! you're done.

use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, PidfdFlags, pidfd_open};
use vaultorno::master_key::{
    CREDENTIAL_FILE, CREDENTIAL_NAME, CREDENTIALS_DIRECTORY, MasterKey, Sealing,
};
use vaultorno_client::protocol::DEFAULT_SOCKET;
use vaultorno_inspect::{DEFAULT_INSPECT_SOCKET, inspect_pidfd};

const BIN: &str = env!("CARGO_BIN_EXE_vaultorno");
const DIST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/dist");
const PROBE: &str = "VAULTORNO_PROBE";
/// Where `setup` copies this binary and `vaultorno` for the services to run.
const STAGE: &str = "/var/lib/vaultorno_hardening";
/// An ordinary user with nothing of its own.
const NOBODY: u32 = 65534;
const CAP_SYS_PTRACE: u64 = 1 << 19;
const NO_CAPS: &str = "0000000000000000";
const UNITS: [&str; 6] = [
    "vaultorno.socket",
    "vaultorno.service",
    "vaultorno_inspect.socket",
    "vaultorno_inspect.service",
    "vaultorno_sweep.service",
    "vaultorno_sweep.timer",
];
const SERVICES: [&str; 3] = [
    "vaultorno.service",
    "vaultorno_inspect.service",
    "vaultorno_sweep.service",
];
/// `[Service]` settings that set a service up rather than confine it.
const SETUP_ONLY: &[&str] = &[
    "ExecStart",
    "Type",
    "StandardInput",
    "StandardOutput",
    "StandardError",
    "StateDirectory",
    "StateDirectoryMode",
    "LoadCredentialEncrypted",
];

// ---------------------------------------------------------------------------
// The unit files

/// The `KEY=VALUE` lines of one section of a unit file in `dist/`.
fn section(unit: &str, name: &str) -> Vec<(String, String)> {
    let path = Path::new(DIST).join(unit);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut current = String::new();
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = header.to_owned();
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("{unit}: not KEY=VALUE: {line:?}"));
        if current == name {
            out.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    out
}

fn value<'a>(settings: &'a [(String, String)], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A service's settings that confine it, as `systemd-run -p` takes them.
fn sandbox(unit: &str) -> Vec<String> {
    section(unit, "Service")
        .into_iter()
        .filter(|(k, _)| !SETUP_ONLY.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

fn without(settings: &[String], key: &str) -> Vec<String> {
    settings
        .iter()
        .filter(|s| s.split_once('=').map(|(k, _)| k) != Some(key))
        .cloned()
        .collect()
}

/// The unit files and the code agree, and systemd reads them as meant.
#[test]
fn the_unit_files_say_what_the_code_expects() {
    for unit in UNITS {
        let text = fs::read_to_string(Path::new(DIST).join(unit)).unwrap();
        for line in text.lines() {
            // systemd has no comments after a setting: one would become
            // part of its value.
            assert!(
                line.trim_start().starts_with('#') || !line.contains('#'),
                "{unit}: {line:?}"
            );
        }
    }

    let socket = section("vaultorno.socket", "Socket");
    assert_eq!(value(&socket, "ListenStream"), Some(DEFAULT_SOCKET));
    assert_eq!(value(&socket, "SocketMode"), Some("0666"));
    let socket = section("vaultorno_inspect.socket", "Socket");
    assert_eq!(value(&socket, "ListenStream"), Some(DEFAULT_INSPECT_SOCKET));
    assert_eq!(value(&socket, "SocketUser"), Some("vaultorno_inspect"));
    assert_eq!(value(&socket, "SocketGroup"), Some("vaultorno"));
    assert_eq!(value(&socket, "SocketMode"), Some("0660"));

    let daemon = section("vaultorno.service", "Service");
    let inspector = section("vaultorno_inspect.service", "Service");
    let sweep = section("vaultorno_sweep.service", "Service");
    for (unit, s, mode) in [
        ("vaultorno.service", &daemon, "daemon"),
        ("vaultorno_inspect.service", &inspector, "inspect"),
    ] {
        let exec = format!("/usr/bin/vaultorno {mode}");
        assert_eq!(value(s, "ExecStart"), Some(exec.as_str()), "{unit}");
        assert_eq!(value(s, "StandardInput"), Some("socket"), "{unit}");
        assert_eq!(value(s, "StandardOutput"), Some("journal"), "{unit}");
        assert_eq!(value(s, "StandardError"), Some("journal"), "{unit}");
    }
    assert_eq!(value(&daemon, "User"), Some("vaultorno"));
    assert_eq!(value(&daemon, "CapabilityBoundingSet"), Some(""));
    let credential = format!("{CREDENTIAL_NAME}:/var/lib/vaultorno/{CREDENTIAL_FILE}");
    assert_eq!(
        value(&daemon, "LoadCredentialEncrypted"),
        Some(credential.as_str())
    );
    assert_eq!(value(&inspector, "User"), Some("vaultorno_inspect"));
    assert_eq!(
        value(&inspector, "CapabilityBoundingSet"),
        Some("CAP_SYS_PTRACE")
    );
    // tests/hardening.rs checks it still hashes binaries under /home.
    assert_eq!(value(&inspector, "ProtectHome"), Some("yes"));
    assert_eq!(value(&sweep, "User"), Some("vaultorno"));
    assert!(
        value(&sweep, "LoadCredentialEncrypted").is_none(),
        "the sweep needs no key"
    );

    // The daemon can't identify anyone without the inspector's socket.
    let unit = section("vaultorno.service", "Unit");
    for key in ["Requires", "After"] {
        assert!(
            value(&unit, key)
                .is_some_and(|v| v.split_whitespace().any(|u| u == "vaultorno_inspect.socket")),
            "vaultorno.service {key}="
        );
    }

    let users = fs::read_to_string(Path::new(DIST).join("vaultorno.conf")).unwrap();
    assert!(users.lines().any(|l| {
        l.split_whitespace().take(2).eq(["u", "vaultorno"]) && l.contains("/var/lib/vaultorno")
    }));
    assert!(
        users
            .lines()
            .any(|l| l.split_whitespace().take(2).eq(["u", "vaultorno_inspect"]))
    );
}

// ---------------------------------------------------------------------------
// Probes

/// Not a test: what the probes run inside their transient services.
#[test]
#[ignore]
fn probe() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let found = match spec.split(' ').collect::<Vec<_>>().as_slice() {
        ["caps"] => caps(),
        ["read", path] => outcome(fs::read(path).map(|b| b.len())),
        ["open_write", path] => {
            outcome(fs::OpenOptions::new().write(true).open(path).map(|_| 0))
        }
        ["inspect", pid] => inspect(pid),
        _ => format!("unknown probe {spec:?}"),
    };
    // On a line of its own: with --nocapture, libtest's "test probe ... "
    // has no newline after it.
    println!("\nprobe: {found}");
}

/// `CapInh CapPrm CapEff CapBnd CapAmb` of this process.
fn caps() -> String {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"]
        .map(|field| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
                .map_or("?", str::trim)
                .to_owned()
        })
        .join(" ")
}

/// The real inspector's code, as `vaultorno inspect` runs it.
fn inspect(pid: &str) -> String {
    let Some(pid) = pid.parse::<i32>().ok().and_then(Pid::from_raw) else {
        return format!("bad pid {pid:?}");
    };
    match pidfd_open(pid, PidfdFlags::empty()) {
        Err(e) => errno(e),
        Ok(fd) => match inspect_pidfd(fd.as_fd()) {
            Ok(found) => format!("ok {}", found.exe_path.display()),
            Err(why) => format!("refused {why}"),
        },
    }
}

fn outcome(result: io::Result<usize>) -> String {
    match result {
        Ok(n) => format!("ok {n}"),
        Err(e) => format!("err {}", e.raw_os_error().unwrap_or(-1)),
    }
}

fn errno(e: Errno) -> String {
    format!("err {}", e.raw_os_error())
}

/// Runs `probe` in a transient service with `settings`; what it found.
fn run_probe(settings: &[String], probe: &str) -> String {
    let exe = staged("hardening");
    let mut cmd = Command::new("systemd-run");
    cmd.args(["--wait", "--pipe", "--quiet", "--collect"]);
    for s in settings {
        cmd.arg("-p").arg(s);
    }
    let out = cmd
        .arg(format!("--setenv={PROBE}={probe}"))
        .arg("--")
        .arg(&exe)
        .args(["--exact", "probe", "--ignored", "--nocapture", "--test-threads=1"])
        .stdin(Stdio::null())
        .output()
        .expect("can't run systemd-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|l| l.split_once("probe: ").map(|(_, found)| found.trim().to_owned()))
        .unwrap_or_else(|| {
            panic!(
                "probe {probe:?} said nothing ({}):\n{stdout}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

// ---------------------------------------------------------------------------
// Set-up

fn setup() {
    assert!(
        rustix::process::geteuid().is_root(),
        "the hardening tests need root"
    );
    let conf = Path::new(DIST).join("vaultorno.conf");
    let out = Command::new("systemd-sysusers").arg(&conf).output().unwrap();
    assert!(
        out.status.success(),
        "systemd-sysusers {}: {}",
        conf.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    static STAGED: OnceLock<()> = OnceLock::new();
    STAGED.get_or_init(stage);
}

/// Copies this binary and `vaultorno` into `STAGE`, owned by root and
/// `0755`, so the services can run them wherever the workspace is:
/// `ProtectHome=` hides `/home`, and a private folder would keep other
/// users out.
fn stage() {
    let _ = fs::remove_dir_all(STAGE);
    fs::create_dir(STAGE).unwrap_or_else(|e| panic!("can't create {STAGE}: {e}"));
    fs::set_permissions(STAGE, fs::Permissions::from_mode(0o755)).unwrap();
    let exe = std::env::current_exe().unwrap();
    for (from, name) in [(exe.as_path(), "hardening"), (Path::new(BIN), "vaultorno")] {
        let to = staged(name);
        fs::copy(from, &to).unwrap_or_else(|e| {
            panic!("can't copy {} to {}: {e}", from.display(), to.display())
        });
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn staged(name: &str) -> PathBuf {
    Path::new(STAGE).join(name)
}

/// A process, killed when dropped.
struct Victim(Child);

impl Victim {
    fn spawn(cmd: &mut Command) -> Victim {
        Victim(cmd.stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap())
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Victim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A folder made for one test, removed when dropped.
struct TempUnder(PathBuf);

impl TempUnder {
    fn new(parent: &str, name: &str, owner: u32, mode: u32) -> TempUnder {
        let path = Path::new(parent).join(format!("{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap_or_else(|e| panic!("can't create {}: {e}", path.display()));
        chown(&path, Some(owner), Some(owner)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        TempUnder(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempUnder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} didn't happen");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Names, sizes and change times in `dir`, and of `dir` itself.
fn snapshot(dir: &Path) -> Vec<(String, u64, i64, i64)> {
    let entry = |name: String, m: fs::Metadata| (name, m.len(), m.mtime(), m.mtime_nsec());
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            entry(e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap())
        })
        .collect();
    out.push(entry(".".into(), fs::metadata(dir).unwrap()));
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The checks (§18)

#[test]
#[ignore]
fn services_hold_only_the_capabilities_they_need() {
    setup();
    for unit in ["vaultorno.service", "vaultorno_sweep.service"] {
        assert_eq!(
            run_probe(&sandbox(unit), "caps"),
            [NO_CAPS; 5].join(" "),
            "{unit}: CapInh CapPrm CapEff CapBnd CapAmb"
        );
    }
    let ptrace = format!("{CAP_SYS_PTRACE:016x}");
    let found = run_probe(&sandbox("vaultorno_inspect.service"), "caps");
    let caps: Vec<&str> = found.split(' ').collect();
    assert!(
        caps.len() == 5
            && caps[1..].iter().all(|c| *c == ptrace)
            && (caps[0] == NO_CAPS || caps[0] == ptrace),
        "the inspector holds CAP_SYS_PTRACE and nothing else: {found}"
    );
}

/// The real inspector's code, in the real sandbox, on an app run by another
/// user. Its environment is in `/proc/<pid>/environ`, mode 0400 and owned by
/// that user; `CAP_SYS_PTRACE` doesn't override file permissions.
#[test]
#[ignore]
fn the_inspector_inspects_another_users_process() {
    setup();
    let app = Victim::spawn(
        Command::new("/usr/bin/sleep")
            .arg("300")
            .uid(NOBODY)
            .gid(NOBODY),
    );
    let found = run_probe(
        &sandbox("vaultorno_inspect.service"),
        &format!("inspect {}", app.pid()),
    );
    assert_eq!(found, "ok /usr/bin/sleep");
}

#[test]
#[ignore]
fn the_inspector_cant_write_through_proc_mem() {
    setup();
    let inspector = sandbox("vaultorno_inspect.service");
    // Its own memory: only ReadOnlyPaths=/proc stands in the way.
    assert_eq!(
        run_probe(&inspector, "open_write /proc/self/mem"),
        errno(Errno::ROFS)
    );
    // An app's.
    let app = Victim::spawn(
        Command::new("/usr/bin/sleep")
            .arg("300")
            .uid(NOBODY)
            .gid(NOBODY),
    );
    let mem = format!("/proc/{}/mem", app.pid());
    // The file's permissions (0600, the app's user's) stop it, with or
    // without ReadOnlyPaths=/proc (§18). CAP_SYS_PTRACE passes the ptrace
    // check, never the permissions: the inspector can't read an app's
    // memory either.
    for settings in [inspector.clone(), without(&inspector, "ReadOnlyPaths")] {
        for op in ["open_write", "read"] {
            assert_eq!(
                run_probe(&settings, &format!("{op} {mem}")),
                errno(Errno::ACCESS),
                "{op} {mem}"
            );
        }
    }
}

#[test]
#[ignore]
fn the_inspector_hashes_binaries_under_home() {
    setup();
    let dir = TempUnder::new("/home", "vaultorno_probe_exe", 0, 0o755);
    let bin = dir.path().join("sleep");
    fs::copy("/usr/bin/sleep", &bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let app = Victim::spawn(Command::new(&bin).arg("300").uid(NOBODY).gid(NOBODY));
    let read_exe = format!("read /proc/{}/exe", app.pid());
    let size = fs::metadata(&bin).unwrap().len();
    let inspector = sandbox("vaultorno_inspect.service");
    assert_eq!(
        run_probe(&inspector, &read_exe),
        format!("ok {size}"),
        "with the unit's ProtectHome=yes"
    );
}

#[test]
#[ignore]
fn no_service_reaches_a_users_half_through_proc() {
    setup();
    let home = TempUnder::new("/home", "vaultorno_probe_half", NOBODY, 0o700);
    let half = home.path().join("user_half");
    fs::write(&half, [0x42u8; 32]).unwrap();
    chown(&half, Some(NOBODY), Some(NOBODY)).unwrap();
    fs::set_permissions(&half, fs::Permissions::from_mode(0o600)).unwrap();

    // An app of the user's: its working folder holds the half, and it keeps
    // the half open as descriptor 3.
    let app = Victim::spawn(
        Command::new("/bin/sh")
            .args(["-c", "exec 3<user_half; exec sleep 300"])
            .current_dir(home.path())
            .uid(NOBODY)
            .gid(NOBODY),
    );
    let pid = app.pid();
    let fd3 = format!("/proc/{pid}/fd/3");
    wait_for("the app opening its half", || Path::new(&fd3).exists());
    let routes = [
        format!("/proc/{pid}/cwd/user_half"),
        format!("/proc/{pid}/root{}", half.display()),
        fd3.clone(),
        half.display().to_string(),
    ];
    // Root can, so the routes are real.
    for route in &routes {
        assert_eq!(fs::read(route).unwrap().len(), 32, "{route}");
    }
    for unit in SERVICES {
        let settings = sandbox(unit);
        for route in &routes {
            let found = run_probe(&settings, &format!("read {route}"));
            println!("{unit}: {route}: {found}");
            assert!(found.starts_with("err "), "{unit} read {route}: {found}");
        }
    }
}

#[test]
#[ignore]
fn running_any_mode_by_hand_gains_nothing() {
    setup();
    // No privileges in the file itself: they come only from the units (§3).
    let meta = fs::metadata(BIN).unwrap();
    assert_eq!(meta.mode() & 0o6000, 0, "setuid or setgid bit on {BIN}");
    let mut buf = [0u8; 64];
    match rustix::fs::getxattr(BIN, "security.capability", &mut buf[..]) {
        Err(e) if e == Errno::NODATA || e == Errno::OPNOTSUPP => {}
        other => panic!("{BIN} has file capabilities: {other:?}"),
    }

    // Root's state folder, as `vaultorno init` leaves it.
    let state = TempUnder::new("/var/tmp", "vaultorno_probe_state", 0, 0o700);
    fs::write(state.path().join(CREDENTIAL_FILE), b"sealed earlier").unwrap();
    fs::write(state.path().join("revoked_keys"), b"# none\n").unwrap();
    let before = snapshot(state.path());

    // nobody's own unsealed key, so the daemon gets past its master key.
    let creds = TempUnder::new("/var/tmp", "vaultorno_probe_creds", NOBODY, 0o700);
    let key_file = creds.path().join(CREDENTIAL_NAME);
    let key = MasterKey::new(&[0x5a; 32], Sealing::Unsealed);
    fs::write(&key_file, key.to_credential().as_slice()).unwrap();
    chown(&key_file, Some(NOBODY), Some(NOBODY)).unwrap();
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();

    let state_arg = state.path().to_str().unwrap();
    let key_line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAGf35LBmeMKO1jMA9oUql/3dGOKq+QS9ISbJkDJzoRu probe";
    let modes: [&[&str]; 5] = [
        &["daemon", "--state-dir", state_arg],
        &["inspect", "--state-dir", state_arg],
        &["sweep", "--state-dir", state_arg],
        &["init", "--state-dir", state_arg],
        &["revoke", "--state-dir", state_arg, key_line],
    ];
    for args in modes {
        let out = Command::new(staged("vaultorno"))
            .args(args)
            .env_clear()
            .env(CREDENTIALS_DIRECTORY, creds.path())
            .uid(NOBODY)
            .gid(NOBODY)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        println!("{}: {}", args[0], err.trim());
        assert!(!out.status.success(), "{args:?} succeeded as nobody");
        if matches!(args[0], "daemon" | "inspect") {
            assert!(err.contains("no socket from systemd"), "{args:?}: {err}");
        }
        assert_eq!(
            snapshot(state.path()),
            before,
            "{args:?} changed root's state folder"
        );
    }
}
