//! `vaultorno_inspect` (design doc §3, §7, §14): the process inspector.
//!
//! Kept in its own crate depending only on `rustix` and `sha2` so everything
//! `vaultorno inspect` can reach is small and self-contained.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg, sockopt,
};
use rustix::process::{Pid, PidfdFlags, pidfd_open};
use sha2::{Digest, Sha256};

/// Default socket where `vaultorno inspect` listens (§3, §14).
pub const DEFAULT_INSPECT_SOCKET: &str = "/run/vaultorno_inspect.sock";

/// Fixed 8-byte request sent with the pidfd in `SCM_RIGHTS` (§3, §14).
const REQUEST_MAGIC: [u8; 8] = *b"VINS\x00\x00\x00\x01";

/// Maximum reply frame length (1 MiB): a GUI maps a few hundred libraries.
const MAX_REPLY: usize = 1024 * 1024;

/// Maximum `/proc/<pid>/maps` bytes read (8 MiB).
const MAX_MAPS: usize = 8 * 1024 * 1024;

/// Most files mapped as code that an inspection reports.
const MAX_CODE: usize = 4096;

/// IO timeout on a single inspector request.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// What the inspector reports about a caller process (§7, §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    pub pid: u32,
    pub uid: u32,
    pub exe_path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub sha256: [u8; 32],
    /// Every file the process maps executable, its executable included: the
    /// code it runs (§7.5).
    pub code: Vec<MappedCode>,
}

/// A file mapped executable into a process, from `/proc/<pid>/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedCode {
    pub path: PathBuf,
    pub ino: u64,
    /// Deleted or replaced since it was mapped (` (deleted)` in `maps`).
    pub deleted: bool,
}

impl Inspected {
    /// Lowercase hex SHA-256 of the executable.
    pub fn sha256_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.sha256 {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
        }
        s
    }
}

/// Acquires a pidfd for the peer of `stream`: tries `pidfd_open` on the peer
/// PID from `SO_PEERCRED` (and checks the process is alive).
pub fn peer_pidfd(stream: &UnixStream) -> io::Result<(u32, i32, OwnedFd)> {
    let cred = sockopt::socket_peercred(stream).map_err(io::Error::from)?;
    let uid = cred.uid.as_raw();
    let pid_raw = cred.pid.as_raw_nonzero().get();
    let pid = Pid::from_raw(pid_raw).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "peer has no valid pid")
    })?;
    let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(io::Error::from)?;
    Ok((uid, pid_raw, pidfd))
}

/// Inspects the process referred to by `pidfd` in-process, verifying through
/// `pidfd` afterwards that the process is still alive so PID reuse cannot
/// substitute another process (§7.3).
pub fn inspect_pidfd(pidfd: BorrowedFd<'_>) -> Result<Inspected, String> {
    let pid = pid_from_fdinfo(pidfd)?;
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));

    let uid = read_proc_uid(&proc_dir.join("status"))?;
    let exe_link = proc_dir.join("exe");
    let raw_target = fs::read_link(&exe_link)
        .map_err(|e| format!("can't read {}: {e}", exe_link.display()))?;
    let exe_path = strip_deleted_suffix(&raw_target);

    let mut exe_file = File::open(&exe_link)
        .map_err(|e| format!("can't open {}: {e}", exe_link.display()))?;
    let meta = exe_file
        .metadata()
        .map_err(|e| format!("can't stat {}: {e}", exe_link.display()))?;
    let dev = meta.dev();
    let ino = meta.ino();

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = exe_file
            .read(&mut buf)
            .map_err(|e| format!("can't read executable: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha256: [u8; 32] = hasher.finalize().into();

    // What else it runs (§7.5). Unlike its environment, which only its own
    // user may read, `maps` opens with CAP_SYS_PTRACE alone.
    let code = read_mapped_code(&proc_dir.join("maps"))?;

    // Pin to the exact process: if the original process exited before or
    // during our `/proc/<pid>` reads, its pidfd fdinfo reports Pid: -1 (or 0),
    // ruling out PID reuse (§7.3).
    if pid_from_fdinfo(pidfd)? != pid {
        return Err("caller process exited during inspection".into());
    }

    Ok(Inspected {
        pid,
        uid,
        exe_path,
        dev,
        ino,
        sha256,
        code,
    })
}

/// Asks the inspector listening at `socket` to inspect `pidfd`, and refuses
/// a reply from any process not running as `inspector_uid` (§3).
///
/// `SO_PEERCRED` can't tell: on a socket systemd activated, it names whoever
/// called `listen()`, which is PID 1. Instead this side sets `SO_PASSCRED`,
/// and the kernel attaches the replying process's own credentials to the
/// reply, which it can't change without `CAP_SETUID`.
pub fn request_inspection(
    socket: &Path,
    pidfd: BorrowedFd<'_>,
    inspector_uid: u32,
) -> Result<Inspected, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("can't connect to inspector at {}: {e}", socket.display()))?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    sockopt::set_socket_passcred(&stream, true)
        .map_err(|e| format!("can't ask for the inspector's credentials: {e}"))?;
    send_request(&stream, pidfd)?;
    read_reply(&stream, inspector_uid)
}

/// A path systemd sets up for one service user, whose owner names that user
/// without a lookup by name, which would need NSS (§3): `/var/lib/vaultorno`
/// (`StateDirectory=`) names the daemon's user, `/run/vaultorno_inspect.sock`
/// (`SocketUser=`) the inspector's. Refused unless the path is absolute and
/// not a symlink, and every folder above it is a real folder owned by root
/// or by the same owner. The folder holding it must not be writable by group
/// or others; folders further up may be only if they are root's and sticky,
/// like `/tmp`.
pub fn anchor(path: &Path) -> Result<fs::Metadata, String> {
    if !path.is_absolute() {
        return Err(format!("{} isn't an absolute path", path.display()));
    }
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("can't stat {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    let owner = meta.uid();
    for (depth, dir) in path.ancestors().skip(1).enumerate() {
        let m =
            fs::symlink_metadata(dir).map_err(|e| format!("can't stat {}: {e}", dir.display()))?;
        if !m.file_type().is_dir() {
            return Err(format!("{} isn't a folder", dir.display()));
        }
        if m.uid() != 0 && m.uid() != owner {
            return Err(format!(
                "{} is owned by uid {}, neither root nor uid {owner}",
                dir.display(),
                m.uid()
            ));
        }
        let mode = m.mode();
        let sticky_root = depth > 0 && m.uid() == 0 && mode & 0o1000 != 0;
        if mode & 0o022 != 0 && !sticky_root {
            return Err(format!(
                "{} has mode {:o}; it must not be writable by group or others",
                dir.display(),
                mode & 0o7777
            ));
        }
    }
    Ok(meta)
}

/// Runs the inspector on `listener`, which systemd made (§14). A connection
/// must come from the owner of `daemon_anchor`, the daemon's state folder,
/// looked up again for each one (§3).
pub fn serve_inspect(listener: UnixListener, daemon_anchor: &Path) -> io::Result<()> {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // E.g. out of file descriptors: don't spin.
            thread::sleep(Duration::from_millis(100));
            continue;
        };
        let daemon_uid = anchor(daemon_anchor).and_then(|m| {
            if m.file_type().is_dir() {
                Ok(m.uid())
            } else {
                Err(format!("{} isn't a folder", daemon_anchor.display()))
            }
        });
        let _ = match daemon_uid {
            Ok(uid) => handle_client(stream, uid),
            Err(msg) => refuse_client(stream, msg),
        };
    }
    Ok(())
}

fn refuse_client(mut stream: UnixStream, msg: String) -> io::Result<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    write_reply(&mut stream, Err(msg))
}

/// Handles one connection from the daemon on `stream`.
pub fn handle_client(mut stream: UnixStream, allowed_daemon_uid: u32) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let cred = sockopt::socket_peercred(&stream).map_err(io::Error::from)?;
    let peer_uid = cred.uid.as_raw();
    if peer_uid != allowed_daemon_uid {
        let _ = write_reply(
            &mut stream,
            Err(format!("peer uid {peer_uid} isn't allowed")),
        );
        return Ok(());
    }

    let pidfd = match recv_request(&stream) {
        Ok(fd) => fd,
        Err(msg) => {
            let _ = write_reply(&mut stream, Err(msg));
            return Ok(());
        }
    };
    let result = inspect_pidfd(pidfd.as_fd());
    write_reply(&mut stream, result)
}

// ---------------------------------------------------------------------------
// Wire framing over `/run/vaultorno_inspect.sock`

fn send_request(stream: &UnixStream, pidfd: BorrowedFd<'_>) -> Result<(), String> {
    let fds = [pidfd];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut cmsg = SendAncillaryBuffer::new(&mut space);
    if !cmsg.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err("failed to attach pidfd to SCM_RIGHTS".into());
    }
    let iov = [IoSlice::new(&REQUEST_MAGIC)];
    let sent = sendmsg(stream, &iov, &mut cmsg, SendFlags::empty())
        .map_err(|e| format!("failed to send pidfd to inspector: {e}"))?;
    if sent != REQUEST_MAGIC.len() {
        return Err("short write to inspector".into());
    }
    Ok(())
}

fn recv_request(stream: &UnixStream) -> Result<OwnedFd, String> {
    let mut buf = [0u8; 8];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut cmsg = RecvAncillaryBuffer::new(&mut space);
    let mut iov = [IoSliceMut::new(&mut buf)];
    let msg = recvmsg(stream, &mut iov, &mut cmsg, RecvFlags::CMSG_CLOEXEC)
        .map_err(|e| format!("failed to read request: {e}"))?;
    if msg.bytes != REQUEST_MAGIC.len() || buf != REQUEST_MAGIC {
        return Err("malformed inspector request".into());
    }
    let mut received: Option<OwnedFd> = None;
    for item in cmsg.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = item {
            for fd in fds {
                if received.is_none() {
                    received = Some(fd);
                }
            }
        }
    }
    received.ok_or_else(|| "inspector request carried no pidfd".into())
}

fn write_reply(w: &mut impl Write, result: Result<Inspected, String>) -> io::Result<()> {
    let mut payload = encode_reply(&result);
    if payload.len() > MAX_REPLY {
        payload = encode_reply(&Err("the inspection is too large to report".into()));
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    w.write_all(&frame)?;
    w.flush()
}

/// Reads the reply, refusing it unless the credentials the kernel attached
/// to it are `inspector_uid`'s (see [`request_inspection`]).
fn read_reply(stream: &UnixStream, inspector_uid: u32) -> Result<Inspected, String> {
    let mut len_bytes = [0u8; 4];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
    let mut cmsg = RecvAncillaryBuffer::new(&mut space);
    let got = {
        let mut iov = [IoSliceMut::new(&mut len_bytes)];
        recvmsg(stream, &mut iov, &mut cmsg, RecvFlags::CMSG_CLOEXEC)
            .map_err(|e| format!("failed to read inspector reply: {e}"))?
            .bytes
    };
    let mut sender = None;
    for item in cmsg.drain() {
        if let RecvAncillaryMessage::ScmCredentials(cred) = item {
            sender = Some(cred.uid.as_raw());
        }
    }
    if got == 0 {
        return Err("the inspector closed without a reply".into());
    }
    match sender {
        None => return Err("the inspector's reply carried no credentials".into()),
        Some(uid) if uid != inspector_uid => {
            return Err(format!(
                "the inspector runs as uid {uid}, not uid {inspector_uid}"
            ));
        }
        Some(_) => {}
    }
    let mut r = stream;
    r.read_exact(&mut len_bytes[got..])
        .map_err(|e| format!("failed to read inspector reply: {e}"))?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_REPLY {
        return Err(format!("invalid inspector reply length {len}"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)
        .map_err(|e| format!("truncated inspector reply: {e}"))?;
    decode_reply(&buf)
}

fn encode_reply(result: &Result<Inspected, String>) -> Vec<u8> {
    let mut out = Vec::new();
    match result {
        Ok(ins) => {
            out.push(0);
            out.extend_from_slice(&ins.pid.to_be_bytes());
            out.extend_from_slice(&ins.uid.to_be_bytes());
            out.extend_from_slice(&ins.dev.to_be_bytes());
            out.extend_from_slice(&ins.ino.to_be_bytes());
            out.extend_from_slice(&ins.sha256);
            let path_bytes = ins.exe_path.as_os_str().as_bytes();
            out.extend_from_slice(&(path_bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(path_bytes);
            out.extend_from_slice(&(ins.code.len() as u32).to_be_bytes());
            for code in &ins.code {
                out.extend_from_slice(&code.ino.to_be_bytes());
                out.push(u8::from(code.deleted));
                let path = code.path.as_os_str().as_bytes();
                out.extend_from_slice(&(path.len() as u32).to_be_bytes());
                out.extend_from_slice(path);
            }
        }
        Err(msg) => {
            out.push(1);
            let bytes = msg.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
    }
    out
}

fn decode_reply(mut b: &[u8]) -> Result<Inspected, String> {
    let take = |b: &mut &[u8], n: usize| -> Result<Vec<u8>, String> {
        if b.len() < n {
            return Err("truncated inspector reply".into());
        }
        let (head, tail) = b.split_at(n);
        *b = tail;
        Ok(head.to_vec())
    };
    let status = take(&mut b, 1)?[0];
    if status != 0 {
        let n = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        let msg = String::from_utf8(take(&mut b, n)?)
            .map_err(|_| "non-utf8 inspector error".to_owned())?;
        return Err(msg);
    }
    let pid = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap());
    let uid = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap());
    let dev = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
    let ino = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
    let sha256: [u8; 32] = take(&mut b, 32)?.try_into().unwrap();
    let path_len = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
    let path_bytes = take(&mut b, path_len)?;
    let exe_path = PathBuf::from(OsStr::from_bytes(&path_bytes));
    let code_count = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
    if code_count > MAX_CODE {
        return Err(format!("inspector reply lists {code_count} mapped files"));
    }
    let mut code = Vec::with_capacity(code_count);
    for _ in 0..code_count {
        let ino = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
        let deleted = match take(&mut b, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err("bad flag in inspector reply".into()),
        };
        let n = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        let path = PathBuf::from(OsStr::from_bytes(&take(&mut b, n)?));
        code.push(MappedCode { path, ino, deleted });
    }
    if !b.is_empty() {
        return Err("trailing bytes in inspector reply".into());
    }
    Ok(Inspected {
        pid,
        uid,
        exe_path,
        dev,
        ino,
        sha256,
        code,
    })
}

// ---------------------------------------------------------------------------
// `/proc` helpers

fn pid_from_fdinfo(pidfd: BorrowedFd<'_>) -> Result<u32, String> {
    use std::os::fd::AsRawFd;
    let path = format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd());
    let text = fs::read_to_string(&path).map_err(|e| format!("can't read {path}: {e}"))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Pid:") {
            let trimmed = rest.trim();
            if trimmed == "0" || trimmed == "-1" {
                return Err("target process has already exited".into());
            }
            return trimmed
                .parse::<u32>()
                .map_err(|_| format!("invalid Pid {trimmed:?} in {path}"));
        }
    }
    Err(format!("no Pid field in {path}"))
}

fn read_proc_uid(status_path: &Path) -> Result<u32, String> {
    let text = fs::read_to_string(status_path)
        .map_err(|e| format!("can't read {}: {e}", status_path.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            let first = rest
                .split_whitespace()
                .next()
                .ok_or_else(|| "empty Uid line".to_owned())?;
            return first
                .parse::<u32>()
                .map_err(|_| format!("invalid Uid {first:?}"));
        }
    }
    Err(format!("no Uid line in {}", status_path.display()))
}

fn strip_deleted_suffix(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    if let Some(stripped) = bytes.strip_suffix(b" (deleted)") {
        PathBuf::from(OsStr::from_bytes(stripped))
    } else {
        path.to_path_buf()
    }
}

/// Every file `maps` shows mapped executable, once each.
fn read_mapped_code(maps_path: &Path) -> Result<Vec<MappedCode>, String> {
    let f = File::open(maps_path)
        .map_err(|e| format!("can't open {}: {e}", maps_path.display()))?;
    let mut raw = Vec::new();
    f.take((MAX_MAPS + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| format!("can't read {}: {e}", maps_path.display()))?;
    if raw.len() > MAX_MAPS {
        return Err("the caller's memory map is too large".into());
    }
    parse_maps(&raw)
}

/// Parses `/proc/<pid>/maps` lines, `address perms offset dev inode path`,
/// where the path is padded with spaces and may contain some. Keeps
/// executable mappings of files; anonymous memory and `[vdso]` have inode 0.
fn parse_maps(raw: &[u8]) -> Result<Vec<MappedCode>, String> {
    let mut code: Vec<MappedCode> = Vec::new();
    for line in raw.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let bad = || {
            format!(
                "unexpected line in maps: {:?}",
                String::from_utf8_lossy(line)
            )
        };
        let mut rest = line;
        let mut fields: [&[u8]; 5] = [&[]; 5];
        for field in &mut fields {
            rest = trim_start(rest);
            let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            *field = &rest[..end];
            rest = &rest[end..];
        }
        let path = trim_start(rest);
        let executable = fields[1].get(2) == Some(&b'x');
        let ino: u64 = std::str::from_utf8(fields[4])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(bad)?;
        if !executable || ino == 0 || !path.starts_with(b"/") {
            continue;
        }
        let (path, deleted) = match path.strip_suffix(b" (deleted)") {
            Some(p) => (p, true),
            None => (path, false),
        };
        let entry = MappedCode {
            path: PathBuf::from(OsStr::from_bytes(path)),
            ino,
            deleted,
        };
        if !code.contains(&entry) {
            if code.len() == MAX_CODE {
                return Err(format!("the caller maps more than {MAX_CODE} files as code"));
            }
            code.push(entry);
        }
    }
    Ok(code)
}

fn trim_start(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|&c| c != b' ').unwrap_or(b.len());
    &b[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn inspects_current_process_in_process() {
        let pid = rustix::process::getpid();
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).unwrap();
        let info = inspect_pidfd(pidfd.as_fd()).unwrap();
        assert_eq!(info.pid, pid.as_raw_nonzero().get() as u32);
        assert_eq!(info.uid, rustix::process::getuid().as_raw());
        assert_eq!(info.exe_path, std::env::current_exe().unwrap());
        assert_ne!(info.ino, 0);
        assert_eq!(info.sha256_hex().len(), 64);
        assert!(
            info.code
                .iter()
                .any(|c| c.path == info.exe_path && c.ino == info.ino && !c.deleted),
            "{:?}",
            info.code
        );
    }

    #[test]
    fn maps_lines() {
        let maps = b"\
55d0c0a00000-55d0c0a20000 r--p 00000000 fd:01 1234                       /usr/bin/app
55d0c0a20000-55d0c0a80000 r-xp 00020000 fd:01 1234                       /usr/bin/app
7f0000000000-7f0000001000 r-xp 00000000 00:00 0 
7f0100000000-7f0100001000 r-xp 00001000 fd:01 99                         /home/u/my libs/evil.so (deleted)
7f0200000000-7f0200001000 r-xp 00001000 fd:01 98                         /usr/lib/libc.so.6
7f0300000000-7f0300001000 r--p 00000000 fd:01 97                         /usr/share/fonts/x.ttf
7f0400000000-7f0400001000 r-xp 00002000 fd:01 98                         /usr/lib/libc.so.6
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0                  [vsyscall]
";
        let entry = |path: &str, ino, deleted| MappedCode {
            path: path.into(),
            ino,
            deleted,
        };
        assert_eq!(
            parse_maps(maps).unwrap(),
            vec![
                entry("/usr/bin/app", 1234, false),
                entry("/home/u/my libs/evil.so", 99, true),
                entry("/usr/lib/libc.so.6", 98, false),
            ]
        );
        assert!(parse_maps(b"garbage\n").is_err());
    }

    #[test]
    fn inspects_over_unix_socket_and_checks_both_peers() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("inspect.sock");
        let my_uid = rustix::process::getuid().as_raw();

        let sock_srv = sock.clone();
        let handle = thread::spawn(move || {
            let listener = UnixListener::bind(&sock_srv).unwrap();
            // The daemon is the allowed user.
            let (s1, _) = listener.accept().unwrap();
            handle_client(s1, my_uid).unwrap();
            // It isn't.
            let (s2, _) = listener.accept().unwrap();
            handle_client(s2, my_uid + 1).unwrap();
            // It is, but it expects another inspector.
            let (s3, _) = listener.accept().unwrap();
            handle_client(s3, my_uid).unwrap();
        });

        while !sock.exists() {
            thread::sleep(Duration::from_millis(5));
        }

        let pid = rustix::process::getpid();
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).unwrap();

        let ok = request_inspection(&sock, pidfd.as_fd(), my_uid).unwrap();
        assert_eq!(ok.exe_path, std::env::current_exe().unwrap());

        let refused = request_inspection(&sock, pidfd.as_fd(), my_uid).unwrap_err();
        assert!(refused.contains("isn't allowed"), "{refused}");

        let impostor = request_inspection(&sock, pidfd.as_fd(), my_uid + 1).unwrap_err();
        assert!(impostor.contains("runs as uid"), "{impostor}");

        handle.join().unwrap();
    }

    #[test]
    fn anchors_are_real_and_out_of_others_reach() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::create_dir(&state).unwrap();
        let me = rustix::process::geteuid().as_raw();
        assert_eq!(anchor(&state).unwrap().uid(), me);
        assert!(anchor(Path::new("relative/state")).is_err());

        std::os::unix::fs::symlink(&state, dir.path().join("link")).unwrap();
        let err = anchor(&dir.path().join("link")).unwrap_err();
        assert!(err.contains("symlink"), "{err}");

        let open = dir.path().join("open");
        fs::create_dir_all(open.join("state")).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let err = anchor(&open.join("state")).unwrap_err();
        assert!(err.contains("writable"), "{err}");
    }
}

