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
use vaultorno::master_key::{MasterKey, Sealing};
use vaultorno::server::{MAX_CONNECTIONS_PER_USER, MAX_VAULTS_PER_USER, Server};
use vaultorno::store::Store;
use vaultorno::sweep::{SweepReport, sweep};
use vaultorno_client::protocol::{
    ENVELOPE_MAGIC, ErrorCode, Frame, MAX_ENVIRONMENT, MAX_FRAME, PROTOCOL, Request, Response,
    read_frame, write_frame,
};
use vaultorno_client::testing::{
    APP, Harness, START, reader_statement, statement, writer_statement,
};
use vaultorno_client::time::{Clock, DAY, FakeClock};
use vaultorno_client::{
    DaemonOptions, Error, ExposeSecret, StatementFiles, SuccessionFiles, Vault,
};

// ---------------------------------------------------------------------------
// Test environment

struct Env {
    server: Arc<Server>,
    clock: Arc<FakeClock>,
    _state: TempDir,
    homes: TempDir,
}

/// The master key the tests' stores are sealed under (§6).
fn test_key() -> MasterKey {
    MasterKey::new(&[0x4d; 32], Sealing::Unsealed)
}

fn env() -> Env {
    let state = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(START));
    let store = Store::open(state.path().join("state"), test_key()).unwrap();
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
            succession: None,
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
            succession_signature: None,
            environment: Vec::new(),
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

    let half = |uid| {
        let lock = env.store().lock_vault(uid, "test_suite").unwrap();
        *env.store().daemon_half(&lock, uid, "test_suite").unwrap()
    };
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
    // The daemon's layer refuses it before the client's could (§6).
    match b.get("api_keys") {
        Err(Error::Storage(msg)) => assert!(msg.contains("doesn't decrypt"), "{msg}"),
        other => panic!("{other:?}"),
    }
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
        succession_signature: None,
        environment: Vec::new(),
    });
    assert_eq!(code(&reply), Some(ErrorCode::ProtocolMismatch));
}

#[test]
fn a_succession_without_its_signature_is_refused() {
    let env = env();
    let mut raw = Raw::new(env.stream(UID));
    let reply = raw.send(Request::Hello {
        protocol: PROTOCOL,
        statement: Some(writer_statement(START)),
        signature: None,
        succession: Some("{}".into()),
        succession_signature: None,
        environment: Vec::new(),
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
    assert_eq!(vault.sealer(), Sealing::Unsealed.describe());
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
    // An edited half no longer decrypts (§6): the daemon won't serve it.
    let mut bytes = fs::read(&path).unwrap();
    bytes[21] ^= 0xff;
    fs::write(&path, &bytes).unwrap();
    match env.open(UID, writer_statement(START)) {
        Err(Error::Storage(msg)) => assert!(msg.contains("daemon_half"), "{msg}"),
        other => panic!("{other:?}"),
    }
    // A lost half is replaced by a new one, which leaves the lockers
    // unreadable, never readable.
    fs::remove_file(&path).unwrap();
    let vault = env.open(UID, writer_statement(START)).unwrap();
    assert!(matches!(vault.get("api_keys"), Err(Error::KeyMismatch)));
}

#[test]
fn a_daemon_with_another_master_key_reads_nothing() {
    let env = env();
    env.open(UID, writer_statement(START))
        .unwrap()
        .put("api_keys", b"x", 0)
        .unwrap();
    let wrong = Server::new(
        Store::open(
            env.store().root(),
            MasterKey::new(&[0x99; 32], Sealing::Unsealed),
        )
        .unwrap(),
        Box::new(TestIdentity),
        env.clock.clone(),
    );
    let (client, daemon) = UnixStream::pair().unwrap();
    thread::spawn(move || wrong.serve(daemon, Peer::without_pidfd(UID, None)));
    match env.open_with(UID, client, APP, writer_statement(START)) {
        Err(Error::Storage(msg)) => assert!(msg.contains("daemon_half"), "{msg}"),
        other => panic!("{other:?}"),
    }
    let vault = env.open(UID, writer_statement(START)).unwrap();
    assert_eq!(
        vault.get("api_keys").unwrap().data.expose_secret(),
        b"x",
        "the right key still reads it"
    );
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
    // The daemon's layer refuses it before the client's could (§6).
    assert!(matches!(vault.get("short"), Err(Error::Storage(_))));
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
    assert_eq!(bytes[4], 1, "Milestone 4 layout");
    assert_eq!(
        u64::from_be_bytes(bytes[5..13].try_into().unwrap()),
        stored.revision
    );
    assert_eq!(
        Some(u64::from_be_bytes(bytes[13..21].try_into().unwrap())),
        stored.expires
    );
    // A nonce, then the client's envelope sealed again under the master key.
    assert!(!contains(&bytes[21..], b"VLT1"), "the envelope is sealed");
    assert_eq!(bytes.len() - 21 - 24 - 16, 1024, "one size class");
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
    let store = Store::open(root.path().join("state"), test_key()).unwrap();
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
    fn inspected_for(&self, bin: &Path) -> vaultorno_inspect::Inspected {
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
            code: Vec::new(),
        }
    }

    fn connect_as(
        &self,
        bin: &Path,
        env_names: &[&str],
        statement: Option<StatementFiles>,
    ) -> Result<Vault, Error> {
        self.connect_inspected(self.inspected_for(bin), env_names, statement)
    }

    /// Connects as a caller the inspector reports as `inspected`, whose
    /// client names `env_names` as set (§7.5).
    fn connect_inspected(
        &self,
        inspected: vaultorno_inspect::Inspected,
        env_names: &[&str],
        statement: Option<StatementFiles>,
    ) -> Result<Vault, Error> {
        self.inspector.set_default(inspected);
        let (client, daemon) = UnixStream::pair().unwrap();
        let server = self.server.clone();
        let uid = self.trusted_uid;
        let peer = Peer::without_pidfd(uid, Some(4242)).with_environment(env_names);
        thread::spawn(move || server.serve_with_environment(daemon, peer));
        let home = self.root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        Vault::connect(DaemonOptions {
            app: "inforno".into(),
            socket: PathBuf::from("/nonexistent"),
            stream: Some(client),
            home: Some(home),
            uid: Some(uid),
            statement,
            succession: None,
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
fn hello_names_the_callers_environment() {
    let env = manifest_env(false);
    env.inspector.set_default(env.inspected_for(&env.writer_bin));
    let hello = |names: &[&str]| {
        let (client, daemon) = UnixStream::pair().unwrap();
        let server = env.server.clone();
        // What the peer was given is replaced by what Hello names.
        let peer =
            Peer::without_pidfd(env.trusted_uid, Some(4242)).with_environment(&["LD_PRELOAD"]);
        thread::spawn(move || server.serve(daemon, peer));
        Raw::new(client).send(Request::Hello {
            protocol: PROTOCOL,
            statement: None,
            signature: None,
            succession: None,
            succession_signature: None,
            environment: names.iter().map(|s| (*s).to_owned()).collect(),
        })
    };
    assert!(matches!(
        hello(&["PATH", "HOME"]),
        Some(Response::Welcome { .. })
    ));
    match hello(&["PATH", "GTK_MODULES"]) {
        Some(Response::Error {
            code: ErrorCode::NotAllowed,
            message,
        }) => assert_eq!(message, "GTK_MODULES set"),
        other => panic!("{other:?}"),
    }
    let many = vec!["X"; MAX_ENVIRONMENT + 1];
    assert_eq!(code(&hello(&many)), Some(ErrorCode::ProtocolMismatch));
}

#[test]
fn foreign_code_gets_a_binary_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use vaultorno_inspect::MappedCode;

    let env = manifest_env(false);
    let lib = env.bin_dir.join("libfine.so");
    fs::write(&lib, b"not really a library").unwrap();
    fs::set_permissions(&lib, fs::Permissions::from_mode(0o755)).unwrap();
    let lib_ino = fs::metadata(&lib).unwrap().ino();
    let writer_ino = fs::metadata(&env.writer_bin).unwrap().ino();
    let mapped = |path: &Path, ino: u64, deleted: bool| MappedCode {
        path: path.to_path_buf(),
        ino,
        deleted,
    };
    let with_code = |code: Vec<MappedCode>| {
        let mut inspected = env.inspected_for(&env.writer_bin);
        inspected.code = code;
        env.connect_inspected(inspected, &["PATH"], None)
    };

    // Libraries only root can change (here, the trusted user), and the
    // executable itself, which its manifest covers.
    with_code(vec![
        mapped(&env.writer_bin, writer_ino, false),
        mapped(&lib, lib_ino, false),
    ])
    .unwrap();

    let refusal = |code: MappedCode| match with_code(vec![code]) {
        Err(Error::NotAllowed(msg)) => msg,
        other => panic!("expected NotAllowed, got {other:?}"),
    };
    let msg = refusal(mapped(&lib, lib_ino, true));
    assert!(msg.contains("replaced or removed"), "{msg}");
    let msg = refusal(mapped(&lib, lib_ino + 1, false));
    assert!(msg.contains("isn't the file that was loaded"), "{msg}");
    // A path the daemon can't see, e.g. under /home with ProtectHome=yes.
    let msg = refusal(mapped(&env.root.path().join("home/libevil.so"), 7, false));
    assert!(msg.contains("can't stat"), "{msg}");
    fs::set_permissions(&lib, fs::Permissions::from_mode(0o775)).unwrap();
    let msg = refusal(mapped(&lib, lib_ino, false));
    assert!(msg.contains("writable by group or others"), "{msg}");
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
    let store = Store::open(state.path().join("state"), test_key()).unwrap();
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

// ---------------------------------------------------------------------------
// Milestone 3: signed statements, vaults by key fingerprint, revoked_keys and
// successions

use ssh_key::private::Ed25519Keypair;
use ssh_key::{HashAlg, LineEnding, PrivateKey, SshSig};
use vaultorno::signing::{
    KeyFingerprint, REVOKED_KEYS, STATEMENT_NAMESPACE, SUCCESSION_NAMESPACE, SUCCESSIONS,
    edit_revoked_keys,
};
use vaultorno_client::time::format_rfc3339;

struct SignedEnv {
    server: Arc<Server>,
    inspector: Arc<vaultorno::identity::FakeInspector>,
    clock: Arc<FakeClock>,
    root: TempDir,
    state: PathBuf,
    bin_dir: PathBuf,
    manifests_dir: PathBuf,
    ptrace_file: PathBuf,
    uid: u32,
}

/// A signing key, the same for the same seed.
fn signing_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn fingerprint(key: &PrivateKey) -> KeyFingerprint {
    KeyFingerprint::of(key.public_key())
}

/// `ssh-keygen -Y sign -n vaultorno/statement` (SHA-512 is its default).
fn sign(key: &PrivateKey, namespace: &str, text: &str) -> String {
    SshSig::sign(key, namespace, HashAlg::Sha512, text.as_bytes())
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

fn signed_env() -> SignedEnv {
    use std::os::unix::fs::PermissionsExt;
    use vaultorno::identity::{FakeInspector, ManifestIdentity, SignedIdentity};

    let uid = rustix::process::geteuid().as_raw();
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let manifests_dir = root.path().join("manifests");
    let bin_dir = root.path().join("bin");
    for dir in [&manifests_dir, &bin_dir] {
        fs::create_dir(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ptrace_file = root.path().join("ptrace_scope");
    fs::write(&ptrace_file, "2\n").unwrap();

    let inspector = FakeInspector::new();
    let manifests = ManifestIdentity::for_testing(
        &manifests_dir,
        root.path(),
        uid,
        &ptrace_file,
        inspector.clone(),
        false,
    );
    let state = root.path().join("state");
    let store = Store::open(&state, test_key()).unwrap();
    let identity = SignedIdentity::new(manifests, &state);
    let clock = Arc::new(FakeClock::new(START));
    let server = Server::new(store, Box::new(identity), clock.clone());
    SignedEnv {
        server,
        inspector,
        clock,
        root,
        state,
        bin_dir,
        manifests_dir,
        ptrace_file,
        uid,
    }
}

impl SignedEnv {
    /// A test executable with distinct contents, so a distinct hash.
    fn binary(&self, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.bin_dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n# {contents}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn inspected(&self, bin: &Path) -> vaultorno_inspect::Inspected {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(bin).unwrap();
        vaultorno_inspect::Inspected {
            pid: 4343,
            uid: self.uid,
            exe_path: bin.to_path_buf(),
            dev: meta.dev(),
            ino: meta.ino(),
            sha256: Sha256::digest(fs::read(bin).unwrap()).into(),
            code: Vec::new(),
        }
    }

    fn sha256_of(&self, bin: &Path) -> String {
        self.inspected(bin).sha256_hex()
    }

    /// A statement for `bin`, issued now and valid 90 days, with `extra`
    /// fields merged in.
    fn statement(
        &self,
        bin: &Path,
        lockers: &[(&str, &str, Option<u32>)],
        extra: serde_json::Value,
    ) -> String {
        let name = bin.file_name().unwrap().to_str().unwrap();
        let text = statement("inforno", name, self.clock.now(), lockers);
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["sha256"] = self.sha256_of(bin).into();
        for (k, v) in extra.as_object().unwrap() {
            value[k] = v.clone();
        }
        value.to_string()
    }

    fn connect(
        &self,
        bin: &Path,
        env_names: &[&str],
        text: String,
        signature: Option<String>,
    ) -> Result<Vault, Error> {
        self.connect_with(bin, env_names, text, signature, None)
    }

    fn connect_with(
        &self,
        bin: &Path,
        env_names: &[&str],
        text: String,
        signature: Option<String>,
        succession: Option<SuccessionFiles>,
    ) -> Result<Vault, Error> {
        self.inspector.set_default(self.inspected(bin));
        let (client, daemon) = UnixStream::pair().unwrap();
        let server = self.server.clone();
        let uid = self.uid;
        let peer = Peer::without_pidfd(uid, Some(4343)).with_environment(env_names);
        thread::spawn(move || server.serve_with_environment(daemon, peer));
        Vault::connect(DaemonOptions {
            app: "inforno".into(),
            socket: PathBuf::from("/nonexistent"),
            stream: Some(client),
            home: Some(self.home()),
            uid: Some(uid),
            statement: Some(StatementFiles {
                statement: text,
                signature,
            }),
            succession,
        })
    }

    /// Opens `bin` with a statement for it signed by `key`.
    fn open(
        &self,
        bin: &Path,
        key: &PrivateKey,
        lockers: &[(&str, &str, Option<u32>)],
    ) -> Result<Vault, Error> {
        let text = self.statement(bin, lockers, serde_json::json!({}));
        let sig = sign(key, STATEMENT_NAMESPACE, &text);
        self.connect(bin, &["PATH", "HOME"], text, Some(sig))
    }

    /// Opens `bin` for writing with a statement signed by `key`, sending
    /// `succession` with it.
    fn open_succeeding(
        &self,
        bin: &Path,
        key: &PrivateKey,
        succession: SuccessionFiles,
    ) -> Result<Vault, Error> {
        let text = self.statement(bin, WRITE, serde_json::json!({}));
        let sig = sign(key, STATEMENT_NAMESPACE, &text);
        self.connect_with(bin, &["PATH", "HOME"], text, Some(sig), Some(succession))
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }
}

const WRITE: &[(&str, &str, Option<u32>)] = &[("api_keys", "write", Some(30))];
const READ: &[(&str, &str, Option<u32>)] = &[("api_keys", "read", None)];

fn not_allowed(result: Result<Vault, Error>) -> String {
    match result {
        Err(Error::NotAllowed(msg)) => msg,
        other => panic!("expected NotAllowed, got {other:?}"),
    }
}

#[test]
fn a_signed_statement_opens_its_keys_vault() {
    let env = signed_env();
    let key = signing_key(1);
    let (inforno, autorno) = (
        env.binary("inforno", "writer"),
        env.binary("autorno", "reader"),
    );

    let writer = env.open(&inforno, &key, WRITE).unwrap();
    let vault = fingerprint(&key).hex();
    assert_eq!(writer.vault_name(), Some(vault.as_str()));
    assert_eq!(writer.binary(), Some("inforno"));
    assert_eq!(writer.statement_expires(), Some(START + 90 * DAY));
    assert!(writer.describe().starts_with("vaultorno, airtight, blind"));
    writer.put("api_keys", b"sk-signed-1", 0).unwrap();

    // Another binary of the same app, signed by the same key, shares the
    // vault and the user's half.
    let reader = env.open(&autorno, &key, READ).unwrap();
    assert_eq!(reader.vault_name(), Some(vault.as_str()));
    assert_eq!(
        reader.get("api_keys").unwrap().data.expose_secret(),
        b"sk-signed-1"
    );
    assert!(matches!(
        reader.put("api_keys", b"x", 0),
        Err(Error::NotAllowed(_))
    ));
    let half = env
        .home()
        .join(".local/share/vaultorno")
        .join(format!("inforno_{}", fingerprint(&key).key_id()))
        .join("user_half");
    assert!(half.is_file(), "{}", half.display());
    assert!(env.state.join(format!("users/{}/{vault}/daemon_half", env.uid)).is_file());
}

#[test]
fn a_statement_for_another_hash_is_refused() {
    let env = signed_env();
    let key = signing_key(1);
    let genuine = env.binary("inforno", "genuine");
    let text = env.statement(&genuine, WRITE, serde_json::json!({}));
    let sig = sign(&key, STATEMENT_NAMESPACE, &text);

    // The genuine binary's statement, presented by another executable.
    let tampered = env.binary("tampered", "something else");
    let msg = not_allowed(env.connect(&tampered, &["PATH"], text.clone(), Some(sig.clone())));
    assert!(msg.contains("the statement is for executable"), "{msg}");

    // A byte-identical copy elsewhere is the same executable.
    fs::copy(&genuine, env.bin_dir.join("copy")).unwrap();
    let copy = env.connect(&env.bin_dir.join("copy"), &["PATH"], text, Some(sig));
    assert_eq!(copy.unwrap().vault_name(), Some(fingerprint(&key).hex().as_str()));
}

#[test]
fn an_expired_or_not_yet_valid_statement_is_refused() {
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "writer");
    let text = env.statement(&bin, WRITE, serde_json::json!({}));
    let sig = sign(&key, STATEMENT_NAMESPACE, &text);

    // Refusals for a key whose vault doesn't exist yet aren't counted: anyone
    // can make keys, and the daemon keeps no count for vaults nobody uses.
    env.clock.set(START + 90 * DAY);
    not_allowed(env.connect(&bin, &["PATH"], text.clone(), Some(sig.clone())));
    env.clock.set(START);
    assert_eq!(env.open(&bin, &key, WRITE).unwrap().refused(), 0);

    env.clock.set(START - 1);
    let msg = not_allowed(env.connect(&bin, &["PATH"], text.clone(), Some(sig.clone())));
    assert!(msg.contains("isn't valid until"), "{msg}");

    env.clock.set(START + 90 * DAY);
    let msg = not_allowed(env.connect(&bin, &["PATH"], text, Some(sig)));
    assert!(msg.contains("statement expired on"), "{msg}");

    // A fresh statement opens the vault, and hears about both refusals.
    let fresh = env.open(&bin, &key, WRITE).unwrap();
    assert_eq!(fresh.refused(), 2);
}

#[test]
fn a_statement_signed_by_another_key_opens_another_empty_vault() {
    let env = signed_env();
    let (release, other) = (signing_key(1), signing_key(2));
    let bin = env.binary("inforno", "writer");
    env.open(&bin, &release, WRITE)
        .unwrap()
        .put("api_keys", b"release secret", 0)
        .unwrap();

    let impostor = env.open(&bin, &other, WRITE).unwrap();
    assert_eq!(impostor.vault_name(), Some(fingerprint(&other).hex().as_str()));
    assert_ne!(fingerprint(&other), fingerprint(&release));
    assert_eq!(impostor.get("api_keys").unwrap().revision, 0);
    assert!(impostor.get("api_keys").unwrap().data.expose_secret().is_empty());
}

#[test]
fn a_tampered_or_misdirected_signature_is_refused() {
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "writer");
    let text = env.statement(&bin, WRITE, serde_json::json!({}));

    let sig = sign(&key, STATEMENT_NAMESPACE, &text);
    let edited = text.replace("\"lifetime_days\":30", "\"lifetime_days\":90");
    assert_ne!(edited, text);
    let msg = not_allowed(env.connect(&bin, &["PATH"], edited, Some(sig)));
    assert_eq!(msg, "statement signature: bad signature");

    let wrong_ns = sign(&key, "file", &text);
    let msg = not_allowed(env.connect(&bin, &["PATH"], text.clone(), Some(wrong_ns)));
    assert!(msg.contains("namespace"), "{msg}");

    let msg = not_allowed(env.connect(&bin, &["PATH"], text, Some("garbage".into())));
    assert!(msg.contains("not an SSH signature"), "{msg}");
}

#[test]
fn a_revoked_key_gets_no_access_until_unrevoked() {
    let env = signed_env();
    let (key, other) = (signing_key(1), signing_key(2));
    let bin = env.binary("inforno", "writer");
    env.open(&bin, &key, WRITE).unwrap();

    assert!(edit_revoked_keys(&env.state, key.public_key(), false).unwrap());
    let msg = not_allowed(env.open(&bin, &key, WRITE));
    assert!(msg.contains("revoked key"), "{msg}");
    assert!(msg.contains(&fingerprint(&key).to_string()), "{msg}");
    assert!(env.open(&bin, &other, WRITE).is_ok(), "other keys still work");

    assert!(edit_revoked_keys(&env.state, key.public_key(), true).unwrap());
    let back = env.open(&bin, &key, WRITE).unwrap();
    assert_eq!(back.refused(), 1);
}

#[test]
fn a_key_list_that_doesnt_parse_refuses_everything() {
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "writer");
    for file in [REVOKED_KEYS, SUCCESSIONS] {
        fs::write(env.state.join(file), "# fine\nnot a key\n").unwrap();
        match env.open(&bin, &key, WRITE) {
            Err(Error::Storage(msg)) => {
                assert!(msg.starts_with(&format!("{file} line 2")), "{msg}")
            }
            other => panic!("expected Storage for {file}, got {other:?}"),
        }
        // Unsigned callers, which only a manifest could vouch for, too.
        let unsigned = env.statement(&bin, WRITE, serde_json::json!({}));
        assert!(matches!(
            env.connect(&bin, &["PATH"], unsigned, None),
            Err(Error::Storage(_))
        ));
        fs::write(env.state.join(file), "# fine\n").unwrap();
        assert!(env.open(&bin, &key, WRITE).is_ok());
    }
}

#[test]
fn a_recorded_succession_carries_the_vault_to_a_new_key() {
    let env = signed_env();
    let (old, new) = (signing_key(1), signing_key(2));
    let bin = env.binary("inforno", "writer");
    env.open(&bin, &old, WRITE)
        .unwrap()
        .put("api_keys", b"carried over", 0)
        .unwrap();

    let mut new_public = new.public_key().clone();
    new_public.set_comment("inforno_release_2027");
    fs::write(
        env.state.join(SUCCESSIONS),
        format!("{} {}\n", fingerprint(&old), new_public.to_openssh().unwrap()),
    )
    .unwrap();
    let rotated = env.open(&bin, &new, WRITE).unwrap();
    assert_eq!(rotated.vault_name(), Some(fingerprint(&old).hex().as_str()));
    assert_eq!(
        rotated.get("api_keys").unwrap().data.expose_secret(),
        b"carried over"
    );

    // Revoking the old key doesn't touch its successor; revoking the new one
    // does.
    edit_revoked_keys(&env.state, old.public_key(), false).unwrap();
    assert!(env.open(&bin, &new, WRITE).is_ok());
    edit_revoked_keys(&env.state, new.public_key(), false).unwrap();
    assert!(not_allowed(env.open(&bin, &new, WRITE)).contains("revoked key"));
}

#[test]
fn airtight_needs_ptrace_scope_2_and_hermetic_is_refused() {
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "writer");
    fs::write(&env.ptrace_file, "1\n").unwrap();
    let msg = not_allowed(env.open(&bin, &key, WRITE));
    assert_eq!(msg, "ptrace_scope is 1, airtight needs 2");
    fs::write(&env.ptrace_file, "3\n").unwrap();
    assert!(env.open(&bin, &key, WRITE).is_ok());

    let text = env.statement(&bin, WRITE, serde_json::json!({ "level": "hermetic" }));
    let sig = sign(&key, STATEMENT_NAMESPACE, &text);
    let msg = not_allowed(env.connect(&bin, &["PATH"], text, Some(sig)));
    assert!(msg.contains("hermetic needs a TPM2-sealed master key"), "{msg}");
}

#[test]
fn the_statements_reject_env_adds_to_the_built_in_list() {
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "writer");
    let text = env.statement(
        &bin,
        WRITE,
        serde_json::json!({ "reject_env": ["INFORNO_PLUGIN_*"] }),
    );
    let sig = sign(&key, STATEMENT_NAMESPACE, &text);
    for bad in ["LD_PRELOAD", "VK_LAYER_PATH", "INFORNO_PLUGIN_DIR"] {
        let msg = not_allowed(env.connect(&bin, &["PATH", bad], text.clone(), Some(sig.clone())));
        assert_eq!(msg, format!("{bad} set"));
    }
    assert!(env.connect(&bin, &["PATH", "INFORNO_HOME"], text, Some(sig)).is_ok());
}

#[test]
fn a_manifest_still_vouches_when_a_statement_fails() {
    use std::os::unix::fs::PermissionsExt;
    let env = signed_env();
    let key = signing_key(1);
    let bin = env.binary("inforno", "packaged");
    let manifest = env.manifests_dir.join("inforno.json");
    fs::write(
        &manifest,
        serde_json::json!({
            "format": 1,
            "app": "inforno",
            "binaries": { "inforno": {
                "path": bin.to_str().unwrap(),
                "lockers": { "api_keys": { "access": "write" } }
            } }
        })
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();

    // A statement for another build: the manifest still lists this binary.
    let other = env.binary("other_build", "upstream build");
    let text = env.statement(&other, WRITE, serde_json::json!({}));
    let sig = sign(&key, STATEMENT_NAMESPACE, &text);
    let vault = env.connect(&bin, &["PATH"], text, Some(sig)).unwrap();
    assert_eq!(vault.vault_name(), Some("pkg_inforno"));
    assert!(vault.describe().contains("watertight"));
}

/// A succession statement handing `from`'s vault to `to` (§8).
fn succession_text(from: &PrivateKey, to: &PrivateKey, issued: u64) -> String {
    let mut public = to.public_key().clone();
    public.set_comment("inforno_release_2027");
    serde_json::json!({
        "format": 1,
        "succession": {
            "from": fingerprint(from).to_string(),
            "to": public.to_openssh().unwrap(),
        },
        "issued": format_rfc3339(issued),
    })
    .to_string()
}

fn signed_succession(text: String, signer: &PrivateKey, namespace: &str) -> SuccessionFiles {
    SuccessionFiles {
        signature: sign(signer, namespace, &text),
        succession: text,
    }
}

#[test]
fn a_succession_in_hello_carries_the_vault_and_is_recorded() {
    let env = signed_env();
    let (old, new) = (signing_key(1), signing_key(2));
    let bin = env.binary("inforno", "writer");
    env.open(&bin, &old, WRITE)
        .unwrap()
        .put("api_keys", b"carried over", 0)
        .unwrap();

    let succession = signed_succession(
        succession_text(&old, &new, START),
        &old,
        SUCCESSION_NAMESPACE,
    );
    let rotated = env.open_succeeding(&bin, &new, succession.clone()).unwrap();
    assert_eq!(rotated.vault_name(), Some(fingerprint(&old).hex().as_str()));
    assert_eq!(
        rotated.get("api_keys").unwrap().data.expose_secret(),
        b"carried over"
    );
    let recorded = fs::read_to_string(env.state.join(SUCCESSIONS)).unwrap();
    assert!(
        recorded.contains(&format!("{} ssh-ed25519 ", fingerprint(&old))),
        "{recorded}"
    );
    assert!(recorded.contains("inforno_release_2027"));

    // Recorded once: presenting it again changes nothing, and the new key
    // no longer needs it.
    env.open_succeeding(&bin, &new, succession).unwrap();
    assert_eq!(
        fs::read_to_string(env.state.join(SUCCESSIONS)).unwrap(),
        recorded
    );
    let later = env.open(&bin, &new, WRITE).unwrap();
    assert_eq!(later.vault_name(), Some(fingerprint(&old).hex().as_str()));

    // A key succeeds only one other.
    let third = signing_key(3);
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &new,
        signed_succession(
            succession_text(&third, &new, START),
            &third,
            SUCCESSION_NAMESPACE,
        ),
    ));
    assert!(msg.contains("already succeeds"), "{msg}");
}

#[test]
fn a_bad_succession_is_refused_and_not_recorded() {
    let env = signed_env();
    let (old, new, other) = (signing_key(1), signing_key(2), signing_key(3));
    let bin = env.binary("inforno", "writer");
    env.open(&bin, &old, WRITE).unwrap();
    let good = succession_text(&old, &new, START);

    // Signed by a key other than the one it's from.
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &new,
        signed_succession(good.clone(), &other, SUCCESSION_NAMESPACE),
    ));
    assert!(msg.contains("not by the key it's from"), "{msg}");

    // A statement's signature can't stand in for a succession's.
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &new,
        signed_succession(good.clone(), &old, STATEMENT_NAMESPACE),
    ));
    assert!(msg.contains("namespace"), "{msg}");

    // Handing over to another key than the statement's.
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &other,
        signed_succession(good.clone(), &old, SUCCESSION_NAMESPACE),
    ));
    assert!(msg.contains("the statement is signed by"), "{msg}");

    // Not valid yet.
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &new,
        signed_succession(
            succession_text(&old, &new, START + DAY),
            &old,
            SUCCESSION_NAMESPACE,
        ),
    ));
    assert!(msg.contains("isn't valid until"), "{msg}");

    // Tampered with.
    let mut tampered = signed_succession(good.clone(), &old, SUCCESSION_NAMESPACE);
    tampered.succession = tampered
        .succession
        .replace("inforno_release_2027", "inforno_release_2028");
    let msg = not_allowed(env.open_succeeding(&bin, &new, tampered));
    assert!(msg.contains("bad signature"), "{msg}");

    // From a revoked key.
    edit_revoked_keys(&env.state, old.public_key(), false).unwrap();
    let msg = not_allowed(env.open_succeeding(
        &bin,
        &new,
        signed_succession(good, &old, SUCCESSION_NAMESPACE),
    ));
    assert!(msg.contains("from revoked key"), "{msg}");

    assert!(!env.state.join(SUCCESSIONS).exists(), "nothing recorded");
}
