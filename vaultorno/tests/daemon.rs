//! The daemon, in-process: the shared backend suite, plus the Milestone 1
//! checks only the daemon has (design doc §16, §17).

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tempfile::TempDir;
use vaultorno::identity::{Peer, TestIdentity};
use vaultorno::server::{MAX_CONNECTIONS_PER_USER, MAX_VAULTS_PER_USER, Server};
use vaultorno::store::Store;
use vaultorno::sweep::{SweepReport, sweep};
use vaultorno_client::protocol::{
    ENVELOPE_MAGIC, ErrorCode, Frame, MAX_FRAME, PROTOCOL, Request, Response, read_frame,
    write_frame,
};
use vaultorno_client::testing::{
    APP, Harness, START, reader_statement, statement, writer_statement,
};
use vaultorno_client::time::{Clock, DAY, FakeClock};
use vaultorno_client::{DaemonOptions, Error, ExposeSecret, StatementFiles, Vault};

// ---------------------------------------------------------------------------
// Test environment

struct Env {
    server: Arc<Server>,
    clock: Arc<FakeClock>,
    _state: TempDir,
    homes: TempDir,
}

fn env() -> Env {
    let state = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(START));
    let store = Store::open(state.path().join("state")).unwrap();
    let server = Server::new(store, Box::new(TestIdentity), clock.clone());
    Env {
        server,
        clock,
        _state: state,
        homes: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    fn home(&self, uid: u32) -> PathBuf {
        self.homes.path().join(uid.to_string())
    }

    fn store(&self) -> &Store {
        self.server.store()
    }

    /// A connection to the in-process daemon, as if from `uid`.
    fn stream(&self, uid: u32) -> UnixStream {
        let (client, daemon) = UnixStream::pair().unwrap();
        let server = self.server.clone();
        thread::spawn(move || server.serve(daemon, Peer::without_pidfd(uid, None)));
        client
    }

    fn open_with(
        &self,
        uid: u32,
        stream: UnixStream,
        app: &str,
        statement: String,
    ) -> Result<Vault, Error> {
        Vault::connect(DaemonOptions {
            app: app.into(),
            socket: PathBuf::from("/nonexistent"),
            stream: Some(stream),
            home: Some(self.home(uid)),
            uid: Some(uid),
            statement: Some(StatementFiles {
                statement,
                signature: None,
            }),
        })
    }

    fn open(&self, uid: u32, statement: String) -> Result<Vault, Error> {
        self.open_with(uid, self.stream(uid), APP, statement)
    }

    fn open_app(&self, uid: u32, app: &str, statement: String) -> Result<Vault, Error> {
        self.open_with(uid, self.stream(uid), app, statement)
    }

    fn locker_file(&self, uid: u32, locker: &str) -> PathBuf {
        self.store().locker_path(uid, "test_suite", locker)
    }
}

const UID: u32 = 1000;

fn make_harness() -> Harness {
    let env = env();
    Harness {
        writer: env.open(UID, writer_statement(START)).unwrap(),
        writer2: env.open(UID, writer_statement(START)).unwrap(),
        reader: env.open(UID, reader_statement(START)).unwrap(),
        clock: env.clock.clone(),
        keep: Box::new(env),
    }
}

vaultorno_client::backend_suite!(make_harness);

/// A client that speaks the protocol directly, to send what
/// `vaultorno_client` never would.
struct Raw {
    stream: UnixStream,
    id: u32,
}

impl Raw {
    fn new(stream: UnixStream) -> Raw {
        Raw { stream, id: 0 }
    }

    fn send(&mut self, body: Request) -> Option<Response> {
        self.id += 1;
        write_frame(&mut self.stream, &Frame { id: self.id, body }).ok()?;
        let frame: Frame<Response> = read_frame(&mut self.stream).ok()??;
        assert_eq!(frame.id, self.id);
        Some(frame.body)
    }

    fn hello(&mut self, statement: String) -> Response {
        self.send(Request::Hello {
            protocol: PROTOCOL,
            statement: Some(statement),
            signature: None,
            succession: None,
        })
        .expect("a reply to hello")
    }
}

fn code(r: &Option<Response>) -> Option<ErrorCode> {
    match r {
        Some(Response::Error { code, .. }) => Some(*code),
        _ => None,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// A valid-looking envelope: right magic, right size, not really sealed.
fn fake_envelope(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    v[..4].copy_from_slice(&ENVELOPE_MAGIC);
    v
}

// ---------------------------------------------------------------------------
// Per-user separation

#[test]
fn users_never_see_each_other() {
    let env = env();
    let a = env.open(1000, writer_statement(START)).unwrap();
    let b = env.open(1001, writer_statement(START)).unwrap();
    a.put("api_keys", b"user 1000", 0).unwrap();
    assert_eq!(
        b.get("api_keys").unwrap().revision,
        0,
        "1001 sees 1000's locker"
    );
    b.put("api_keys", b"user 1001", 0).unwrap();
    assert_eq!(
        a.get("api_keys").unwrap().data.expose_secret(),
        b"user 1000"
    );
    assert_eq!(
        b.get("api_keys").unwrap().data.expose_secret(),
        b"user 1001"
    );

    let half =
        |uid| fs::read(env.store().vault_dir(uid, "test_suite").join("daemon_half")).unwrap();
    assert_ne!(half(1000), half(1001), "each user's vault has its own half");
}

#[test]
fn a_locker_copied_to_another_user_fails_to_open() {
    let env = env();
    let a = env.open(1000, writer_statement(START)).unwrap();
    a.put("api_keys", b"user 1000", 0).unwrap();
    let b = env.open(1001, writer_statement(START)).unwrap();
    let target = env.locker_file(1001, "api_keys");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(env.locker_file(1000, "api_keys"), &target).unwrap();
    assert!(matches!(b.get("api_keys"), Err(Error::KeyMismatch)));
}

// ---------------------------------------------------------------------------
// Blindness

#[test]
fn no_plaintext_canary_reaches_the_daemon() {
    const CANARY: &[u8] = b"CANARY-7f3a9c1e-must-never-reach-the-daemon";
    let env = env();

    // A recording proxy between the client and the daemon.
    let (client, proxy_client) = UnixStream::pair().unwrap();
    let (proxy_daemon, daemon) = UnixStream::pair().unwrap();
    let server = env.server.clone();
    thread::spawn(move || server.serve(daemon, Peer::without_pidfd(UID, None)));
    let up = Arc::new(Mutex::new(Vec::new()));
    let down = Arc::new(Mutex::new(Vec::new()));
    let pumps = [
        pump(
            proxy_client.try_clone().unwrap(),
            proxy_daemon.try_clone().unwrap(),
            up.clone(),
        ),
        pump(proxy_daemon, proxy_client, down.clone()),
    ];

    let vault = env
        .open_with(UID, client, APP, writer_statement(START))
        .unwrap();
    let rev = vault.put("api_keys", CANARY, 0).unwrap().revision;
    vault
        .update("api_keys", |old| Ok([old, b"+more"].concat()))
        .unwrap();
    let _ = vault.put("short", &[CANARY; 100].concat(), 0).unwrap();
    assert!(contains(
        vault.get("api_keys").unwrap().data.expose_secret(),
        CANARY
    ));
    assert_ne!(rev, 0);
    vault.list().unwrap();
    drop(vault);
    for p in pumps {
        p.join().unwrap();
    }

    for (direction, bytes) in [("to the daemon", &up), ("from the daemon", &down)] {
        let bytes = bytes.lock().unwrap();
        assert!(!bytes.is_empty());
        assert!(
            !contains(&bytes, CANARY),
            "canary in raw frames {direction}"
        );
        for decoded in decoded_strings(&bytes) {
            assert!(
                !contains(&decoded, CANARY),
                "canary in a base64 field {direction}"
            );
        }
    }
    for file in files_under(env.store().root()) {
        assert!(
            !contains(&fs::read(&file).unwrap(), CANARY),
            "canary in {}",
            file.display()
        );
    }
}

fn pump(mut from: UnixStream, mut to: UnixStream, seen: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = to.shutdown(Shutdown::Write);
                    break;
                }
                Ok(n) => {
                    seen.lock().unwrap().extend_from_slice(&buf[..n]);
                    if to.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// Every string in every frame, base64-decoded where it decodes.
fn decoded_strings(mut bytes: &[u8]) -> Vec<Vec<u8>> {
    fn walk(v: &serde_json::Value, out: &mut Vec<Vec<u8>>) {
        match v {
            serde_json::Value::String(s) => {
                if let Ok(d) = STANDARD.decode(s) {
                    out.push(d);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        let value: serde_json::Value = serde_json::from_slice(&bytes[4..4 + len]).unwrap();
        walk(&value, &mut out);
        bytes = &bytes[4 + len..];
    }
    out
}

#[test]
fn the_shape_check_refuses_plaintext() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    assert!(matches!(
        raw.hello(writer_statement(START)),
        Response::Welcome { .. }
    ));
    let put = |raw: &mut Raw, data: Vec<u8>| {
        raw.send(Request::Put {
            locker: "api_keys".into(),
            data,
            expected: 0,
        })
    };

    assert_eq!(
        code(&put(&mut raw, b"sk-plaintext-api-key".to_vec())),
        Some(ErrorCode::NotSealed)
    );
    assert_eq!(
        code(&put(&mut raw, fake_envelope(1500))),
        Some(ErrorCode::NotSealed),
        "not a power of two"
    );
    assert_eq!(
        code(&put(&mut raw, fake_envelope(512))),
        Some(ErrorCode::NotSealed),
        "below 1 KiB"
    );
    let mut wrong_magic = fake_envelope(2048);
    wrong_magic[..4].copy_from_slice(b"JSON");
    assert_eq!(
        code(&put(&mut raw, wrong_magic)),
        Some(ErrorCode::NotSealed)
    );
    assert_eq!(
        code(&put(&mut raw, fake_envelope(300_000))),
        Some(ErrorCode::TooLarge)
    );
    assert!(!env.locker_file(UID, "api_keys").exists());

    // The shape check isn't a decryption: a right-shaped envelope is stored.
    assert!(matches!(
        put(&mut raw, fake_envelope(1024)),
        Some(Response::Stored { .. })
    ));
}

// ---------------------------------------------------------------------------
// Protocol rules

#[test]
fn the_first_request_must_be_hello() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    assert_eq!(
        code(&raw.send(Request::List)),
        Some(ErrorCode::ProtocolMismatch)
    );
    assert!(raw.send(Request::List).is_none(), "connection closed");
}

#[test]
fn a_second_hello_is_refused() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    assert!(matches!(
        raw.hello(writer_statement(START)),
        Response::Welcome { .. }
    ));
    assert_eq!(
        code(&Some(raw.hello(writer_statement(START)))),
        Some(ErrorCode::ProtocolMismatch)
    );
    assert!(
        matches!(raw.send(Request::List), Some(Response::Lockers { .. })),
        "connection still usable"
    );
}

#[test]
fn another_protocol_version_is_refused() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    let reply = raw.send(Request::Hello {
        protocol: PROTOCOL + 1,
        statement: Some(writer_statement(START)),
        signature: None,
        succession: None,
    });
    assert_eq!(code(&reply), Some(ErrorCode::ProtocolMismatch));
}

#[test]
fn an_oversized_frame_closes_the_connection() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    assert!(matches!(
        raw.hello(writer_statement(START)),
        Response::Welcome { .. }
    ));
    raw.stream
        .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
        .unwrap();
    let _ = raw.stream.write_all(&[b'{'; 64]);
    let mut buf = [0u8; 1];
    assert_eq!(
        raw.stream.read(&mut buf).unwrap_or(0),
        0,
        "closed without a reply"
    );
}

#[test]
fn a_read_only_binary_is_refused_by_the_daemon_too() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    assert!(matches!(
        raw.hello(reader_statement(START)),
        Response::Welcome { .. }
    ));
    let reply = raw.send(Request::Put {
        locker: "api_keys".into(),
        data: fake_envelope(1024),
        expected: 0,
    });
    assert_eq!(code(&reply), Some(ErrorCode::NotAllowed));
    let reply = raw.send(Request::Get {
        locker: "not_named".into(),
    });
    assert_eq!(code(&reply), Some(ErrorCode::UnknownLocker));
}

// ---------------------------------------------------------------------------
// Statements

#[test]
fn statements_outside_their_window_are_refused() {
    let env = env();
    let expired = statement(
        APP,
        "writer",
        START - 100 * DAY,
        &[("api_keys", "write", None)],
    );
    match env.open(UID, expired) {
        Err(Error::NotAllowed(msg)) => assert!(msg.contains("expired"), "{msg}"),
        other => panic!("{other:?}"),
    }
    let future = statement(APP, "writer", START + DAY, &[("api_keys", "write", None)]);
    assert!(matches!(env.open(UID, future), Err(Error::NotAllowed(_))));
    let garbage = r#"{"format":1,"surprise":true}"#.to_owned();
    assert!(matches!(env.open(UID, garbage), Err(Error::NotAllowed(_))));
}

#[test]
fn refused_opens_are_reported_once() {
    let env = env();
    let expired = statement(
        APP,
        "writer",
        START - 100 * DAY,
        &[("api_keys", "write", None)],
    );
    assert!(env.open(UID, expired.clone()).is_err());
    assert!(env.open(UID, expired).is_err());
    assert_eq!(env.open(UID, writer_statement(START)).unwrap().refused(), 2);
    assert_eq!(env.open(UID, writer_statement(START)).unwrap().refused(), 0);
}

#[test]
fn describe_says_what_the_daemon_checked() {
    let env = env();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    let d = vault.describe();
    assert!(d.starts_with("vaultorno, test identity"), "{d}");
    assert!(d.contains("blind"), "{d}");
    assert!(d.contains("statement until 2027-01-14"), "{d}");
    assert_eq!(vault.binary(), Some("writer"));
    assert_eq!(vault.vault_name(), Some("test_suite"));
}

// ---------------------------------------------------------------------------
// Limits

#[test]
fn an_app_may_fill_at_most_16_lockers() {
    let env = env();
    let names: Vec<String> = (0..17).map(|i| format!("l{i}")).collect();
    let first16: Vec<(&str, &str, Option<u32>)> = names[..16]
        .iter()
        .map(|n| (n.as_str(), "write", None))
        .collect();
    let a = env.open(UID, statement(APP, "a", START, &first16)).unwrap();
    let b = env
        .open(UID, statement(APP, "b", START, &[("l16", "write", None)]))
        .unwrap();
    for n in &names[..16] {
        a.put(n, b"x", 0).unwrap();
    }
    assert!(matches!(b.put("l16", b"x", 0), Err(Error::NotAllowed(_))));
    let rev = a.get("l0").unwrap().revision;
    a.put("l0", b"", rev).unwrap();
    b.put("l16", b"x", 0).unwrap();
}

#[test]
fn a_user_may_hold_at_most_32_connections() {
    let env = env();
    let held: Vec<Vault> = (0..MAX_CONNECTIONS_PER_USER)
        .map(|_| env.open(UID, writer_statement(START)).unwrap())
        .collect();
    assert!(matches!(
        env.open(UID, writer_statement(START)),
        Err(Error::Busy(_))
    ));
    assert!(
        env.open(UID + 1, writer_statement(START)).is_ok(),
        "other users aren't affected"
    );
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match env.open(UID, writer_statement(START)) {
            Ok(_) => break,
            Err(Error::Busy(_)) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20))
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_user_may_have_at_most_64_vaults() {
    let env = env();
    for i in 0..MAX_VAULTS_PER_USER {
        let app = format!("app{i}");
        env.open_app(
            UID,
            &app,
            statement(&app, "w", START, &[("k", "write", None)]),
        )
        .unwrap();
    }
    let reply = env.open_app(
        UID,
        "one_more",
        statement("one_more", "w", START, &[("k", "write", None)]),
    );
    assert!(matches!(reply, Err(Error::NotAllowed(_))));
    env.open_app(
        UID,
        "app0",
        statement("app0", "w", START, &[("k", "write", None)]),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// The two halves

#[test]
fn a_missing_users_half_makes_a_filled_vault_unavailable() {
    let env = env();
    env.open(UID, writer_statement(START))
        .unwrap()
        .put("api_keys", b"x", 0)
        .unwrap();
    let half = env
        .home(UID)
        .join(".local/share/vaultorno/suite_test/user_half");
    fs::remove_file(&half).unwrap();
    assert!(matches!(
        env.open(UID, writer_statement(START)),
        Err(Error::KeyUnavailable)
    ));
    assert!(!half.exists(), "never papered over with a new half");
}

#[test]
fn a_missing_users_half_is_recreated_for_an_empty_vault() {
    let env = env();
    env.open(UID, writer_statement(START)).unwrap();
    let half = env
        .home(UID)
        .join(".local/share/vaultorno/suite_test/user_half");
    fs::remove_file(&half).unwrap();
    env.open(UID, writer_statement(START)).unwrap();
    assert!(half.exists());
}

#[test]
fn a_changed_daemon_half_shows_as_key_mismatch() {
    let env = env();
    env.open(UID, writer_statement(START))
        .unwrap()
        .put("api_keys", b"x", 0)
        .unwrap();
    let path = env.store().vault_dir(UID, "test_suite").join("daemon_half");
    let mut bytes = fs::read(&path).unwrap();
    bytes[21] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    assert!(matches!(vault.get("api_keys"), Err(Error::KeyMismatch)));
}

#[test]
fn a_users_half_with_a_loose_mode_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let env = env();
    env.open(UID, writer_statement(START)).unwrap();
    let half = env
        .home(UID)
        .join(".local/share/vaultorno/suite_test/user_half");
    fs::set_permissions(&half, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        env.open(UID, writer_statement(START)),
        Err(Error::Private(_))
    ));
}

#[test]
fn binaries_opening_at_once_share_one_users_half() {
    let env = Arc::new(env());
    let opened: Vec<Vault> = (0..6)
        .map(|i| {
            let env = env.clone();
            let st = if i % 2 == 0 {
                writer_statement(START)
            } else {
                reader_statement(START)
            };
            thread::spawn(move || env.open(UID, st).unwrap())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    opened[0].put("api_keys", b"shared", 0).unwrap();
    for v in &opened {
        assert_eq!(v.get("api_keys").unwrap().data.expose_secret(), b"shared");
    }
}

#[test]
fn an_envelope_moved_between_lockers_fails_to_open() {
    let env = env();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    vault.put("api_keys", b"x", 0).unwrap();
    fs::copy(
        env.locker_file(UID, "api_keys"),
        env.locker_file(UID, "short"),
    )
    .unwrap();
    assert!(matches!(vault.get("short"), Err(Error::Corrupt)));
}

// ---------------------------------------------------------------------------
// Files and the sweep

#[test]
fn locker_files_use_the_fixed_layout() {
    let env = env();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    let stored = vault.put("api_keys", b"x", 0).unwrap();
    let bytes = fs::read(env.locker_file(UID, "api_keys")).unwrap();
    assert_eq!(&bytes[..4], b"VLTD");
    assert_eq!(bytes[4], 0, "Milestone 1 layout");
    assert_eq!(
        u64::from_be_bytes(bytes[5..13].try_into().unwrap()),
        stored.revision
    );
    assert_eq!(
        Some(u64::from_be_bytes(bytes[13..21].try_into().unwrap())),
        stored.expires
    );
    assert_eq!(&bytes[21..25], b"VLT1", "the client's envelope follows");
    assert_eq!(bytes.len() - 21, 1024, "one size class");
}

#[test]
fn the_sweep_deletes_expired_lockers_and_stale_vaults() {
    let env = env();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    vault.put("api_keys", b"30 days", 0).unwrap();
    vault.put("short", b"7 days", 0).unwrap();
    drop(vault);
    let lockers = env.store().lockers_dir(UID, "test_suite");
    fs::write(lockers.join("short.tmp"), b"leftover").unwrap();

    env.clock.advance(7 * DAY);
    let r = sweep(env.store(), env.clock.now()).unwrap();
    assert_eq!(
        r,
        SweepReport {
            expired_lockers: 1,
            stale_vaults: 0,
            temp_files: 1
        }
    );
    assert!(!env.locker_file(UID, "short").exists());
    assert!(env.locker_file(UID, "api_keys").exists());

    env.clock.advance(23 * DAY);
    let r = sweep(env.store(), env.clock.now()).unwrap();
    assert_eq!(r.expired_lockers, 1);
    assert!(env.store().vault_exists(UID, "test_suite"), "not stale yet");

    env.clock.advance(91 * DAY);
    let r = sweep(env.store(), env.clock.now()).unwrap();
    assert_eq!(r.stale_vaults, 1);
    assert!(!env.store().vault_exists(UID, "test_suite"));
}

// ---------------------------------------------------------------------------
// Milestone 2: Package manifests, inspector, environment & ptrace_scope checks

struct ManifestEnv {
    server: Arc<Server>,
    inspector: Arc<vaultorno::identity::FakeInspector>,
    root: TempDir,
    manifests_dir: PathBuf,
    bin_dir: PathBuf,
    ptrace_file: PathBuf,
    writer_bin: PathBuf,
    reader_bin: PathBuf,
    unlisted_bin: PathBuf,
    trusted_uid: u32,
}

fn manifest_env(test_fallback: bool) -> ManifestEnv {
    use std::os::unix::fs::PermissionsExt;
    use vaultorno::identity::{FakeInspector, ManifestIdentity};

    let trusted_uid = rustix::process::geteuid().as_raw();
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();

    let manifests_dir = root.path().join("manifests");
    let bin_dir = root.path().join("bin");
    fs::create_dir(&manifests_dir).unwrap();
    fs::set_permissions(&manifests_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(&bin_dir).unwrap();
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o755)).unwrap();

    let writer_bin = bin_dir.join("inforno");
    let reader_bin = bin_dir.join("autorno");
    let unlisted_bin = bin_dir.join("unlisted");
    for (p, bytes) in [
        (&writer_bin, b"#!/bin/sh\n# writer\n" as &[u8]),
        (&reader_bin, b"#!/bin/sh\n# reader\n"),
        (&unlisted_bin, b"#!/bin/sh\n# unlisted\n"),
    ] {
        fs::write(p, bytes).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest_json = serde_json::json!({
        "format": 1,
        "app": "inforno",
        "reject_env": ["INFORNO_PLUGIN_*"],
        "binaries": {
            "inforno": {
                "path": writer_bin.to_str().unwrap(),
                "lockers": { "api_keys": { "access": "write", "lifetime_days": 30 } }
            },
            "autorno": {
                "path": reader_bin.to_str().unwrap(),
                "lockers": { "api_keys": { "access": "read" } }
            }
        }
    })
    .to_string();
    let manifest_path = manifests_dir.join("inforno.json");
    fs::write(&manifest_path, manifest_json).unwrap();
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o644)).unwrap();

    let ptrace_file = root.path().join("ptrace_scope");
    fs::write(&ptrace_file, "1\n").unwrap();

    let inspector = FakeInspector::new();
    let identity = ManifestIdentity::for_testing(
        &manifests_dir,
        root.path(),
        trusted_uid,
        &ptrace_file,
        inspector.clone(),
        test_fallback,
    );
    let clock = Arc::new(FakeClock::new(START));
    let store = Store::open(root.path().join("state")).unwrap();
    let server = Server::new(store, Box::new(identity), clock);

    ManifestEnv {
        server,
        inspector,
        root,
        manifests_dir,
        bin_dir,
        ptrace_file,
        writer_bin,
        reader_bin,
        unlisted_bin,
        trusted_uid,
    }
}

impl ManifestEnv {
    fn inspected_for(&self, bin: &Path, env_names: &[&str]) -> vaultorno_inspect::Inspected {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(bin).unwrap();
        let bytes = fs::read(bin).unwrap();
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        vaultorno_inspect::Inspected {
            pid: 4242,
            uid: self.trusted_uid,
            exe_path: bin.to_path_buf(),
            dev: meta.dev(),
            ino: meta.ino(),
            sha256,
            env_names: env_names.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn connect_as(
        &self,
        bin: &Path,
        env_names: &[&str],
        statement: Option<StatementFiles>,
    ) -> Result<Vault, Error> {
        self.inspector
            .set_default(self.inspected_for(bin, env_names));
        let (client, daemon) = UnixStream::pair().unwrap();
        let server = self.server.clone();
        let uid = self.trusted_uid;
        thread::spawn(move || server.serve(daemon, Peer::without_pidfd(uid, Some(4242))));
        let home = self.root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        Vault::connect(DaemonOptions {
            app: "inforno".into(),
            socket: PathBuf::from("/nonexistent"),
            stream: Some(client),
            home: Some(home),
            uid: Some(uid),
            statement,
        })
    }
}

#[test]
fn an_unlisted_binary_is_refused() {
    let env = manifest_env(false);
    match env.connect_as(&env.unlisted_bin, &["PATH", "HOME"], None) {
        Err(Error::NotAllowed(msg)) => assert!(msg.contains("isn't listed"), "{msg}"),
        other => panic!("expected NotAllowed, got {other:?}"),
    }
}

#[test]
fn a_read_only_manifest_binary_cannot_put() {
    let env = manifest_env(false);
    let writer = env
        .connect_as(&env.writer_bin, &["PATH", "HOME"], None)
        .unwrap();
    assert_eq!(writer.vault_name(), Some("pkg_inforno"));
    assert_eq!(writer.binary(), Some("inforno"));
    assert_eq!(writer.statement_expires(), None);
    assert!(writer.describe().contains("watertight"));
    writer.put("api_keys", b"secret-from-manifest", 0).unwrap();

    let reader = env
        .connect_as(&env.reader_bin, &["PATH", "HOME"], None)
        .unwrap();
    assert_eq!(reader.binary(), Some("autorno"));
    assert_eq!(
        reader.get("api_keys").unwrap().data.expose_secret(),
        b"secret-from-manifest"
    );
    match reader.put("api_keys", b"overwrite", 0) {
        Err(Error::NotAllowed(msg)) => assert!(msg.contains("read-only"), "{msg}"),
        other => panic!("expected NotAllowed, got {other:?}"),
    }
}

#[test]
fn a_rejected_environment_variable_gets_a_binary_refused() {
    let env = manifest_env(false);
    for bad in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "VK_LAYER_PATH",
        "VK_ICD_FILENAMES",
        "GTK_MODULES",
        "INFORNO_PLUGIN_DIR",
    ] {
        match env.connect_as(&env.writer_bin, &["PATH", bad], None) {
            Err(Error::NotAllowed(msg)) => assert_eq!(msg, format!("{bad} set")),
            other => panic!("expected NotAllowed for {bad}, got {other:?}"),
        }
    }
    // Next clean open sees the refused count on `pkg_inforno`.
    let clean = env.connect_as(&env.writer_bin, &["PATH"], None).unwrap();
    assert_eq!(clean.refused(), 6);
}

#[test]
fn manifest_ignores_unsigned_dev_statement_and_allows_no_statement() {
    // Even with test_fallback enabled and an unsigned dev statement in Hello,
    // a binary matching a manifest opens `pkg_<app>` at level `watertight`.
    let env = manifest_env(true);
    let dev_st = StatementFiles {
        statement: statement(
            "inforno",
            "inforno",
            START,
            &[("api_keys", "write", Some(7))],
        ),
        signature: None,
    };
    let vault = env
        .connect_as(&env.writer_bin, &["PATH"], Some(dev_st.clone()))
        .unwrap();
    assert_eq!(vault.vault_name(), Some("pkg_inforno"));
    assert!(vault.describe().contains("watertight"));

    // An unlisted binary falls back to TestIdentity when test_fallback is on.
    let fallback = env
        .connect_as(&env.unlisted_bin, &["PATH"], Some(dev_st))
        .unwrap();
    assert_eq!(fallback.vault_name(), Some("test_inforno"));
}

#[test]
fn manifest_permission_and_ptrace_scope_checks() {
    use std::os::unix::fs::PermissionsExt;
    let env = manifest_env(false);

    // 1. ptrace_scope = 0 is refused for watertight (needs >= 1).
    fs::write(&env.ptrace_file, "0\n").unwrap();
    match env.connect_as(&env.writer_bin, &["PATH"], None) {
        Err(Error::NotAllowed(msg)) => {
            assert!(msg.contains("ptrace_scope is 0, watertight needs 1"), "{msg}")
        }
        other => panic!("{other:?}"),
    }
    fs::write(&env.ptrace_file, "1\n").unwrap();

    // 2. Group-writable binary is refused.
    fs::set_permissions(&env.writer_bin, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(matches!(
        env.connect_as(&env.writer_bin, &["PATH"], None),
        Err(Error::NotAllowed(_))
    ));
    fs::set_permissions(&env.writer_bin, fs::Permissions::from_mode(0o755)).unwrap();

    // 3. Non-world-readable binary (0700) is refused.
    fs::set_permissions(&env.writer_bin, fs::Permissions::from_mode(0o700)).unwrap();
    match env.connect_as(&env.writer_bin, &["PATH"], None) {
        Err(Error::NotAllowed(msg)) => assert!(msg.contains("world-readable"), "{msg}"),
        other => panic!("{other:?}"),
    }
    fs::set_permissions(&env.writer_bin, fs::Permissions::from_mode(0o755)).unwrap();

    // 4. World-writable parent directory is refused.
    fs::set_permissions(&env.bin_dir, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        env.connect_as(&env.writer_bin, &["PATH"], None),
        Err(Error::NotAllowed(_))
    ));
    fs::set_permissions(&env.bin_dir, fs::Permissions::from_mode(0o755)).unwrap();

    // 5. Group-writable manifest file is refused.
    let manifest_path = env.manifests_dir.join("inforno.json");
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(matches!(
        env.connect_as(&env.writer_bin, &["PATH"], None),
        Err(Error::NotAllowed(_))
    ));
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o644)).unwrap();

    // Restored permissions pass again.
    assert!(env.connect_as(&env.writer_bin, &["PATH"], None).is_ok());
}

#[test]
fn progressive_backoff_slows_repeated_refusals() {
    let state = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(START));
    let store = Store::open(state.path().join("state")).unwrap();
    let server = Server::with_backoff(
        store,
        Box::new(TestIdentity),
        clock,
        Duration::from_millis(15),
    );
    let bad = statement(
        APP,
        "writer",
        START - 100 * DAY,
        &[("api_keys", "write", None)],
    );
    let try_bad = || {
        let (client, daemon) = UnixStream::pair().unwrap();
        let srv = server.clone();
        thread::spawn(move || srv.serve(daemon, Peer::without_pidfd(UID, Some(999))));
        let mut raw = Raw::new(client);
        let t0 = Instant::now();
        let _ = raw.hello(bad.clone());
        t0.elapsed()
    };
    let d1 = try_bad();
    let d2 = try_bad();
    let d3 = try_bad();
    assert!(d1 >= Duration::from_millis(12), "d1={d1:?}");
    assert!(d2 >= Duration::from_millis(25), "d2={d2:?}");
    assert!(d3 >= Duration::from_millis(50), "d3={d3:?}");
}
