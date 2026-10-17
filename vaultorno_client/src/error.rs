use std::io;

use crate::protocol::{ErrorCode, FrameError, StatementError};

/// Everything that can go wrong for an app using a vault.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("protocol mismatch: {0}")]
    ProtocolMismatch(String),
    #[error("not allowed: {0}")]
    NotAllowed(String),
    #[error("unknown locker: {0}")]
    UnknownLocker(String),
    #[error("conflict: the locker changed since it was read")]
    Conflict,
    #[error("too large: {0}")]
    TooLarge(String),
    #[error("not sealed: {0}")]
    NotSealed(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("busy: {0}")]
    Busy(String),
    /// No valid statement was found for this executable (§5).
    #[error("no valid statement: {0}")]
    NoStatement(String),
    /// The user's half is missing while lockers aren't empty (§6).
    #[error("the user's half is missing but the vault isn't empty; reset the vault to start over")]
    KeyUnavailable,
    /// An envelope was sealed under a different app key: one of the halves
    /// changed (§6).
    #[error("this locker was sealed under a different app key")]
    KeyMismatch,
    /// An envelope or sealed file fails to decrypt.
    #[error("a locker failed to decrypt")]
    Corrupt,
    #[error("invalid name {0:?}: names are [a-z0-9_]{{1,32}}")]
    InvalidName(String),
    /// The user's half, or a private folder, has the wrong owner, mode or size.
    #[error("refused a private file: {0}")]
    Private(String),
    /// The OS sealer of the `Local` backend failed.
    #[error("sealer failed: {0}")]
    Sealer(String),
}

impl Error {
    pub(crate) fn from_daemon(code: ErrorCode, message: String) -> Error {
        match code {
            ErrorCode::ProtocolMismatch => Error::ProtocolMismatch(message),
            ErrorCode::NotAllowed => Error::NotAllowed(message),
            ErrorCode::UnknownLocker => Error::UnknownLocker(message),
            ErrorCode::Conflict => Error::Conflict,
            ErrorCode::TooLarge => Error::TooLarge(message),
            ErrorCode::NotSealed => Error::NotSealed(message),
            ErrorCode::Storage => Error::Storage(message),
            ErrorCode::Busy => Error::Busy(message),
        }
    }
}

impl From<FrameError> for Error {
    fn from(e: FrameError) -> Error {
        match e {
            FrameError::Io(e) => Error::Io(e),
            other => Error::Protocol(other.to_string()),
        }
    }
}

impl From<StatementError> for Error {
    fn from(e: StatementError) -> Error {
        Error::NoStatement(e.0)
    }
}
