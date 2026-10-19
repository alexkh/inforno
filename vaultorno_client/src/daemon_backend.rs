//! The `Daemon` backend: one connection to vaultorno, one vault.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;

use secrecy::SecretBox;
use zeroize::Zeroizing;

use crate::Error;
use crate::names::{is_valid_name, is_valid_vault_name};
use crate::protocol::{
    Frame, HALF_LEN, LockerInfo, PROTOCOL, Request, Response, read_frame, write_frame,
};
use crate::seal::AppKey;
use crate::user_half;
use crate::vault::{Locker, StatementFiles, Stored};

struct Conn {
    stream: UnixStream,
    next_id: u32,
}

impl Conn {
    fn request(&mut self, body: Request) -> Result<Response, Error> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        write_frame(&mut self.stream, &Frame { id, body })?;
        let frame: Frame<Response> = read_frame(&mut self.stream)?
            .ok_or_else(|| Error::Protocol("the daemon closed the connection".into()))?;
        if frame.id != id {
            return Err(Error::Protocol(format!(
                "reply {} to request {id}",
                frame.id
            )));
        }
        match frame.body {
            Response::Error { code, message } => Err(Error::from_daemon(code, message)),
            other => Ok(other),
        }
    }

    fn list(&mut self) -> Result<Vec<LockerInfo>, Error> {
        match self.request(Request::List)? {
            Response::Lockers { lockers } => Ok(lockers),
            other => unexpected(other),
        }
    }
}

fn unexpected<T>(reply: Response) -> Result<T, Error> {
    Err(Error::Protocol(format!("unexpected reply {reply:?}")))
}

/// What the daemon said in `Welcome`, minus its half.
#[derive(Debug, Clone)]
pub(crate) struct Welcome {
    pub vault: String,
    pub binary: String,
    pub level: String,
    pub sealer: String,
    pub statement_expires: Option<u64>,
    pub refused: u32,
}

pub(crate) struct DaemonConn {
    conn: Mutex<Conn>,
    key: AppKey,
    pub(crate) welcome: Welcome,
}

impl DaemonConn {
    /// Says `Hello`, then combines the daemon's half from `Welcome` with the
    /// user's half from `home` into the app key (§6).
    pub(crate) fn open(
        stream: UnixStream,
        app: &str,
        statement: Option<StatementFiles>,
        home: &Path,
        uid: u32,
    ) -> Result<DaemonConn, Error> {
        let mut conn = Conn { stream, next_id: 1 };
        let (st_text, signature) = match statement {
            Some(s) => (Some(s.statement), s.signature),
            None => (None, None),
        };
        let reply = conn.request(Request::Hello {
            protocol: PROTOCOL,
            statement: st_text,
            signature,
            succession: None,
        })?;
        let Response::Welcome {
            protocol,
            app: got_app,
            vault,
            binary,
            level,
            sealer,
            half,
            statement_expires,
            refused,
        } = reply
        else {
            return unexpected(reply);
        };
        let half = Zeroizing::new(half);
        if protocol != PROTOCOL {
            return Err(Error::ProtocolMismatch(format!(
                "the daemon speaks protocol {protocol}, not {PROTOCOL}"
            )));
        }
        if got_app != app {
            return Err(Error::Protocol(format!(
                "the daemon opened app {got_app:?}, not {app:?}"
            )));
        }
        if !is_valid_vault_name(&vault) || !is_valid_name(&binary) {
            return Err(Error::Protocol(format!(
                "bad vault {vault:?} or binary {binary:?} in Welcome"
            )));
        }
        let daemon_half: Zeroizing<[u8; HALF_LEN]> = Zeroizing::new(
            half.as_slice()
                .try_into()
                .map_err(|_| Error::Protocol("the daemon's half isn't 32 bytes".into()))?,
        );

        let user_half = match user_half::load(home, app, &vault, false)? {
            Some(h) => h,
            None => {
                // A missing half with lockers still filled means they're lost;
                // never paper over that with a new half (§6).
                if conn.list()?.iter().any(|l| l.revision != 0) {
                    return Err(Error::KeyUnavailable);
                }
                user_half::load(home, app, &vault, true)?.ok_or(Error::KeyUnavailable)?
            }
        };
        let key = AppKey::derive(&daemon_half, &user_half, uid, &vault);
        Ok(DaemonConn {
            conn: Mutex::new(conn),
            key,
            welcome: Welcome {
                vault,
                binary,
                level,
                sealer,
                statement_expires,
                refused,
            },
        })
    }

    fn with_conn<T>(&self, f: impl FnOnce(&mut Conn) -> Result<T, Error>) -> Result<T, Error> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Protocol("connection lock poisoned".into()))?;
        f(&mut conn)
    }

    pub(crate) fn get(&self, locker: &str) -> Result<Locker, Error> {
        match self.with_conn(|c| {
            c.request(Request::Get {
                locker: locker.to_owned(),
            })
        })? {
            Response::Locker {
                data,
                revision,
                expires,
            } => {
                if data.is_empty() {
                    return Ok(Locker::empty());
                }
                let plain = self.key.open(locker, &data)?;
                Ok(Locker {
                    data: SecretBox::new(plain.to_vec().into_boxed_slice()),
                    revision,
                    expires,
                })
            }
            other => unexpected(other),
        }
    }

    pub(crate) fn put(&self, locker: &str, data: &[u8], expected: u64) -> Result<Stored, Error> {
        let envelope = if data.is_empty() {
            Vec::new()
        } else {
            self.key.seal(locker, data)?
        };
        match self.with_conn(|c| {
            c.request(Request::Put {
                locker: locker.to_owned(),
                data: envelope,
                expected,
            })
        })? {
            Response::Stored { revision, expires } => Ok(Stored {
                revision,
                expires: (expires != 0).then_some(expires),
            }),
            other => unexpected(other),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<LockerInfo>, Error> {
        self.with_conn(|c| c.list())
    }
}
