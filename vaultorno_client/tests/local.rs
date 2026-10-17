//! The `Local` backend: the shared suite, plus what only `Local` has.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use vaultorno_client::testing::{
    APP, Harness, START, reader_statement, statement, writer_statement,
};
use vaultorno_client::time::{DAY, FakeClock};
use vaultorno_client::{Error, ExposeSecret, FakeSealer, LocalOptions, OsSealer, Vault};

fn open(
    base: &Path,
    sealer: Arc<dyn OsSealer>,
    clock: Arc<FakeClock>,
    statement: String,
) -> Result<Vault, Error> {
    Vault::local(LocalOptions {
        app: APP.into(),
        base: Some(base.into()),
        sealer,
        statement: Some(statement),
        clock,
    })
}

fn make_harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let sealer: Arc<dyn OsSealer> = Arc::new(FakeSealer::new());
    Harness {
        writer: open(
            &base,
            sealer.clone(),
            clock.clone(),
            writer_statement(START),
        )
        .unwrap(),
        writer2: open(
            &base,
            sealer.clone(),
            clock.clone(),
            writer_statement(START),
        )
        .unwrap(),
        reader: open(&base, sealer, clock.clone(), reader_statement(START)).unwrap(),
        clock,
        keep: Box::new(dir),
    }
}

vaultorno_client::backend_suite!(make_harness);

#[test]
fn files_are_private_and_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let vault = open(
        &base,
        Arc::new(FakeSealer::new()),
        clock,
        writer_statement(START),
    )
    .unwrap();
    let canary = b"CANARY-local-plaintext-7f3a9c1e";
    vault.put("api_keys", canary, 0).unwrap();

    let folder = base.join("local_suite");
    let file = folder.join("api_keys.cred");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let bytes = fs::read(&file).unwrap();
    assert_eq!(&bytes[..4], b"VLTL");
    assert!(!bytes.windows(canary.len()).any(|w| w == canary));
    assert!(vault.describe().contains("no per-app protection"));
}

#[test]
fn edited_header_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let vault = open(
        &base,
        Arc::new(FakeSealer::new()),
        clock,
        writer_statement(START),
    )
    .unwrap();
    vault.put("api_keys", b"x", 0).unwrap();
    let file = base.join("local_suite/api_keys.cred");
    let mut bytes = fs::read(&file).unwrap();
    bytes[20] ^= 1; // last byte of the expiry
    fs::write(&file, bytes).unwrap();
    assert!(matches!(vault.get("api_keys"), Err(Error::Corrupt)));
}

#[test]
fn locker_moved_to_another_name_fails() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let vault = open(
        &base,
        Arc::new(FakeSealer::new()),
        clock,
        writer_statement(START),
    )
    .unwrap();
    vault.put("api_keys", b"x", 0).unwrap();
    fs::copy(
        base.join("local_suite/api_keys.cred"),
        base.join("local_suite/short.cred"),
    )
    .unwrap();
    assert!(matches!(vault.get("short"), Err(Error::Corrupt)));
}

#[test]
fn statement_must_be_valid_and_for_this_app() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let sealer: Arc<dyn OsSealer> = Arc::new(FakeSealer::new());
    let expired = statement(
        APP,
        "writer",
        START - 100 * DAY,
        &[("api_keys", "write", None)],
    );
    assert!(matches!(
        open(&base, sealer.clone(), clock.clone(), expired),
        Err(Error::NoStatement(_))
    ));
    let other_app = statement("other", "writer", START, &[("api_keys", "write", None)]);
    assert!(matches!(
        open(&base, sealer, clock, other_app),
        Err(Error::NoStatement(_))
    ));
}

#[test]
fn opening_sweeps_expired_files() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let sealer: Arc<dyn OsSealer> = Arc::new(FakeSealer::new());
    let vault = open(
        &base,
        sealer.clone(),
        clock.clone(),
        writer_statement(START),
    )
    .unwrap();
    vault.put("short", b"x", 0).unwrap();
    assert!(base.join("local_suite/short.cred").exists());
    clock.advance(7 * DAY);
    let _again = open(&base, sealer, clock, writer_statement(START)).unwrap();
    assert!(!base.join("local_suite/short.cred").exists());
}

/// Needs a real `systemd-creds` and a user session; run with
/// `cargo test --release -p vaultorno_client --features testing -- --ignored real_systemd_creds`.
#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn real_systemd_creds() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let vault = open(
        &base,
        Arc::new(vaultorno_client::SystemdCredsUser),
        clock,
        writer_statement(START),
    )
    .unwrap();
    vault
        .put("api_keys", b"sealed by systemd-creds", 0)
        .unwrap();
    assert_eq!(
        vault.get("api_keys").unwrap().data.expose_secret(),
        b"sealed by systemd-creds"
    );
}

/// Needs Windows; run with
/// `cargo test --release -p vaultorno_client --features testing -- --ignored real_dpapi`.
#[cfg(windows)]
#[test]
#[ignore]
fn real_dpapi() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("vaultorno");
    let clock = Arc::new(FakeClock::new(START));
    let vault = open(
        &base,
        Arc::new(vaultorno_client::Dpapi),
        clock,
        writer_statement(START),
    )
    .unwrap();
    vault.put("api_keys", b"sealed by DPAPI", 0).unwrap();
    assert_eq!(
        vault.get("api_keys").unwrap().data.expose_secret(),
        b"sealed by DPAPI"
    );
    let raw = fs::read(base.join("local_suite/api_keys.cred")).unwrap();
    assert!(!raw.windows(15).any(|w| w == b"sealed by DPAPI"));

    // Bound to the label.
    let sealer = vaultorno_client::Dpapi;
    let blob = sealer.seal("one_label", b"data").unwrap();
    assert!(sealer.unseal("another_label", &blob).is_err());
}
