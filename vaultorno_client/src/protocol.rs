//! The wire protocol and statement types, shared by the client and the
//! daemon so they can't drift apart (design doc §5, §9).
//!
//! Framing: a 4-byte big-endian length, then UTF-8 JSON. Binary fields are
//! base64. Frames over [`MAX_FRAME`] close the connection.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::marker::PhantomData;

use serde::de::{DeserializeOwned, Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

use crate::names::is_valid_name;
use crate::time::{DAY, parse_rfc3339};

/// Protocol version spoken by this build.
pub const PROTOCOL: u32 = 1;
/// Largest frame, in bytes of JSON.
pub const MAX_FRAME: usize = 512 * 1024;
/// Where the daemon listens on Linux.
pub const DEFAULT_SOCKET: &str = "/run/vaultorno.sock";

/// First four bytes of every client envelope (§6).
pub const ENVELOPE_MAGIC: [u8; 4] = *b"VLT1";
/// Smallest stored envelope.
pub const MIN_STORED: usize = 1024;
/// Largest stored envelope.
pub const MAX_STORED: usize = 256 * 1024;
/// Envelope bytes that aren't app data: magic, key id, nonce, length, tag.
pub const ENVELOPE_OVERHEAD: usize = 64;
/// Most app data one locker holds.
pub const MAX_DATA: usize = MAX_STORED - ENVELOPE_OVERHEAD;

/// Most locker names one app may use.
pub const MAX_LOCKERS_PER_APP: usize = 16;
/// Lifetime of a written locker when its statement doesn't say.
pub const DEFAULT_LIFETIME_DAYS: u32 = 30;
/// Longest lifetime any locker may have.
pub const MAX_LIFETIME_DAYS: u32 = 90;
/// Longest a statement may be valid.
pub const MAX_STATEMENT_DAYS: u64 = 180;
/// Length of each half of an app key.
pub const HALF_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Messages

/// One message in either direction. Responses echo the request's `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame<T> {
    pub id: u32,
    pub body: T,
}

/// What a client asks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Always first. Identity is checked once, here.
    Hello {
        protocol: u32,
        /// The statement file (§5), as its exact text.
        #[serde(default)]
        statement: Option<String>,
        /// Its SSHSIG (checked from Milestone 3).
        #[serde(default)]
        signature: Option<String>,
        /// A succession statement with its signature (§8; Milestone 3).
        #[serde(default)]
        succession: Option<String>,
    },
    List,
    Get {
        locker: String,
    },
    /// `data` is a client envelope (§6); empty `data` empties the locker.
    Put {
        locker: String,
        #[serde(with = "b64")]
        data: Vec<u8>,
        expected: u64,
    },
}

/// What the daemon answers.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Welcome {
        protocol: u32,
        app: String,
        vault: String,
        binary: String,
        level: String,
        sealer: String,
        /// The daemon's half of this vault's app key (§6).
        #[serde(with = "b64")]
        half: Vec<u8>,
        /// Unix seconds; none for package manifests.
        statement_expires: Option<u64>,
        /// Refused opens of this vault since its last successful open.
        refused: u32,
    },
    Lockers {
        lockers: Vec<LockerInfo>,
    },
    /// An empty locker has no data and revision 0.
    Locker {
        #[serde(with = "b64")]
        data: Vec<u8>,
        revision: u64,
        expires: Option<u64>,
    },
    /// `revision` 0 and `expires` 0 after emptying a locker.
    Stored {
        revision: u64,
        expires: u64,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

impl fmt::Debug for Response {
    // Never prints the daemon's half.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Response::Welcome {
                app,
                vault,
                binary,
                level,
                refused,
                ..
            } => f
                .debug_struct("Welcome")
                .field("app", app)
                .field("vault", vault)
                .field("binary", binary)
                .field("level", level)
                .field("half", &"<redacted>")
                .field("refused", refused)
                .finish_non_exhaustive(),
            Response::Lockers { lockers } => {
                f.debug_struct("Lockers").field("lockers", lockers).finish()
            }
            Response::Locker {
                data,
                revision,
                expires,
            } => f
                .debug_struct("Locker")
                .field("data_len", &data.len())
                .field("revision", revision)
                .field("expires", expires)
                .finish(),
            Response::Stored { revision, expires } => f
                .debug_struct("Stored")
                .field("revision", revision)
                .field("expires", expires)
                .finish(),
            Response::Error { code, message } => f
                .debug_struct("Error")
                .field("code", code)
                .field("message", message)
                .finish(),
        }
    }
}

/// One locker, as `List` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockerInfo {
    pub name: String,
    /// 0 when empty.
    pub revision: u64,
    /// Stored size class, in bytes; 0 when empty.
    pub size: u32,
    /// Unix seconds; none when empty.
    pub expires: Option<u64>,
    pub read: bool,
    pub write: bool,
}

/// Why a request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProtocolMismatch,
    NotAllowed,
    UnknownLocker,
    Conflict,
    TooLarge,
    NotSealed,
    Storage,
    Busy,
}

mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text.as_bytes()).map_err(D::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Framing

/// A frame couldn't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("frame larger than {MAX_FRAME} bytes")]
    TooLarge,
    #[error("malformed frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// Writes one frame.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, frame: &Frame<T>) -> Result<(), FrameError> {
    let body = Zeroizing::new(serde_json::to_vec(frame)?);
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    let mut out = Zeroizing::new(Vec::with_capacity(4 + body.len()));
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()?;
    Ok(())
}

/// Reads one frame; `None` when the peer closed the connection cleanly
/// between frames.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<Option<Frame<T>>, FrameError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    let mut body = Zeroizing::new(vec![0u8; len]);
    r.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

// ---------------------------------------------------------------------------
// Envelope shape

/// Why the daemon refuses a `Put` without decrypting anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    TooLarge,
    NotSealed,
}

/// The daemon's shape check (§6): magic, and a size that is a power of two
/// between [`MIN_STORED`] and [`MAX_STORED`]. It catches a client that
/// forgot to encrypt; it can't prove the bytes are encrypted.
pub fn check_envelope_shape(data: &[u8]) -> Result<(), ShapeError> {
    if data.len() > MAX_STORED {
        return Err(ShapeError::TooLarge);
    }
    if data.len() < MIN_STORED || !data.len().is_power_of_two() || data[..4] != ENVELOPE_MAGIC {
        return Err(ShapeError::NotSealed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Statements

/// A statement (§5): which executable it covers, which lockers that binary
/// may use, and until when. Parsed only after its signature checks out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub format: u32,
    pub app: String,
    pub binary: String,
    pub sha256: String,
    pub issued: String,
    pub expires: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_env: Vec<String>,
    #[serde(deserialize_with = "unique_keys")]
    pub lockers: BTreeMap<String, LockerGrant>,
}

/// One locker in a statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockerGrant {
    pub access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_days: Option<u32>,
}

/// Read, or write (which includes read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

/// A statement that passed every check.
#[derive(Debug, Clone)]
pub struct CheckedStatement {
    pub app: String,
    pub binary: String,
    pub sha256: String,
    pub issued: u64,
    pub expires: u64,
    pub level: Option<String>,
    pub reject_env: Vec<String>,
    pub lockers: BTreeMap<String, Grant>,
}

/// What one binary may do with one locker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    pub write: bool,
    /// Seconds a filled locker lives; 0 for read-only grants.
    pub lifetime_secs: u64,
}

/// A statement was refused; the message says why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct StatementError(pub String);

fn refuse<T>(msg: impl Into<String>) -> Result<T, StatementError> {
    Err(StatementError(msg.into()))
}

impl Statement {
    /// Parses a statement's exact text. Unknown fields and duplicate keys are
    /// refused.
    pub fn parse(text: &str) -> Result<Statement, StatementError> {
        serde_json::from_str(text).map_err(|e| StatementError(format!("malformed statement: {e}")))
    }

    /// Checks everything that doesn't need a signature or a hash: format,
    /// names, times against `now`, levels, variables and lockers.
    pub fn check(&self, now: u64) -> Result<CheckedStatement, StatementError> {
        if self.format != 1 {
            return refuse(format!("statement format {} isn't supported", self.format));
        }
        for (what, name) in [("app", &self.app), ("binary", &self.binary)] {
            if !is_valid_name(name) {
                return refuse(format!("{what} name {name:?} isn't [a-z0-9_]{{1,32}}"));
            }
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return refuse("sha256 must be 64 lowercase hex digits");
        }
        let Some(issued) = parse_rfc3339(&self.issued) else {
            return refuse("issued must be YYYY-MM-DDTHH:MM:SSZ");
        };
        let Some(expires) = parse_rfc3339(&self.expires) else {
            return refuse("expires must be YYYY-MM-DDTHH:MM:SSZ");
        };
        if expires <= issued {
            return refuse("statement expires before it is issued");
        }
        if expires - issued > MAX_STATEMENT_DAYS * DAY {
            return refuse(format!(
                "statement is valid for more than {MAX_STATEMENT_DAYS} days"
            ));
        }
        if now < issued {
            return refuse(format!("statement isn't valid until {}", self.issued));
        }
        if now >= expires {
            return refuse(format!("statement expired on {}", &self.expires[..10]));
        }
        match self.level.as_deref() {
            None | Some("airtight") | Some("hermetic") => {}
            Some(other) => return refuse(format!("level {other:?} isn't airtight or hermetic")),
        }
        for var in &self.reject_env {
            if !is_valid_env_pattern(var) {
                return refuse(format!("reject_env entry {var:?} isn't a variable name"));
            }
        }
        let lockers = check_locker_grants(&self.lockers)?;
        Ok(CheckedStatement {
            app: self.app.clone(),
            binary: self.binary.clone(),
            sha256: self.sha256.clone(),
            issued,
            expires,
            level: self.level.clone(),
            reject_env: self.reject_env.clone(),
            lockers,
        })
    }
}

// Package manifests (§5)

/// Where root-owned package manifests live on Linux (§5).
pub const DEFAULT_MANIFESTS_DIR: &str = "/usr/share/vaultorno/manifests";

/// Built-in environment variable patterns refused at every level (§7).
pub const BUILTIN_REJECT_ENV: &[&str] = &[
    "LD_*",
    "VK_*",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "LIBGL_DRIVERS_PATH",
    "GBM_BACKENDS_PATH",
    "GTK_MODULES",
    "GTK_PATH",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
];

/// A root-owned package manifest (`/usr/share/vaultorno/manifests/<app>.json`, §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub app: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_env: Vec<String>,
    #[serde(deserialize_with = "unique_keys")]
    pub binaries: BTreeMap<String, ManifestBinary>,
}

/// One binary entry inside a [`Manifest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestBinary {
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_env: Vec<String>,
    #[serde(deserialize_with = "unique_keys")]
    pub lockers: BTreeMap<String, LockerGrant>,
}

/// A manifest that passed format, name, locker and path checks.
#[derive(Debug, Clone)]
pub struct CheckedManifest {
    pub app: String,
    pub reject_env: Vec<String>,
    pub binaries: BTreeMap<String, CheckedManifestBinary>,
}

/// One checked binary inside a [`CheckedManifest`].
#[derive(Debug, Clone)]
pub struct CheckedManifestBinary {
    pub path: String,
    pub reject_env: Vec<String>,
    pub lockers: BTreeMap<String, Grant>,
}

impl Manifest {
    /// Parses a manifest's exact text. Unknown fields and duplicate keys are
    /// refused.
    pub fn parse(text: &str) -> Result<Manifest, StatementError> {
        serde_json::from_str(text).map_err(|e| StatementError(format!("malformed manifest: {e}")))
    }

    /// Checks format, names, paths, `reject_env`, and locker grants.
    pub fn check(&self) -> Result<CheckedManifest, StatementError> {
        if self.format != 1 {
            return refuse(format!("manifest format {} isn't supported", self.format));
        }
        if !is_valid_name(&self.app) {
            return refuse(format!("app name {:?} isn't [a-z0-9_]{{1,32}}", self.app));
        }
        for var in &self.reject_env {
            if !is_valid_env_pattern(var) {
                return refuse(format!("reject_env entry {var:?} isn't a variable name"));
            }
        }
        if self.binaries.is_empty() {
            return refuse("manifest names no binaries");
        }
        let mut all_lockers = std::collections::BTreeSet::new();
        let mut binaries = BTreeMap::new();
        for (bin_name, bin) in &self.binaries {
            if !is_valid_name(bin_name) {
                return refuse(format!(
                    "binary name {bin_name:?} isn't [a-z0-9_]{{1,32}}"
                ));
            }
            if !bin.path.starts_with('/') || bin.path.len() < 2 || bin.path.ends_with('/') {
                return refuse(format!(
                    "binary {bin_name}: path {:?} must be an absolute file path",
                    bin.path
                ));
            }
            for var in &bin.reject_env {
                if !is_valid_env_pattern(var) {
                    return refuse(format!("reject_env entry {var:?} isn't a variable name"));
                }
            }
            let lockers = check_locker_grants(&bin.lockers)?;
            all_lockers.extend(lockers.keys().cloned());
            if all_lockers.len() > MAX_LOCKERS_PER_APP {
                return refuse(format!(
                    "manifest names more than {MAX_LOCKERS_PER_APP} lockers"
                ));
            }
            binaries.insert(
                bin_name.clone(),
                CheckedManifestBinary {
                    path: bin.path.clone(),
                    reject_env: bin.reject_env.clone(),
                    lockers,
                },
            );
        }
        Ok(CheckedManifest {
            app: self.app.clone(),
            reject_env: self.reject_env.clone(),
            binaries,
        })
    }
}

fn check_locker_grants(
    raw: &BTreeMap<String, LockerGrant>,
) -> Result<BTreeMap<String, Grant>, StatementError> {
    if raw.is_empty() {
        return refuse("statement names no lockers");
    }
    if raw.len() > MAX_LOCKERS_PER_APP {
        return refuse(format!(
            "statement names more than {MAX_LOCKERS_PER_APP} lockers"
        ));
    }
    let mut lockers = BTreeMap::new();
    for (name, grant) in raw {
        if !is_valid_name(name) {
            return refuse(format!("locker name {name:?} isn't [a-z0-9_]{{1,32}}"));
        }
        let checked = match (grant.access, grant.lifetime_days) {
            (Access::Read, None) => Grant {
                write: false,
                lifetime_secs: 0,
            },
            (Access::Read, Some(_)) => {
                return refuse(format!(
                    "{name}: lifetime_days only applies to write access"
                ));
            }
            (Access::Write, days) => {
                let days = days.unwrap_or(DEFAULT_LIFETIME_DAYS);
                if days == 0 || days > MAX_LIFETIME_DAYS {
                    return refuse(format!(
                        "{name}: lifetime_days must be 1 to {MAX_LIFETIME_DAYS}"
                    ));
                }
                Grant {
                    write: true,
                    lifetime_secs: u64::from(days) * DAY,
                }
            }
        };
        lockers.insert(name.clone(), checked);
    }
    Ok(lockers)
}

/// Whether environment variable `name` matches `pattern` (`NAME` or `PREFIX*`, §7).
pub fn env_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// First rejected variable in `env_names` matching [`BUILTIN_REJECT_ENV`] or
/// `extra_patterns`, if any.
pub fn find_rejected_env<'a>(
    env_names: &'a [String],
    extra_patterns: &[String],
) -> Option<&'a str> {
    for var in env_names {
        if BUILTIN_REJECT_ENV.iter().any(|p| env_matches(p, var))
            || extra_patterns.iter().any(|p| env_matches(p, var))
        {
            return Some(var.as_str());
        }
    }
    None
}

/// `NAME` or `PREFIX*`, as in the built-in reject list (§7).
pub fn is_valid_env_pattern(var: &str) -> bool {
    let name = var.strip_suffix('*').unwrap_or(var);
    !name.is_empty()
        && name.len() <= 128
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A map that refuses duplicate keys, which `serde_json` would otherwise
/// accept with the last one winning.
fn unique_keys<'de, D, V>(d: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueKeys<V>(PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for UniqueKeys<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map with unique keys")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, V>()? {
                if out.contains_key(&key) {
                    return Err(A::Error::custom(format!("duplicate locker {key:?}")));
                }
                out.insert(key, value);
            }
            Ok(out)
        }
    }

    d.deserialize_map(UniqueKeys(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::format_rfc3339;

    const NOW: u64 = 1_792_108_800; // 2026-10-16

    fn text(extra: &str, lockers: &str) -> String {
        format!(
            r#"{{"format":1,"app":"inforno","binary":"autorno","sha256":"{}","issued":"{}","expires":"{}"{extra},"lockers":{lockers}}}"#,
            "ab".repeat(32),
            format_rfc3339(NOW),
            format_rfc3339(NOW + 90 * DAY),
        )
    }

    fn check(extra: &str, lockers: &str) -> Result<CheckedStatement, StatementError> {
        Statement::parse(&text(extra, lockers))?.check(NOW)
    }

    #[test]
    fn accepts_a_good_statement() {
        let c = check(
            "",
            r#"{"api_keys":{"access":"read"},"short":{"access":"write","lifetime_days":7}}"#,
        )
        .unwrap();
        assert_eq!(
            c.lockers["api_keys"],
            Grant {
                write: false,
                lifetime_secs: 0
            }
        );
        assert_eq!(
            c.lockers["short"],
            Grant {
                write: true,
                lifetime_secs: 7 * DAY
            }
        );
        let c = check("", r#"{"api_keys":{"access":"write"}}"#).unwrap();
        assert_eq!(c.lockers["api_keys"].lifetime_secs, 30 * DAY);
    }

    #[test]
    fn refuses_unknown_fields_and_duplicates() {
        assert!(check(r#","extra":1"#, r#"{"a":{"access":"read"}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"read","why":1}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"read"},"a":{"access":"write"}}"#).is_err());
        assert!(check(r#","app":"other""#, r#"{"a":{"access":"read"}}"#).is_err());
    }

    #[test]
    fn refuses_bad_values() {
        assert!(check("", r#"{}"#).is_err());
        assert!(check("", r#"{"Api-Keys":{"access":"read"}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"read","lifetime_days":3}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"write","lifetime_days":0}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"write","lifetime_days":91}}"#).is_err());
        assert!(check("", r#"{"a":{"access":"admin"}}"#).is_err());
        assert!(check(r#","level":"open""#, r#"{"a":{"access":"read"}}"#).is_err());
        assert!(
            check(
                r#","reject_env":["BAD-NAME"]"#,
                r#"{"a":{"access":"read"}}"#
            )
            .is_err()
        );
        assert!(
            check(
                r#","reject_env":["MY_PLUGIN_*"]"#,
                r#"{"a":{"access":"read"}}"#
            )
            .is_ok()
        );
        let seventeen: Vec<String> = (0..17)
            .map(|i| format!(r#""l{i}":{{"access":"read"}}"#))
            .collect();
        assert!(check("", &format!("{{{}}}", seventeen.join(","))).is_err());
    }

    #[test]
    fn refuses_bad_windows() {
        let st = Statement::parse(&text("", r#"{"a":{"access":"read"}}"#)).unwrap();
        assert!(st.check(NOW - 1).is_err(), "not yet valid");
        assert!(st.check(NOW + 90 * DAY).is_err(), "expired");
        assert!(st.check(NOW + 90 * DAY - 1).is_ok());
        let mut long = st.clone();
        long.expires = format_rfc3339(NOW + 181 * DAY);
        assert!(long.check(NOW).is_err(), "valid over 180 days");
    }

    #[test]
    fn messages_round_trip() {
        let put = Frame {
            id: 7,
            body: Request::Put {
                locker: "a".into(),
                data: vec![1, 2, 255],
                expected: u64::MAX,
            },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &put).unwrap();
        let back: Frame<Request> = read_frame(&mut buf.as_slice()).unwrap().unwrap();
        match back.body {
            Request::Put { data, expected, .. } => {
                assert_eq!(data, vec![1, 2, 255]);
                assert_eq!(expected, u64::MAX);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refuses_unknown_message_fields_and_big_frames() {
        let json = br#"{"id":1,"body":{"type":"get","locker":"a","sneaky":true}}"#;
        let mut buf = (json.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(json);
        assert!(read_frame::<_, Request>(&mut buf.as_slice()).is_err());

        let huge = ((MAX_FRAME + 1) as u32).to_be_bytes();
        assert!(matches!(
            read_frame::<_, Request>(&mut huge.as_slice()),
            Err(FrameError::TooLarge)
        ));
        assert!(
            read_frame::<_, Request>(&mut [].as_slice())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shape_check() {
        let mut env = vec![0u8; 1024];
        env[..4].copy_from_slice(&ENVELOPE_MAGIC);
        assert_eq!(check_envelope_shape(&env), Ok(()));
        assert_eq!(
            check_envelope_shape(&env[..1000]),
            Err(ShapeError::NotSealed)
        );
        let mut odd = env.clone();
        odd.push(0);
        assert_eq!(check_envelope_shape(&odd), Err(ShapeError::NotSealed));
        env[0] = b'X';
        assert_eq!(check_envelope_shape(&env), Err(ShapeError::NotSealed));
        assert_eq!(
            check_envelope_shape(&vec![0u8; MAX_STORED + 1]),
            Err(ShapeError::TooLarge)
        );
        assert_eq!(
            check_envelope_shape(b"plain text"),
            Err(ShapeError::NotSealed)
        );
    }

    #[test]
    fn manifest_checks_and_refusals() {
        let good = r#"{
            "format": 1,
            "app": "inforno",
            "reject_env": ["INFORNO_PLUGIN_*"],
            "binaries": {
                "inforno": {
                    "path": "/usr/bin/inforno",
                    "lockers": { "api_keys": { "access": "write", "lifetime_days": 30 } }
                },
                "autorno": {
                    "path": "/usr/bin/autorno",
                    "lockers": { "api_keys": { "access": "read" } }
                }
            }
        }"#;
        let m = Manifest::parse(good).unwrap().check().unwrap();
        assert_eq!(m.app, "inforno");
        assert!(m.binaries["inforno"].lockers["api_keys"].write);
        assert!(!m.binaries["autorno"].lockers["api_keys"].write);

        // Relative path refused.
        let rel = r#"{"format":1,"app":"a","binaries":{"b":{"path":"bin/b","lockers":{"k":{"access":"read"}}}}}"#;
        assert!(Manifest::parse(rel).unwrap().check().is_err());

        // Duplicate binary refused.
        let dup = r#"{"format":1,"app":"a","binaries":{"b":{"path":"/bin/b","lockers":{"k":{"access":"read"}}},"b":{"path":"/bin/b2","lockers":{"k":{"access":"read"}}}}}"#;
        assert!(Manifest::parse(dup).is_err());
    }

    #[test]
    fn env_rejection_patterns() {
        let vars = vec![
            "PATH".to_owned(),
            "HOME".to_owned(),
            "VK_LAYER_PATH".to_owned(),
        ];
        assert_eq!(find_rejected_env(&vars, &[]), Some("VK_LAYER_PATH"));

        let clean = vec!["PATH".to_owned(), "CUSTOM_PLUGINS".to_owned()];
        assert_eq!(find_rejected_env(&clean, &[]), None);
        assert_eq!(
            find_rejected_env(&clean, &["CUSTOM_*".to_owned()]),
            Some("CUSTOM_PLUGINS")
        );
    }
}
