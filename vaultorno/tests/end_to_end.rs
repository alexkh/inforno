//! The real `vaultorno` binary, over a real socket. Needs the development
//! build: `cargo test -p vaultorno --features test_identity`.

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vaultorno_client::testing::statement;
use vaultorno_client::time::{Clock, SystemClock};
use vaultorno_client::{DaemonOptions, Error, ExposeSecret, StatementFiles, Vault};

const BIN: &str = env!("CARGO_BIN_EXE_vaultorno");

/// The daemon process, killed when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(state: &Path, socket: &Path) -> Daemon {
    let child = Command::new(BIN)
        .args(["daemon", "--test-identity", "--state-dir"])
        .arg(state)
        .arg("--socket")
        .arg(socket)
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

fn open(
    socket: &Path,
    home: &Path,
    binary: &str,
    lockers: &[(&str, &str, Option<u32>)],
) -> Result<Vault, Error> {
    let now = SystemClock.now() - 60;
    Vault::connect(DaemonOptions {
        app: "e2e".into(),
        socket: socket.to_path_buf(),
        stream: None,
        home: Some(home.to_path_buf()),
        uid: None,
        statement: Some(StatementFiles {
            statement: statement("e2e", binary, now, lockers),
            signature: None,
        }),
    })
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
    let writer = open(&socket, &home, "writer", &[("api_keys", "write", Some(30))]).unwrap();
    assert!(writer.describe().contains("test identity"));
    let stored = writer.put("api_keys", SECRET, 0).unwrap();

    // A second process, read-only.
    let reader = open(&socket, &home, "reader", &[("api_keys", "read", None)]).unwrap();
    let got = reader.get("api_keys").unwrap();
    assert_eq!(got.data.expose_secret(), SECRET);
    assert_eq!(got.revision, stored.revision);
    assert!(matches!(
        reader.put("api_keys", b"x", got.revision),
        Err(Error::NotAllowed(_))
    ));

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
        reader.get("api_keys").unwrap().data.expose_secret(),
        SECRET,
        "live lockers survive the sweep"
    );
}

#[test]
fn a_daemon_restart_keeps_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (state, socket, home) = (
        dir.path().join("state"),
        dir.path().join("vaultorno.sock"),
        dir.path().join("home"),
    );
    {
        let _daemon = start(&state, &socket);
        open(&socket, &home, "writer", &[("api_keys", "write", None)])
            .unwrap()
            .put("api_keys", b"kept", 0)
            .unwrap();
    }
    let _daemon = start(&state, &socket);
    let vault = open(&socket, &home, "writer", &[("api_keys", "write", None)]).unwrap();
    assert_eq!(vault.get("api_keys").unwrap().data.expose_secret(), b"kept");
}

#[test]
fn the_daemon_refuses_to_serve_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("daemon")
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--socket")
        .arg(dir.path().join("s.sock"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!dir.path().join("s.sock").exists());
}

#[test]
fn later_modes_and_unknown_modes_fail_cleanly() {
    for mode in [
        "inspect", "init", "status", "check", "vaults", "log", "revoke", "forget",
    ] {
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
