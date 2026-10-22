//! Where the daemon and the inspector get their listening sockets (design
//! doc §3, §14).
//!
//! A release build takes its socket only from systemd. Its unit says
//! `StandardInput=socket`, so the socket arrives as standard input: the one
//! descriptor this crate can borrow without `unsafe`. (`LISTEN_FDS` hands it
//! over as descriptor 3, which only `OwnedFd::from_raw_fd` can take.) The
//! socket must also be bound where clients connect, so the units and the
//! code can't drift apart. Standard input stays open on the socket; nothing
//! reads it.
//!
//! Development builds (`test_identity`) may bind a socket themselves.

use std::io;
use std::os::unix::net::UnixListener;
use std::path::Path;

use rustix::net::{SocketType, sockopt};

/// The listening socket systemd passed as standard input, which must be
/// bound at `expected`.
pub fn from_systemd(expected: &Path) -> Result<UnixListener, String> {
    let stdin = io::stdin();
    if sockopt::socket_type(&stdin).ok() != Some(SocketType::STREAM)
        || !sockopt::socket_acceptconn(&stdin).unwrap_or(false)
    {
        return Err(format!(
            "no socket from systemd: standard input isn't a listening stream socket. \
             Start this mode through its .socket unit (StandardInput=socket), which \
             listens on {}",
            expected.display()
        ));
    }
    let fd = rustix::io::fcntl_dupfd_cloexec(&stdin, 3)
        .map_err(|e| format!("can't take the socket from standard input: {e}"))?;
    let listener = UnixListener::from(fd);
    let addr = listener
        .local_addr()
        .map_err(|e| format!("standard input isn't a Unix socket: {e}"))?;
    match addr.as_pathname() {
        Some(path) if path == expected => Ok(listener),
        other => Err(format!(
            "the socket from systemd is bound at {}, not {}",
            other.map_or_else(|| "no path".to_owned(), |p| p.display().to_string()),
            expected.display()
        )),
    }
}

/// Binds `path` with `mode`, replacing a socket left there. Development
/// builds only.
#[cfg(feature = "test_identity")]
pub fn bind(path: &Path, mode: u32) -> io::Result<UnixListener> {
    use std::fs;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} isn't a socket", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(listener)
}
