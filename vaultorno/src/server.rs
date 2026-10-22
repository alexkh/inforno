//! The daemon (`vaultorno daemon`): one thread per connection, one vault per
//! connection, four requests (design doc §9). It stores and serves the
//! client's envelopes and checks only their shape: it has no key to them.

use std::collections::HashMap;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use vaultorno_client::protocol::{
    ErrorCode, Frame, FrameError, LockerInfo, MAX_ENVIRONMENT, MAX_LOCKERS_PER_APP, PROTOCOL,
    Request, Response, ShapeError, check_envelope_shape, read_frame, write_frame,
};
use vaultorno_client::time::Clock;
use zeroize::Zeroize;

use crate::identity::{Identify, Identity, Peer};
use crate::store::{Header, Store, VaultLock};

/// Connections one user may hold open (§10).
pub const MAX_CONNECTIONS_PER_USER: usize = 32;
/// Vaults one user may have (§10).
pub const MAX_VAULTS_PER_USER: usize = 64;
/// How long a new connection has to say `Hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Base delay for progressive refusal back-off in production (§10).
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(25);
/// Maximum delay for progressive refusal back-off (§10).
pub const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// The daemon.
pub struct Server {
    store: Store,
    identity: Box<dyn Identify>,
    clock: Arc<dyn Clock>,
    backoff_base: Duration,
    connections: Mutex<HashMap<u32, usize>>,
    refused: Mutex<HashMap<(u32, String), u32>>,
    backoff_streaks: Mutex<HashMap<(u32, Option<i32>), u32>>,
}

fn error(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A connection's place in its user's count, given back on drop.
struct Slot<'a> {
    server: &'a Server,
    uid: u32,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut conns = lock(&self.server.connections);
        if let Some(n) = conns.get_mut(&self.uid) {
            *n -= 1;
            if *n == 0 {
                conns.remove(&self.uid);
            }
        }
    }
}

impl Server {
    pub fn new(store: Store, identity: Box<dyn Identify>, clock: Arc<dyn Clock>) -> Arc<Server> {
        Self::with_backoff(store, identity, clock, Duration::ZERO)
    }

    pub fn with_backoff(
        store: Store,
        identity: Box<dyn Identify>,
        clock: Arc<dyn Clock>,
        backoff_base: Duration,
    ) -> Arc<Server> {
        Arc::new(Server {
            store,
            identity,
            clock,
            backoff_base,
            connections: Mutex::new(HashMap::new()),
            refused: Mutex::new(HashMap::new()),
            backoff_streaks: Mutex::new(HashMap::new()),
        })
    }

    /// Computes and sleeps the progressive back-off for a refused request
    /// from `peer`, returning the delay applied (§10).
    fn apply_backoff(&self, peer: &Peer) -> Duration {
        let streak = {
            let mut map = lock(&self.backoff_streaks);
            let n = map.entry((peer.uid, peer.pid)).or_default();
            *n = n.saturating_add(1);
            *n
        };
        if self.backoff_base.is_zero() {
            return Duration::ZERO;
        }
        let shift = (streak - 1).min(10);
        let delay = self
            .backoff_base
            .saturating_mul(1u32 << shift)
            .min(MAX_BACKOFF);
        thread::sleep(delay);
        delay
    }

    fn clear_backoff(&self, peer: &Peer) {
        lock(&self.backoff_streaks).remove(&(peer.uid, peer.pid));
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Serves connections on `listener`: the one `vaultorno.socket` passes
    /// (mode `0666`: access is decided per connection), or one a development
    /// daemon bound (`activation.rs`).
    pub fn listen(self: &Arc<Self>, listener: UnixListener) -> io::Result<()> {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("vaultorno daemon: accept failed: {e}");
                    // E.g. out of file descriptors: don't spin.
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            let peer = match peer_of(&stream) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("vaultorno daemon: no peer credentials: {e}");
                    continue;
                }
            };
            let server = Arc::clone(self);
            thread::spawn(move || server.serve(stream, peer));
        }
        Ok(())
    }

    /// Serves one connection from `peer` until it closes. The names of the
    /// caller's environment variables come from its `Hello` (§7.5).
    pub fn serve(&self, stream: UnixStream, peer: Peer) {
        self.serve_inner(stream, peer, true)
    }

    /// As [`Server::serve`], but checks `peer.environment` and ignores what
    /// `Hello` names, so in-process tests choose it.
    pub fn serve_with_environment(&self, stream: UnixStream, peer: Peer) {
        self.serve_inner(stream, peer, false)
    }

    fn serve_inner(&self, stream: UnixStream, peer: Peer, from_hello: bool) {
        let Some(_slot) = self.claim_slot(peer.uid) else {
            refuse_busy(&stream);
            return;
        };
        // Any error, including a malformed or oversized frame, closes the
        // connection (§9).
        let _ = self.session(&stream, peer, from_hello);
    }

    fn claim_slot(&self, uid: u32) -> Option<Slot<'_>> {
        let mut conns = lock(&self.connections);
        let n = conns.entry(uid).or_default();
        if *n >= MAX_CONNECTIONS_PER_USER {
            return None;
        }
        *n += 1;
        Some(Slot { server: self, uid })
    }

    fn session(
        &self,
        stream: &UnixStream,
        mut peer: Peer,
        from_hello: bool,
    ) -> Result<(), FrameError> {
        let mut io = stream;
        stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        let Some(first) = read_frame::<_, Request>(&mut io)? else {
            return Ok(());
        };
        let id = first.id;
        let reply = |body: Response| write_frame(&mut &*stream, &Frame { id, body });

        let (statement, signature, succession_text, succession_signature) = match first.body {
            Request::Hello {
                protocol,
                statement,
                signature,
                succession,
                succession_signature,
                environment,
            } => {
                if protocol != PROTOCOL {
                    return reply(error(
                        ErrorCode::ProtocolMismatch,
                        format!("this daemon speaks protocol {PROTOCOL}"),
                    ));
                }
                if environment.len() > MAX_ENVIRONMENT {
                    return reply(error(
                        ErrorCode::ProtocolMismatch,
                        format!("hello names more than {MAX_ENVIRONMENT} variables"),
                    ));
                }
                if from_hello {
                    peer.environment = environment;
                }
                (statement, signature, succession, succession_signature)
            }
            _ => {
                return reply(error(
                    ErrorCode::ProtocolMismatch,
                    "the first request must be hello",
                ));
            }
        };
        // A succession and its signature come together (§8).
        let succession = match (succession_text.as_deref(), succession_signature.as_deref()) {
            (Some(text), Some(sig)) => Some((text, sig)),
            (None, None) => None,
            _ => {
                return reply(error(
                    ErrorCode::ProtocolMismatch,
                    "a succession is sent with its signature, or not at all",
                ));
            }
        };

        // Unreadable state, e.g. a line in `revoked_keys` that isn't a key:
        // refuse everything rather than guess (§9).
        if let Err(message) = self.identity.check_state() {
            eprintln!("vaultorno daemon: refusing every hello: {message}");
            return reply(error(ErrorCode::Storage, message));
        }
        let now = self.clock.now();
        let ident =
            match self.identity.identify(
                &peer,
                statement.as_deref(),
                signature.as_deref(),
                succession,
                now,
            ) {
                Ok(i) => {
                    self.clear_backoff(&peer);
                    i
                }
                Err(refusal) => {
                    // Until the log (§11, Milestone 6): why, never what.
                    eprintln!(
                        "vaultorno daemon: refused uid {} pid {}: {}",
                        peer.uid,
                        peer.pid.map_or_else(|| "?".to_owned(), |p| p.to_string()),
                        refusal.message
                    );
                    // Anyone can sign with a fresh key, so a fingerprint vault
                    // is counted only once it exists; manifest vaults are
                    // bounded by the manifests root installed.
                    if let Some(vault) = refusal.vault {
                        if !is_key_vault(&vault) || self.store.vault_exists(peer.uid, &vault) {
                            *lock(&self.refused).entry((peer.uid, vault)).or_default() += 1;
                        }
                    }
                    self.apply_backoff(&peer);
                    return reply(error(ErrorCode::NotAllowed, refusal.message));
                }
            };
        let half = match self.open_vault(&peer, &ident) {
            Ok(Ok(half)) => half,
            Ok(Err(refused)) => return reply(refused),
            Err(e) => return reply(error(ErrorCode::Storage, e.to_string())),
        };
        let refused = lock(&self.refused)
            .remove(&(peer.uid, ident.vault.clone()))
            .unwrap_or(0);
        let mut welcome = Frame {
            id,
            body: Response::Welcome {
                protocol: PROTOCOL,
                app: ident.app.clone(),
                vault: ident.vault.clone(),
                binary: ident.binary.clone(),
                level: ident.level.clone(),
                sealer: self.store.sealer().into(),
                half: half.to_vec(),
                statement_expires: ident.statement_expires,
                refused,
            },
        };
        let sent = write_frame(&mut io, &welcome);
        if let Response::Welcome { half, .. } = &mut welcome.body {
            half.zeroize();
        }
        sent?;
        eprintln!(
            "vaultorno daemon: uid {} opened {} as {} ({})",
            peer.uid, ident.vault, ident.binary, ident.level
        );

        stream.set_read_timeout(None)?;
        while let Some(frame) = read_frame::<_, Request>(&mut io)? {
            let body = self.handle(&peer, &ident, frame.body);
            write_frame(&mut io, &Frame { id: frame.id, body })?;
        }
        Ok(())
    }

    /// Creates the vault if needed (within the vault limit) and returns its
    /// daemon half.
    #[allow(clippy::type_complexity)]
    fn open_vault(
        &self,
        peer: &Peer,
        ident: &Identity,
    ) -> io::Result<Result<zeroize::Zeroizing<[u8; 32]>, Response>> {
        if !self.store.vault_exists(peer.uid, &ident.vault)
            && self.store.vault_count(peer.uid)? >= MAX_VAULTS_PER_USER
        {
            return Ok(Err(error(
                ErrorCode::NotAllowed,
                format!("this user already has {MAX_VAULTS_PER_USER} vaults"),
            )));
        }
        let lock = self.store.lock_vault(peer.uid, &ident.vault)?;
        Ok(Ok(self.store.daemon_half(&lock, peer.uid, &ident.vault)?))
    }

    fn handle(&self, peer: &Peer, ident: &Identity, request: Request) -> Response {
        let result = match request {
            Request::Hello { .. } => {
                return error(ErrorCode::ProtocolMismatch, "already said hello");
            }
            Request::List => self.list(peer, ident),
            Request::Get { locker } => self.get(peer, ident, &locker),
            Request::Put {
                locker,
                data,
                expected,
            } => self.put(peer, ident, &locker, &data, expected),
        };
        let resp = result.unwrap_or_else(|e| error(ErrorCode::Storage, e.to_string()));
        if matches!(
            resp,
            Response::Error {
                code: ErrorCode::NotAllowed | ErrorCode::UnknownLocker,
                ..
            }
        ) {
            self.apply_backoff(peer);
        }
        resp
    }

    /// Reads a locker, emptying it first if it has expired.
    fn live(
        &self,
        lock: &VaultLock,
        peer: &Peer,
        ident: &Identity,
        locker: &str,
        now: u64,
    ) -> io::Result<Option<(Header, Vec<u8>)>> {
        match self
            .store
            .read_locker(lock, peer.uid, &ident.vault, locker)?
        {
            Some((h, _)) if h.is_expired(now) => {
                self.store
                    .delete_locker(lock, peer.uid, &ident.vault, locker)?;
                Ok(None)
            }
            other => Ok(other),
        }
    }

    fn unknown(ident: &Identity, locker: &str) -> Response {
        error(
            ErrorCode::UnknownLocker,
            format!("{locker} isn't in {}'s statement", ident.binary),
        )
    }

    fn list(&self, peer: &Peer, ident: &Identity) -> io::Result<Response> {
        let lock = self.store.lock_vault(peer.uid, &ident.vault)?;
        let now = self.clock.now();
        let mut lockers = Vec::with_capacity(ident.lockers.len());
        for (name, grant) in &ident.lockers {
            let (revision, size, expires) =
                match self
                    .store
                    .locker_header(&lock, peer.uid, &ident.vault, name)?
                {
                    Some((h, _)) if h.is_expired(now) => {
                        self.store
                            .delete_locker(&lock, peer.uid, &ident.vault, name)?;
                        (0, 0, None)
                    }
                    Some((h, size)) => (h.revision, size as u32, Some(h.expires)),
                    None => (0, 0, None),
                };
            lockers.push(LockerInfo {
                name: name.clone(),
                revision,
                size,
                expires,
                read: true,
                write: grant.write,
            });
        }
        Ok(Response::Lockers { lockers })
    }

    fn get(&self, peer: &Peer, ident: &Identity, locker: &str) -> io::Result<Response> {
        if !ident.lockers.contains_key(locker) {
            return Ok(Self::unknown(ident, locker));
        }
        let lock = self.store.lock_vault(peer.uid, &ident.vault)?;
        Ok(
            match self.live(&lock, peer, ident, locker, self.clock.now())? {
                None => Response::Locker {
                    data: Vec::new(),
                    revision: 0,
                    expires: None,
                },
                Some((h, body)) => Response::Locker {
                    data: body,
                    revision: h.revision,
                    expires: Some(h.expires),
                },
            },
        )
    }

    fn put(
        &self,
        peer: &Peer,
        ident: &Identity,
        locker: &str,
        data: &[u8],
        expected: u64,
    ) -> io::Result<Response> {
        let Some(grant) = ident.lockers.get(locker) else {
            return Ok(Self::unknown(ident, locker));
        };
        if !grant.write {
            return Ok(error(
                ErrorCode::NotAllowed,
                format!("{locker} is read-only for {}", ident.binary),
            ));
        }
        if !data.is_empty() {
            match check_envelope_shape(data) {
                Ok(()) => {}
                Err(ShapeError::TooLarge) => {
                    return Ok(error(ErrorCode::TooLarge, "a locker holds at most 256 KiB"));
                }
                Err(ShapeError::NotSealed) => {
                    return Ok(error(
                        ErrorCode::NotSealed,
                        "data isn't a client envelope; use vaultorno_client",
                    ));
                }
            }
        }

        let lock = self.store.lock_vault(peer.uid, &ident.vault)?;
        let now = self.clock.now();
        let current = self.live(&lock, peer, ident, locker, now)?;
        let current_revision = current.as_ref().map_or(0, |(h, _)| h.revision);
        if current_revision != expected {
            return Ok(error(
                ErrorCode::Conflict,
                format!("revision is {current_revision:x}, not {expected:x}"),
            ));
        }
        if data.is_empty() {
            if current.is_some() {
                self.store
                    .delete_locker(&lock, peer.uid, &ident.vault, locker)?;
            }
            return Ok(Response::Stored {
                revision: 0,
                expires: 0,
            });
        }
        let expires = match &current {
            // Rewriting never extends the expiry (§6).
            Some((h, _)) => h.expires,
            None => {
                let mut filled = Vec::new();
                for name in self.store.filled_lockers(&lock, peer.uid, &ident.vault)? {
                    match self
                        .store
                        .locker_header(&lock, peer.uid, &ident.vault, &name)?
                    {
                        Some((h, _)) if h.is_expired(now) => {
                            self.store
                                .delete_locker(&lock, peer.uid, &ident.vault, &name)?
                        }
                        Some(_) => filled.push(name),
                        None => {}
                    }
                }
                if filled.len() >= MAX_LOCKERS_PER_APP {
                    return Ok(error(
                        ErrorCode::NotAllowed,
                        format!("this app already uses {MAX_LOCKERS_PER_APP} lockers"),
                    ));
                }
                now + grant.lifetime_secs
            }
        };
        let revision = new_revision(current_revision)?;
        self.store.write_locker(
            &lock,
            peer.uid,
            &ident.vault,
            locker,
            &Header { revision, expires },
            data,
        )?;
        Ok(Response::Stored { revision, expires })
    }
}

/// Over the connection limit: answer the first request with `Busy`, then
/// close.
fn refuse_busy(stream: &UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let id = read_frame::<_, Request>(&mut &*stream)
        .ok()
        .flatten()
        .map_or(0, |f| f.id);
    let _ = write_frame(
        &mut &*stream,
        &Frame {
            id,
            body: error(
                ErrorCode::Busy,
                format!("this user already has {MAX_CONNECTIONS_PER_USER} connections"),
            ),
        },
    );
}

/// A vault named by a key fingerprint (64 hex digits), not `pkg_` or
/// `test_` (§6).
fn is_key_vault(vault: &str) -> bool {
    vault.len() == 64 && vault.bytes().all(|b| b.is_ascii_hexdigit())
}

fn peer_of(stream: &UnixStream) -> io::Result<Peer> {
    let (uid, pid, pidfd) = vaultorno_inspect::peer_pidfd(stream)?;
    Ok(Peer {
        uid,
        pid: Some(pid),
        pidfd: Some(Arc::new(pidfd)),
        // The caller's client names these in Hello (§7.5): step 2. Until
        // then a real connection reports none.
        environment: Vec::new(),
    })
}

fn new_revision(previous: u64) -> io::Result<u64> {
    loop {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
        let r = u64::from_be_bytes(b);
        if r != 0 && r != previous {
            return Ok(r);
        }
    }
}
