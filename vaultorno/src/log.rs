//! The log (design doc §11): per user, `users/<uid>/log` in the state
//! folder, plain text, one event per line: the time, then `key=value`
//! fields. Never contents, which the daemon doesn't have, and never the
//! daemon's halves. `log` is rotated into `log.1` and `log.2` at 1 MiB.
//!
//! The daemon, the sweep and root's admin commands all write it. Each write
//! locks the user's folder, and a file root creates gets that folder's
//! owner (§12). Values can come from callers (a locker name, a refusal
//! naming a path), so anything but plain ASCII is quoted and escaped: one
//! event is always one line.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, flock, mkdirat, openat, renameat, statat,
};
use rustix::io::Errno;
use vaultorno_client::protocol::ErrorCode;
use vaultorno_client::time::format_rfc3339;

use crate::store::{dir_flags, open_dir_beneath, own_like_dir};

/// The current log; older events are in `log.1`, then `log.2` (§11).
pub const LOG: &str = "log";
const ROTATED: [&str; 2] = ["log.1", "log.2"];
/// `log` is rotated before it grows past this.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Longest value written, in characters: a locker name in a request can be
/// as long as a frame.
const MAX_VALUE: usize = 256;

/// One event, without its newline: the time, then `key=value` fields.
pub fn line(time: u64, fields: &[(&str, &str)]) -> String {
    let mut out = format_rfc3339(time);
    for (key, value) in fields {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        push_value(&mut out, value);
    }
    out
}

/// `-` for nothing; bare when plain; otherwise quoted, with `\`, `"` and
/// every control or non-ASCII character escaped, and cut after
/// [`MAX_VALUE`] characters.
fn push_value(out: &mut String, value: &str) {
    if value.is_empty() {
        out.push('-');
        return;
    }
    let plain = value.len() <= MAX_VALUE
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./:+".contains(&b));
    if plain {
        out.push_str(value);
        return;
    }
    out.push('"');
    for (i, c) in value.chars().enumerate() {
        if i == MAX_VALUE {
            out.push_str("...");
            break;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
        }
    }
    out.push('"');
}

/// An error code, as the log names it.
pub fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::ProtocolMismatch => "protocol_mismatch",
        ErrorCode::NotAllowed => "not_allowed",
        ErrorCode::UnknownLocker => "unknown_locker",
        ErrorCode::Conflict => "conflict",
        ErrorCode::TooLarge => "too_large",
        ErrorCode::NotSealed => "not_sealed",
        ErrorCode::Storage => "storage_error",
        ErrorCode::Busy => "busy",
        ErrorCode::Unavailable => "unavailable",
    }
}

/// How a vault appears in the log. A signing key's vault is
/// `<app>/<key id>`, or its key id alone before the app is known; a `pkg_`
/// or `test_` vault is its name, which already says which app.
pub fn vault_label(app: Option<&str>, vault: &str) -> String {
    if vault.len() == 64 && vault.bytes().all(|b| b.is_ascii_hexdigit()) {
        let key_id = &vault[..16];
        match app {
            Some(app) => format!("{app}/{key_id}"),
            None => key_id.to_owned(),
        }
    } else {
        vault.to_owned()
    }
}

/// Appends one event to `uid`'s log in `state_dir`. A failure is reported
/// on standard error, without the event, and never stops the request it
/// describes.
pub fn record(state_dir: &Path, uid: u32, time: u64, fields: &[(&str, &str)]) {
    if let Err(e) = append(state_dir, uid, &line(time, fields)) {
        eprintln!("vaultorno: can't write uid {uid}'s log: {e}");
    }
}

/// Appends `line` to `uid`'s log, creating the user's folder if needed and
/// rotating first if the log would pass [`MAX_LOG_BYTES`].
pub fn append(state_dir: &Path, uid: u32, line: &str) -> io::Result<()> {
    let users = open_dir_beneath(state_dir, &["users"])?;
    let name = uid.to_string();
    let dir = match openat(&users, name.as_str(), dir_flags(), Mode::empty()) {
        Err(Errno::NOENT) => {
            match mkdirat(&users, name.as_str(), Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
            let dir = openat(&users, name.as_str(), dir_flags(), Mode::empty())?;
            own_like_dir(&users, &dir)?;
            dir
        }
        other => other?,
    };
    // Released when `dir` is closed.
    flock(&dir, FlockOperation::LockExclusive)?;

    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    let size = match statat(&dir, LOG, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st.st_size as u64,
        Err(Errno::NOENT) => 0,
        Err(e) => return Err(e.into()),
    };
    if size > 0 && size + bytes.len() as u64 > MAX_LOG_BYTES {
        match renameat(&dir, ROTATED[0], &dir, ROTATED[1]) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        renameat(&dir, LOG, &dir, ROTATED[0])?;
    }
    // NONBLOCK: a FIFO planted as `log` fails to open instead of hanging.
    let flags = OFlags::WRONLY
        | OFlags::APPEND
        | OFlags::CREATE
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC;
    let mut file = File::from(openat(&dir, LOG, flags, Mode::from_raw_mode(0o600))?);
    check_plain_file(&file, LOG)?;
    own_like_dir(&dir, &file)?;
    file.write_all(&bytes)
}

/// `uid`'s log, oldest first: `log.2`, `log.1`, then `log`. Empty if the
/// user has none.
pub fn read(state_dir: &Path, uid: u32) -> io::Result<Vec<u8>> {
    let name = uid.to_string();
    let dir = match open_dir_beneath(state_dir, &["users", name.as_str()]) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut out = Vec::new();
    for name in [ROTATED[1], ROTATED[0], LOG] {
        let file = match openat(&dir, name, flags, Mode::empty()) {
            Err(Errno::NOENT) => continue,
            other => File::from(other?),
        };
        check_plain_file(&file, name)?;
        file.take(2 * MAX_LOG_BYTES).read_to_end(&mut out)?;
    }
    Ok(out)
}

/// A regular file with one link: never a planted device, FIFO or hard link
/// to a file elsewhere.
fn check_plain_file(file: &File, name: &str) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.nlink() != 1 {
        return Err(io::Error::other(format!(
            "{name} isn't a regular file with one link"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn values_are_escaped_and_one_event_is_one_line() {
        let l = line(
            0,
            &[
                ("locker", "api_keys"),
                ("check", "say \"hi\"\nnext"),
                ("binary", ""),
                ("path", "/tmp/\u{202e}x"),
            ],
        );
        assert_eq!(
            l,
            r#"1970-01-01T00:00:00Z locker=api_keys check="say \"hi\"\u{a}next" binary=- path="/tmp/\u{202e}x""#
        );
        let long = line(0, &[("locker", &"a".repeat(1000))]);
        assert!(long.len() < 300 && long.ends_with("...\""), "{long}");
    }

    #[test]
    fn rotates_at_1_mib_and_keeps_two() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("users")).unwrap();
        let filler = "x".repeat(1000);
        for i in 0..3500 {
            append(dir.path(), 7, &format!("{i:05} {filler}")).unwrap();
        }
        let user = dir.path().join("users/7");
        for name in [LOG, ROTATED[0], ROTATED[1]] {
            let len = fs::metadata(user.join(name)).unwrap().len();
            assert!(len > 0 && len <= MAX_LOG_BYTES, "{name}: {len}");
        }
        assert!(!user.join("log.3").exists());
        let text = String::from_utf8(read(dir.path(), 7).unwrap()).unwrap();
        assert!(text.ends_with(&format!("03499 {filler}\n")));
        assert!(!text.starts_with("00000 "), "the oldest events are gone");
    }

    #[test]
    fn never_writes_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("users/7")).unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, b"keep\n").unwrap();
        let log = dir.path().join("users/7/log");
        std::os::unix::fs::symlink(&target, &log).unwrap();
        assert!(append(dir.path(), 7, "event").is_err());
        fs::remove_file(&log).unwrap();
        fs::hard_link(&target, &log).unwrap();
        assert!(append(dir.path(), 7, "event").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep\n");
    }
}
