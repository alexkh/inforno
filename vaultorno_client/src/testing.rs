//! The backend suite (design doc §17): the same tests, run against the
//! daemon and the `Local` backend, so both keep the same semantics.
//!
//! A crate runs it with `vaultorno_client::backend_suite!(make_harness);`,
//! where `make_harness() -> Harness` opens three vaults on one fresh store.

use std::any::Any;
use std::cell::Cell;
use std::sync::Arc;

use secrecy::ExposeSecret;
use serde_json::json;

use crate::protocol::MAX_DATA;
use crate::time::{Clock, DAY, FakeClock, format_rfc3339};
use crate::{Error, Locker, Vault};

/// The app every suite vault belongs to.
pub const APP: &str = "suite";

/// Three vaults on one fresh store, sharing a fake clock.
pub struct Harness {
    /// May write `api_keys` (30 days) and `short` (7 days).
    pub writer: Vault,
    /// Same statement as `writer`; a second process.
    pub writer2: Vault,
    /// May only read both lockers.
    pub reader: Vault,
    pub clock: Arc<FakeClock>,
    /// Whatever must outlive the vaults: temp folders, servers.
    pub keep: Box<dyn Any>,
}

/// A statement as JSON text, valid for 90 days from `now`. Each locker is
/// `(name, "read" | "write", lifetime_days)`.
pub fn statement(
    app: &str,
    binary: &str,
    now: u64,
    lockers: &[(&str, &str, Option<u32>)],
) -> String {
    let mut map = serde_json::Map::new();
    for (name, access, days) in lockers {
        let mut grant = json!({ "access": access });
        if let Some(d) = days {
            grant["lifetime_days"] = json!(d);
        }
        map.insert((*name).to_owned(), grant);
    }
    json!({
        "format": 1,
        "app": app,
        "binary": binary,
        "sha256": "0".repeat(64),
        "issued": format_rfc3339(now),
        "expires": format_rfc3339(now + 90 * DAY),
        "lockers": map,
    })
    .to_string()
}

/// The suite's writer statement.
pub fn writer_statement(now: u64) -> String {
    statement(
        APP,
        "writer",
        now,
        &[("api_keys", "write", Some(30)), ("short", "write", Some(7))],
    )
}

/// The suite's reader statement.
pub fn reader_statement(now: u64) -> String {
    statement(
        APP,
        "reader",
        now,
        &[("api_keys", "read", None), ("short", "read", None)],
    )
}

/// Where the suite's fake clock starts: 2026-10-16.
pub const START: u64 = 1_792_108_800;

type Make<'a> = &'a dyn Fn() -> Harness;

fn data(l: &Locker) -> Vec<u8> {
    l.data.expose_secret().to_vec()
}

pub fn round_trip(make: Make) {
    let h = make();
    let stored = h.writer.put("api_keys", b"sk-test-123", 0).unwrap();
    assert_ne!(stored.revision, 0);
    let got = h.reader.get("api_keys").unwrap();
    assert_eq!(data(&got), b"sk-test-123");
    assert_eq!(got.revision, stored.revision);
    assert_eq!(got.expires, stored.expires);
    assert_eq!(data(&h.writer2.get("api_keys").unwrap()), b"sk-test-123");
}

pub fn empty_locker_reads_empty(make: Make) {
    let h = make();
    let got = h.reader.get("api_keys").unwrap();
    assert!(data(&got).is_empty());
    assert_eq!(got.revision, 0);
    assert_eq!(got.expires, None);
}

pub fn every_write_changes_revision(make: Make) {
    let h = make();
    let r1 = h.writer.put("api_keys", b"one", 0).unwrap().revision;
    let r2 = h.writer.put("api_keys", b"one", r1).unwrap().revision;
    let r3 = h.writer.put("api_keys", b"two", r2).unwrap().revision;
    assert!(r1 != 0 && r2 != 0 && r3 != 0);
    assert!(r1 != r2 && r2 != r3);
}

pub fn conflicting_writers_detected(make: Make) {
    let h = make();
    let base = h.writer.put("api_keys", b"base", 0).unwrap().revision;
    let a = h.writer.get("api_keys").unwrap();
    let b = h.writer2.get("api_keys").unwrap();
    assert_eq!(a.revision, base);
    assert_eq!(b.revision, base);
    h.writer.put("api_keys", b"from a", a.revision).unwrap();
    assert!(matches!(
        h.writer2.put("api_keys", b"from b", b.revision),
        Err(Error::Conflict)
    ));
    assert_eq!(data(&h.reader.get("api_keys").unwrap()), b"from a");
    // Filling an empty locker races the same way.
    h.writer.put("short", b"first", 0).unwrap();
    assert!(matches!(
        h.writer2.put("short", b"second", 0),
        Err(Error::Conflict)
    ));
}

pub fn update_retries_after_a_conflict(make: Make) {
    let h = make();
    h.writer.put("api_keys", b"a", 0).unwrap();
    let calls = Cell::new(0);
    h.writer2
        .update("api_keys", |old| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                // Another process writes between our get and our put.
                let cur = h.writer.get("api_keys").unwrap();
                h.writer.put("api_keys", b"ab", cur.revision).unwrap();
            }
            Ok([old, b"c"].concat())
        })
        .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(data(&h.reader.get("api_keys").unwrap()), b"abc");
}

pub fn read_only_binary_cannot_put(make: Make) {
    let h = make();
    assert!(matches!(
        h.reader.put("api_keys", b"x", 0),
        Err(Error::NotAllowed(_))
    ));
    assert!(matches!(
        h.reader.update("api_keys", |_| Ok(b"x".to_vec())),
        Err(Error::NotAllowed(_))
    ));
}

pub fn unknown_locker_refused(make: Make) {
    let h = make();
    assert!(matches!(
        h.writer.get("missing"),
        Err(Error::UnknownLocker(_))
    ));
    assert!(matches!(
        h.writer.put("missing", b"x", 0),
        Err(Error::UnknownLocker(_))
    ));
}

pub fn invalid_name_refused(make: Make) {
    let h = make();
    for name in ["Api-Keys", "", "../x", "a.b"] {
        assert!(
            matches!(h.writer.get(name), Err(Error::InvalidName(_))),
            "{name:?}"
        );
    }
}

pub fn empty_put_deletes(make: Make) {
    let h = make();
    let rev = h.writer.put("api_keys", b"x", 0).unwrap().revision;
    let stored = h.writer.put("api_keys", b"", rev).unwrap();
    assert_eq!(stored.revision, 0);
    assert_eq!(stored.expires, None);
    let got = h.reader.get("api_keys").unwrap();
    assert_eq!(got.revision, 0);
    assert!(data(&got).is_empty());
}

pub fn size_limit(make: Make) {
    let h = make();
    let biggest = vec![7u8; MAX_DATA];
    let rev = h.writer.put("api_keys", &biggest, 0).unwrap().revision;
    assert_eq!(data(&h.reader.get("api_keys").unwrap()).len(), MAX_DATA);
    assert!(matches!(
        h.writer.put("api_keys", &vec![7u8; MAX_DATA + 1], rev),
        Err(Error::TooLarge(_))
    ));
}

pub fn expiry_set_at_fill_and_not_extended(make: Make) {
    let h = make();
    let now = h.clock.now();
    let first = h.writer.put("api_keys", b"x", 0).unwrap();
    assert_eq!(first.expires, Some(now + 30 * DAY));
    h.clock.advance(10 * DAY);
    let second = h.writer.put("api_keys", b"y", first.revision).unwrap();
    assert_eq!(
        second.expires, first.expires,
        "rewriting must not extend expiry"
    );
    assert_eq!(h.reader.get("api_keys").unwrap().expires, first.expires);
}

pub fn expired_locker_reads_empty(make: Make) {
    let h = make();
    h.writer.put("api_keys", b"x", 0).unwrap();
    h.clock.advance(30 * DAY - 1);
    assert_eq!(data(&h.reader.get("api_keys").unwrap()), b"x");
    h.clock.advance(1);
    let got = h.reader.get("api_keys").unwrap();
    assert_eq!(got.revision, 0);
    assert!(data(&got).is_empty());
    assert_eq!(
        h.writer
            .list()
            .unwrap()
            .iter()
            .find(|l| l.name == "api_keys")
            .unwrap()
            .revision,
        0
    );
    // Refilling starts a new period.
    let refilled = h.writer.put("api_keys", b"new", 0).unwrap();
    assert_eq!(refilled.expires, Some(h.clock.now() + 30 * DAY));
}

pub fn lifetimes_per_locker(make: Make) {
    let h = make();
    let now = h.clock.now();
    assert_eq!(
        h.writer.put("short", b"x", 0).unwrap().expires,
        Some(now + 7 * DAY)
    );
    assert_eq!(
        h.writer.put("api_keys", b"x", 0).unwrap().expires,
        Some(now + 30 * DAY)
    );
}

pub fn list_reports_lockers(make: Make) {
    let h = make();
    let stored = h.writer.put("api_keys", b"x", 0).unwrap();
    let list = h.writer.list().unwrap();
    let names: Vec<&str> = list.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["api_keys", "short"]);
    let api = &list[0];
    assert_eq!(api.revision, stored.revision);
    assert_eq!(api.expires, stored.expires);
    assert!(api.read && api.write && api.size > 0);
    assert_eq!(list[1].revision, 0);
    assert_eq!(list[1].expires, None);
    assert!(h.reader.list().unwrap().iter().all(|l| l.read && !l.write));
}

/// Generates one `#[test]` per suite case.
#[macro_export]
macro_rules! backend_suite {
    ($make:path) => {
        mod backend_suite {
            use super::*;

            #[test]
            fn round_trip() {
                $crate::testing::round_trip(&$make)
            }
            #[test]
            fn empty_locker_reads_empty() {
                $crate::testing::empty_locker_reads_empty(&$make)
            }
            #[test]
            fn every_write_changes_revision() {
                $crate::testing::every_write_changes_revision(&$make)
            }
            #[test]
            fn conflicting_writers_detected() {
                $crate::testing::conflicting_writers_detected(&$make)
            }
            #[test]
            fn update_retries_after_a_conflict() {
                $crate::testing::update_retries_after_a_conflict(&$make)
            }
            #[test]
            fn read_only_binary_cannot_put() {
                $crate::testing::read_only_binary_cannot_put(&$make)
            }
            #[test]
            fn unknown_locker_refused() {
                $crate::testing::unknown_locker_refused(&$make)
            }
            #[test]
            fn invalid_name_refused() {
                $crate::testing::invalid_name_refused(&$make)
            }
            #[test]
            fn empty_put_deletes() {
                $crate::testing::empty_put_deletes(&$make)
            }
            #[test]
            fn size_limit() {
                $crate::testing::size_limit(&$make)
            }
            #[test]
            fn expiry_set_at_fill_and_not_extended() {
                $crate::testing::expiry_set_at_fill_and_not_extended(&$make)
            }
            #[test]
            fn expired_locker_reads_empty() {
                $crate::testing::expired_locker_reads_empty(&$make)
            }
            #[test]
            fn lifetimes_per_locker() {
                $crate::testing::lifetimes_per_locker(&$make)
            }
            #[test]
            fn list_reports_lockers() {
                $crate::testing::list_reports_lockers(&$make)
            }
        }
    };
}
