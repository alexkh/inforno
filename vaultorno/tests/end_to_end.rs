//! The real `vaultorno` binary, over a real socket. Needs the development
//! build: `cargo test -p vaultorno --features test_identity`.

#![cfg_attr(not(feature = "test_identity"), allow(dead_code, unused_imports))]

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vaultorno::master_key::{CREDENTIALS_DIRECTORY, MasterKey, Sealing};

const BIN: &str = env!("CARGO_BIN_EXE_vaultorno");
/// A client in a process of its own (`tests/support/e2e_client.rs`).
#[cfg(feature = "test_identity")]
const CLIENT: &str = env!("CARGO_BIN_EXE_vaultorno_e2e_client");

/// The daemon process, killed when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What systemd would give the daemon: a folder holding the unsealed master
/// key (§6), here labelled `unsealed`. The same `state` gets the same key.
fn credentials(state: &Path) -> PathBuf {
    let dir = state.with_file_name("credentials");
    let file = dir.join("master_key");
    if !file.exists() {
        fs::create_dir_all(&dir).unwrap();
        let key = MasterKey::new(&[0x5a; 32], Sealing::Unsealed);
        fs::write(&file, key.to_credential().as_slice()).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir
}

fn start(state: &Path, socket: &Path) -> Daemon {
    let child = Command::new(BIN)
        .args(["daemon", "--test-identity", "--state-dir"])
        .arg(state)
        .arg("--socket")
        .arg(socket)
        // Never the machine's inspector, whatever runs in /run.
        .arg("--in-process-inspector")
        .env(CREDENTIALS_DIRECTORY, credentials(state))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = Daemon(child);
    // Wait for a connection to succeed, not just for the socket file: a
    // previous daemon's file may still be there until the new one replaces it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "the daemon didn't start");
        thread::sleep(Duration::from_millis(20));
    }
    daemon
}

/// Runs one client operation in a process of its own with an empty
/// environment, as `binary` with `lockers` (`name:read` or
/// `name:write[:days]`). Cargo sets `LD_LIBRARY_PATH` for tests, and the
/// daemon refuses a caller with any variable on its reject list (§7.5), so
/// the clients can't be this test process. `Err` carries a refusal.
#[cfg(feature = "test_identity")]
fn client(
    socket: &Path,
    home: &Path,
    binary: &str,
    lockers: &[&str],
    op: &[&str],
) -> Result<String, String> {
    let out = Command::new(CLIENT)
        .env_clear()
        .arg("--socket")
        .arg(socket)
        .arg("--home")
        .arg(home)
        .args(["--binary", binary])
        .args(lockers.iter().flat_map(|l| ["--locker", *l]))
        .arg("--")
        .args(op)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    match out.status.code() {
        Some(0) => Ok(stdout),
        Some(1) => Err(stdout),
        _ => panic!(
            "client {op:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[cfg(feature = "test_identity")]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A `get`'s output, `<revision>:<hex>`, as the revision and the data.
#[cfg(feature = "test_identity")]
fn parse_get(out: &str) -> (String, Vec<u8>) {
    let (revision, data) = out
        .split_once(':')
        .unwrap_or_else(|| panic!("not a get: {out:?}"));
    let data = (0..data.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&data[i..i + 2], 16).unwrap())
        .collect();
    (revision.to_owned(), data)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_under(&path))
        } else {
            out.push(path)
        }
    }
    out
}

#[cfg(feature = "test_identity")]
#[test]
fn round_trip_through_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let (state, socket, home) = (
        dir.path().join("state"),
        dir.path().join("vaultorno.sock"),
        dir.path().join("home"),
    );
    let _daemon = start(&state, &socket);

    let meta = fs::symlink_metadata(&socket).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o666,
        "access is decided per connection"
    );

    const SECRET: &[u8] = b"sk-e2e-canary-0b5e2c";
    let writer = |op: &[&str]| client(&socket, &home, "writer", &["api_keys:write:30"], op);
    let reader = |op: &[&str]| client(&socket, &home, "reader", &["api_keys:read"], op);
    assert!(writer(&["describe"]).unwrap().contains("test identity"));
    let stored = writer(&["put", "api_keys", hex(SECRET).as_str(), "0"]).unwrap();
    let revision = stored.strip_prefix("stored ").unwrap();

    // Another process, read-only.
    let (got_revision, got) = parse_get(&reader(&["get", "api_keys"]).unwrap());
    assert_eq!(got, SECRET);
    assert_eq!(got_revision, revision);
    let refused = reader(&["put", "api_keys", hex(b"x").as_str(), revision]).unwrap_err();
    assert!(refused.starts_with("NotAllowed"), "{refused}");

    // The state is ciphertext only, under the real uid.
    let uid = rustix::process::getuid().as_raw();
    assert!(
        state
            .join(format!("users/{uid}/test_e2e/lockers/api_keys"))
            .exists()
    );
    for file in files_under(&state) {
        let bytes = fs::read(&file).unwrap();
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET),
            "plaintext in {}",
            file.display()
        );
    }

    // The sweep runs against the same state, alongside the daemon.
    let out = Command::new(BIN)
        .arg("sweep")
        .arg("--state-dir")
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("swept:"));
    assert_eq!(
        parse_get(&reader(&["get", "api_keys"]).unwrap()).1,
        SECRET,
        "live lockers survive the sweep"
    );
}

#[cfg(feature = "test_identity")]
#[test]
fn a_daemon_restart_keeps_the_vault() {
    const WRITER: &[&str] = &["api_keys:write"];
    let dir = tempfile::tempdir().unwrap();
    let (state, socket, home) = (
        dir.path().join("state"),
        dir.path().join("vaultorno.sock"),
        dir.path().join("home"),
    );
    {
        let _daemon = start(&state, &socket);
        client(
            &socket,
            &home,
            "writer",
            WRITER,
            &["put", "api_keys", hex(b"kept").as_str(), "0"],
        )
        .unwrap();
    }
    let _daemon = start(&state, &socket);
    let got = client(&socket, &home, "writer", WRITER, &["get", "api_keys"]).unwrap();
    assert_eq!(parse_get(&got).1, b"kept");
}

#[cfg(feature = "test_identity")]
#[test]
fn real_inspect_mode_over_socket() {
    use std::os::fd::AsFd;
    use rustix::process::{PidfdFlags, pidfd_open};

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("inspect.sock");
    // The daemon's user is whoever owns its state folder (§3): here, us.
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    let inspector = Daemon(
        Command::new(BIN)
            .args(["inspect", "--socket"])
            .arg(&sock)
            .arg("--state-dir")
            .arg(&state)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        assert!(Instant::now() < deadline, "inspector didn't start");
        thread::sleep(Duration::from_millis(20));
    }

    // Inspect self (which is fully readable by the unprivileged test runner user).
    let pid = rustix::process::getpid();
    let pidfd = pidfd_open(pid, PidfdFlags::empty()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let info = vaultorno_inspect::request_inspection(&sock, pidfd.as_fd(), uid).unwrap();
    assert_eq!(info.uid, uid);
    assert_eq!(info.exe_path, std::env::current_exe().unwrap());
    drop(inspector);
}

#[test]
fn later_modes_and_unknown_modes_fail_cleanly() {
    for mode in ["status", "check", "vaults", "log", "forget"] {
        let out = Command::new(BIN).arg(mode).output().unwrap();
        assert!(!out.status.success(), "{mode}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("later milestone"),
            "{mode}"
        );
    }
    assert_eq!(
        Command::new(BIN)
            .arg("bogus")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(
        Command::new(BIN)
            .arg("help")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn revoke_mode_appends_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAGf35LBmeMKO1jMA9oUql/3dGOKq+QS9ISbJkDJzoRu inforno_release_2026";
    let fingerprint = "SHA256:er8u9VfNuR0xkLRM0QgzXCYQOcmcBhHaj1lAkDj0+fA";
    let revoke = |args: &[&str]| {
        Command::new(BIN)
            .arg("revoke")
            .arg("--state-dir")
            .arg(dir.path())
            .args(args)
            .output()
            .unwrap()
    };
    let list = || fs::read_to_string(dir.path().join("revoked_keys")).unwrap_or_default();

    // Unquoted words make up the key line.
    let out = revoke(&key.split(' ').collect::<Vec<_>>());
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(fingerprint));
    assert!(list().contains(key));

    // A .pub file works too, and revoking twice changes nothing.
    let pub_file = dir.path().join("release.pub");
    fs::write(&pub_file, format!("{key}\n")).unwrap();
    let out = revoke(&[pub_file.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("already revoked"));
    assert_eq!(list().matches("ssh-ed25519").count(), 1);

    let out = revoke(&["--undo", pub_file.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(!list().contains("ssh-ed25519"));

    assert!(!revoke(&["not", "a", "key"]).status.success());
}

#[test]
fn a_daemon_without_its_master_key_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let run = |credentials: Option<&Path>| {
        let mut cmd = Command::new(BIN);
        cmd.arg("daemon").arg("--state-dir").arg(&state);
        match credentials {
            Some(dir) => cmd.env(CREDENTIALS_DIRECTORY, dir),
            None => cmd.env_remove(CREDENTIALS_DIRECTORY),
        };
        cmd.output().unwrap()
    };

    let out = run(None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(CREDENTIALS_DIRECTORY));
    assert!(!state.exists(), "nothing touched");

    // A key file others may read isn't one systemd would have made.
    let creds = credentials(&state);
    fs::set_permissions(creds.join("master_key"), fs::Permissions::from_mode(0o644)).unwrap();
    let out = run(Some(&creds));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("others must have no access"));
    assert!(!state.exists());
}

#[test]
fn release_modes_take_their_sockets_only_from_systemd() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixListener;

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let creds = credentials(&state);
    let run = |mode: &str, stdin: Stdio, extra: &[&str]| {
        Command::new(BIN)
            .arg(mode)
            .arg("--state-dir")
            .arg(&state)
            .args(extra)
            .env(CREDENTIALS_DIRECTORY, &creds)
            .stdin(stdin)
            .output()
            .unwrap()
    };
    let refused = |out: &std::process::Output, why: &str| {
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains(why), "{err}");
    };

    // Not started by systemd: standard input isn't a socket.
    refused(&run("daemon", Stdio::null(), &[]), "no socket from systemd");
    refused(&run("inspect", Stdio::null(), &[]), "no socket from systemd");

    // A listening socket, but not where clients connect.
    let elsewhere = dir.path().join("elsewhere.sock");
    let listener = OwnedFd::from(UnixListener::bind(&elsewhere).unwrap());
    let out = run("daemon", Stdio::from(listener.try_clone().unwrap()), &[]);
    refused(&out, "not /run/vaultorno.sock");
    refused(
        &run("inspect", Stdio::from(listener), &[]),
        "not /run/vaultorno_inspect.sock",
    );
    assert!(!state.exists(), "refused before touching the state");

    // Only a development build binds a socket of its own.
    #[cfg(not(feature = "test_identity"))]
    refused(
        &run("daemon", Stdio::null(), &["--socket", elsewhere.to_str().unwrap()]),
        "unknown option",
    );
}

#[test]
fn init_never_replaces_a_master_key() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("master_key.cred"), b"sealed earlier").unwrap();
    let out = Command::new(BIN)
        .args(["init", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
    assert_eq!(
        fs::read(state.join("master_key.cred")).unwrap(),
        b"sealed earlier"
    );
    assert!(
        !state.join("CACHEDIR.TAG").exists(),
        "refused before touching anything"
    );
}

/// Needs root and `systemd-creds`; as root, run
/// `cargo test --release -p vaultorno --test end_to_end -- --ignored real_init`.
#[test]
#[ignore]
fn real_init_seals_a_master_key_that_unseals() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let init = || {
        Command::new(BIN)
            .args(["init", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap()
    };
    let out = init();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(
        fs::read_to_string(state.join("CACHEDIR.TAG"))
            .unwrap()
            .starts_with("Signature: 8a477f597d28d172789f06886806bc55")
    );
    let cred = state.join("master_key.cred");
    assert_eq!(
        fs::metadata(&cred).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let plain = Command::new("/usr/bin/systemd-creds")
        .args(["decrypt", "--name=master_key"])
        .arg(&cred)
        .arg("-")
        .output()
        .unwrap();
    assert!(plain.status.success());
    let key = MasterKey::from_credential(&plain.stdout).unwrap();
    assert_ne!(key.sealing(), Sealing::Unsealed);
    println!("sealed with {}", key.sealing().describe());

    assert!(!init().status.success(), "a second init refuses");
}
