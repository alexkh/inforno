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
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
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

/// Maximum reply frame length (64 KiB).
const MAX_REPLY: usize = 64 * 1024;

/// Maximum `/proc/<pid>/environ` bytes read (256 KiB).
const MAX_ENVIRON: usize = 256 * 1024;

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
    /// Names of environment variables set in `/proc/<pid>/environ`.
    pub env_names: Vec<String>,
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

    let env_names = read_environ_names(&proc_dir.join("environ"))?;

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
        env_names,
    })
}

/// Asks the inspector listening at `socket` to inspect `pidfd`.
/// If `expected_inspector_uid` is `Some(uid)`, refuses any inspector process
/// not running as `uid` (§3).
pub fn request_inspection(
    socket: &Path,
    pidfd: BorrowedFd<'_>,
    expected_inspector_uid: Option<u32>,
) -> Result<Inspected, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| format!("can't connect to inspector at {}: {e}", socket.display()))?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    if let Some(expected) = expected_inspector_uid {
        let cred = sockopt::socket_peercred(&stream)
            .map_err(|e| format!("can't check inspector credentials: {e}"))?;
        let actual = cred.uid.as_raw();
        if actual != expected {
            return Err(format!(
                "inspector socket is owned by uid {actual}, expected {expected}"
            ));
        }
    }

    send_request(&stream, pidfd)?;
    read_reply(&mut stream)
}

/// Runs the inspector loop on `socket` (mode `0660`), accepting connections
/// only from `allowed_daemon_uid` (§3, §14).
pub fn serve_inspect(socket: &Path, allowed_daemon_uid: u32) -> io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(socket) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} isn't a socket", socket.display()),
            ));
        }
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = handle_client(stream, allowed_daemon_uid);
    }
    Ok(())
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
    let payload = encode_reply(&result);
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(&payload)?;
    w.flush()
}

fn read_reply(r: &mut impl Read) -> Result<Inspected, String> {
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes)
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
            out.extend_from_slice(&(ins.env_names.len() as u32).to_be_bytes());
            for name in &ins.env_names {
                let nb = name.as_bytes();
                let len = u16::try_from(nb.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(&nb[.. usize::from(len)]);
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
    let env_count = u32::from_be_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
    let mut env_names = Vec::with_capacity(env_count.min(4096));
    for _ in 0..env_count {
        let n = u16::from_be_bytes(take(&mut b, 2)?.try_into().unwrap()) as usize;
        let name = String::from_utf8(take(&mut b, n)?)
            .map_err(|_| "non-utf8 env name in inspector reply".to_owned())?;
        env_names.push(name);
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
        env_names,
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

fn read_environ_names(environ_path: &Path) -> Result<Vec<String>, String> {
    let f = File::open(environ_path)
        .map_err(|e| format!("can't open {}: {e}", environ_path.display()))?;
    let mut raw = Vec::new();
    f.take((MAX_ENVIRON + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| format!("can't read {}: {e}", environ_path.display()))?;
    if raw.len() > MAX_ENVIRON {
        return Err("caller environment exceeds 256 KiB".into());
    }
    let mut names = Vec::new();
    for entry in raw.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let name_bytes = match entry.iter().position(|&b| b == b'=') {
            Some(pos) => &entry[..pos],
            None => entry,
        };
        // Non-UTF-8 variable names are preserved lossily so they can never
        // smuggle past prefix checks.
        names.push(String::from_utf8_lossy(name_bytes).into_owned());
    }
    Ok(names)
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
    }

    #[test]
    fn inspects_over_unix_socket_and_checks_peer_uid() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("inspect.sock");
        let my_uid = rustix::process::getuid().as_raw();

        let sock_srv = sock.clone();
        let handle = thread::spawn(move || {
            let listener = UnixListener::bind(&sock_srv).unwrap();
            // First connection: allowed uid.
            let (s1, _) = listener.accept().unwrap();
            handle_client(s1, my_uid).unwrap();
            // Second connection: wrong allowed uid.
            let (s2, _) = listener.accept().unwrap();
            handle_client(s2, my_uid + 1).unwrap();
        });

        while !sock.exists() {
            thread::sleep(Duration::from_millis(5));
        }

        let pid = rustix::process::getpid();
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).unwrap();

        let ok = request_inspection(&sock, pidfd.as_fd(), Some(my_uid)).unwrap();
        assert_eq!(ok.exe_path, std::env::current_exe().unwrap());

        let refused = request_inspection(&sock, pidfd.as_fd(), Some(my_uid));
        assert!(refused.is_err());

        handle.join().unwrap();
    }
}

