//! The `Vault` API (design doc §9): what apps call. `get` and `put` take and
//! return plaintext; encryption happens here, inside the app's process.

use std::fmt;
use std::fs;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretBox};
use zeroize::Zeroizing;

use crate::Error;
use crate::daemon_backend::DaemonConn;
use crate::local::{LocalStore, OsSealer, default_sealer};
use crate::names::is_valid_name;
#[cfg(unix)]
use crate::protocol::DEFAULT_SOCKET;
use crate::protocol::{LockerInfo, Statement};
use crate::time::{Clock, SystemClock, format_date};

/// How often `update` retries after a conflict.
const UPDATE_ATTEMPTS: usize = 8;

/// One locker's contents, decrypted. `data` is zeroized on drop.
#[derive(Debug)]
pub struct Locker {
    pub data: SecretBox<[u8]>,
    /// 0 when empty.
    pub revision: u64,
    /// Unix seconds; none when empty.
    pub expires: Option<u64>,
}

impl Locker {
    pub(crate) fn empty() -> Locker {
        Locker {
            data: SecretBox::new(Vec::new().into_boxed_slice()),
            revision: 0,
            expires: None,
        }
    }
}

/// The result of a successful `put`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    /// 0 after emptying the locker.
    pub revision: u64,
    /// Unix seconds; none after emptying the locker.
    pub expires: Option<u64>,
}

/// A statement file's text and, once signatures exist, its SSHSIG.
#[derive(Clone)]
pub struct StatementFiles {
    pub statement: String,
    pub signature: Option<String>,
}

/// How to reach the daemon. `Vault::open` fills these in; tests and tools
/// can set them. Unix only: there is no daemon on Windows yet (M7).
#[cfg(unix)]
pub struct DaemonOptions {
    pub app: String,
    /// Where the daemon listens.
    pub socket: PathBuf,
    /// An already connected stream, used instead of `socket`.
    pub stream: Option<UnixStream>,
    /// The user's home folder; by default from the password database, never
    /// from `$HOME` (§6).
    pub home: Option<PathBuf>,
    /// The user ID bound into the app key; by default the real one. Tests
    /// set it to act as several users.
    pub uid: Option<u32>,
    /// The statement to send; by default found next to the executable (§5).
    pub statement: Option<StatementFiles>,
}

#[cfg(unix)]
impl DaemonOptions {
    pub fn new(app: &str) -> DaemonOptions {
        DaemonOptions {
            app: app.to_owned(),
            socket: PathBuf::from(DEFAULT_SOCKET),
            stream: None,
            home: None,
            uid: None,
            statement: None,
        }
    }
}

/// How to use the `Local` backend.
pub struct LocalOptions {
    pub app: String,
    /// Where `local_<app>` lives; by default `~/.local/share/vaultorno`.
    pub base: Option<PathBuf>,
    pub sealer: Arc<dyn OsSealer>,
    /// The statement's text, for its lockers and lifetimes; by default found
    /// next to the executable. `Local` doesn't verify signatures: it has no
    /// per-app protection to offer.
    pub statement: Option<String>,
    pub clock: Arc<dyn Clock>,
}

impl LocalOptions {
    pub fn new(app: &str) -> LocalOptions {
        LocalOptions {
            app: app.to_owned(),
            base: None,
            sealer: default_sealer(),
            statement: None,
            clock: Arc::new(SystemClock),
        }
    }
}

enum Backend {
    /// Never constructed where there's no daemon (see `daemon_stub.rs`).
    #[cfg_attr(not(unix), allow(dead_code))]
    Daemon(DaemonConn),
    Local(LocalStore),
}

/// One app's lockers for the current user.
pub struct Vault {
    app: String,
    backend: Backend,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("app", &self.app)
            .field("backend", &self.describe())
            .finish()
    }
}

impl Vault {
    /// Opens `app`'s vault: the daemon if it's installed, otherwise the
    /// `Local` backend. If the daemon is installed but can't be reached, this
    /// fails rather than falling back. The statement is found next to the
    /// executable (§5).
    pub fn open(app: &str) -> Result<Vault, Error> {
        Vault::open_with(app, None)
    }

    /// As [`Vault::open`], with the statement given instead of found.
    pub fn open_with(app: &str, statement: Option<StatementFiles>) -> Result<Vault, Error> {
        #[cfg(unix)]
        if Path::new(DEFAULT_SOCKET).exists() {
            return Vault::connect(DaemonOptions {
                statement,
                ..DaemonOptions::new(app)
            });
        }
        Vault::local(LocalOptions {
            statement: statement.map(|s| s.statement),
            ..LocalOptions::new(app)
        })
    }

    /// Opens a vault through the daemon.
    #[cfg(unix)]
    pub fn connect(opts: DaemonOptions) -> Result<Vault, Error> {
        if !is_valid_name(&opts.app) {
            return Err(Error::InvalidName(opts.app));
        }
        let real_uid = rustix::process::getuid().as_raw();
        let home = match opts.home {
            Some(h) => h,
            None => home_dir(real_uid)?,
        };
        let statement = match opts.statement {
            Some(s) => s,
            None => find_statement(&opts.app, &SystemClock)?,
        };
        let stream = match opts.stream {
            Some(s) => s,
            None => UnixStream::connect(&opts.socket)?,
        };
        let conn = DaemonConn::open(
            stream,
            &opts.app,
            statement,
            &home,
            opts.uid.unwrap_or(real_uid),
        )?;
        Ok(Vault {
            app: opts.app,
            backend: Backend::Daemon(conn),
        })
    }

    /// Opens a vault in the `Local` backend.
    pub fn local(opts: LocalOptions) -> Result<Vault, Error> {
        if !is_valid_name(&opts.app) {
            return Err(Error::InvalidName(opts.app));
        }
        let text = match opts.statement {
            Some(t) => t,
            None => find_statement(&opts.app, opts.clock.as_ref())?.statement,
        };
        let checked = Statement::parse(&text)?.check(opts.clock.now())?;
        if checked.app != opts.app {
            return Err(Error::NoStatement(format!(
                "the statement is for app {:?}",
                checked.app
            )));
        }
        let base = match opts.base {
            Some(b) => b,
            None => user_data_dir()?,
        };
        let store = LocalStore::open(&base, &opts.app, checked.lockers, opts.sealer, opts.clock)?;
        Ok(Vault {
            app: opts.app,
            backend: Backend::Local(store),
        })
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    /// Which backend and level this vault got, for the app's security report.
    pub fn describe(&self) -> String {
        match &self.backend {
            Backend::Daemon(c) => {
                let w = &c.welcome;
                let mut s = format!("vaultorno, {}, blind", w.level);
                if let Some(t) = w.statement_expires {
                    s.push_str(&format!(", statement until {}", format_date(t)));
                }
                s
            }
            Backend::Local(l) => format!("local {}, no per-app protection", l.sealer_name()),
        }
    }

    /// Refused attempts to open this vault since its last successful open.
    pub fn refused(&self) -> u32 {
        match &self.backend {
            Backend::Daemon(c) => c.welcome.refused,
            Backend::Local(_) => 0,
        }
    }

    /// When this binary's statement expires, if the daemon checked one.
    pub fn statement_expires(&self) -> Option<u64> {
        match &self.backend {
            Backend::Daemon(c) => c.welcome.statement_expires,
            Backend::Local(_) => None,
        }
    }

    /// This binary's name, as the daemon identified it.
    pub fn binary(&self) -> Option<&str> {
        match &self.backend {
            Backend::Daemon(c) => Some(&c.welcome.binary),
            Backend::Local(_) => None,
        }
    }

    /// How the daemon seals its own copy of the files (`Welcome.sealer`), or
    /// the OS sealer of the `Local` backend.
    pub fn sealer(&self) -> &str {
        match &self.backend {
            Backend::Daemon(c) => &c.welcome.sealer,
            Backend::Local(l) => l.sealer_name(),
        }
    }

    /// The vault's name, as the daemon reported it.
    pub fn vault_name(&self) -> Option<&str> {
        match &self.backend {
            Backend::Daemon(c) => Some(&c.welcome.vault),
            Backend::Local(_) => None,
        }
    }

    /// Reads and decrypts a locker. An empty locker has no data and revision 0.
    pub fn get(&self, locker: &str) -> Result<Locker, Error> {
        check_name(locker)?;
        match &self.backend {
            Backend::Daemon(c) => c.get(locker),
            Backend::Local(l) => l.get(locker),
        }
    }

    /// Encrypts and stores `data` if the locker is still at `expected`
    /// (0 to fill an empty one); otherwise `Error::Conflict`. Empty `data`
    /// empties the locker.
    pub fn put(&self, locker: &str, data: &[u8], expected: u64) -> Result<Stored, Error> {
        check_name(locker)?;
        match &self.backend {
            Backend::Daemon(c) => c.put(locker, data, expected),
            Backend::Local(l) => l.put(locker, data, expected),
        }
    }

    /// `get` + `put`, retried on conflict. `edit` gets the current contents
    /// (empty if none) and returns the new ones.
    pub fn update<F>(&self, locker: &str, mut edit: F) -> Result<Stored, Error>
    where
        F: FnMut(&[u8]) -> Result<Vec<u8>, Error>,
    {
        for _ in 0..UPDATE_ATTEMPTS {
            let old = self.get(locker)?;
            let new = Zeroizing::new(edit(old.data.expose_secret())?);
            match self.put(locker, &new, old.revision) {
                Err(Error::Conflict) => continue,
                result => return result,
            }
        }
        Err(Error::Conflict)
    }

    /// The lockers this binary may use, with revisions, sizes and expiries.
    pub fn list(&self) -> Result<Vec<LockerInfo>, Error> {
        match &self.backend {
            Backend::Daemon(c) => c.list(),
            Backend::Local(l) => l.list(),
        }
    }
}

fn check_name(locker: &str) -> Result<(), Error> {
    if is_valid_name(locker) {
        Ok(())
    } else {
        Err(Error::InvalidName(locker.to_owned()))
    }
}

/// Finds this executable's statement (§5): `<binary>_statement.json` next to
/// the executable, then in `~/.local/share/vaultorno/statements/<app>/`, then
/// in `/usr/share/vaultorno/statements/<app>/`. Returns the newest one that is
/// valid now, with its signature if present.
pub fn find_statement(app: &str, clock: &dyn Clock) -> Result<StatementFiles, Error> {
    let exe = std::env::current_exe()?;
    let binary = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| is_valid_name(s))
        .ok_or_else(|| Error::NoStatement(format!("{} isn't a valid binary name", exe.display())))?
        .to_owned();
    let file = format!("{binary}_statement.json");
    let mut dirs: Vec<PathBuf> = exe.parent().map(Path::to_path_buf).into_iter().collect();
    if let Ok(data) = user_data_dir() {
        dirs.push(data.join("statements").join(app));
    }
    #[cfg(unix)]
    dirs.push(Path::new("/usr/share/vaultorno/statements").join(app));

    let mut best: Option<(u64, StatementFiles)> = None;
    for dir in &dirs {
        let path = dir.join(&file);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(checked) = Statement::parse(&text).and_then(|s| s.check(clock.now())) else {
            continue;
        };
        if checked.app != app || checked.binary != binary {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(issued, _)| checked.issued > *issued)
        {
            let signature = fs::read_to_string(path.with_extension("json.sig")).ok();
            best = Some((
                checked.issued,
                StatementFiles {
                    statement: text,
                    signature,
                },
            ));
        }
    }
    best.map(|(_, s)| s).ok_or_else(|| {
        let looked: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        Error::NoStatement(format!("no valid {file} in {}", looked.join(", ")))
    })
}

/// Where this user's vaultorno files live: `~/.local/share/vaultorno` on
/// Unix, `%LocalAppData%\vaultorno` on Windows (§6). Neither comes from an
/// environment variable, which is attacker-controlled.
pub fn user_data_dir() -> Result<PathBuf, Error> {
    #[cfg(unix)]
    return Ok(home_dir(rustix::process::getuid().as_raw())?.join(".local/share/vaultorno"));
    #[cfg(windows)]
    return Ok(local_app_data()?.join("vaultorno"));
}

/// `%LocalAppData%` from the shell's known-folder API, never from the
/// environment.
#[cfg(windows)]
#[allow(unsafe_code)]
fn local_app_data() -> Result<PathBuf, Error> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: `raw` is a valid out-pointer; no token means the current user.
    let hr = unsafe {
        SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut raw)
    };
    let path = if hr >= 0 && !raw.is_null() {
        // SAFETY: on success `raw` is a NUL-terminated wide string.
        let wide = unsafe {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            std::slice::from_raw_parts(raw, len)
        };
        Some(PathBuf::from(OsString::from_wide(wide)))
    } else {
        None
    };
    // SAFETY: the shell allocated `raw` (or left it null); it must be freed
    // with CoTaskMemFree whether or not the call succeeded.
    unsafe { CoTaskMemFree(raw as *const _) };
    path.ok_or_else(|| Error::Private(format!("no local app data folder (HRESULT {hr:#x})")))
}

/// The user's home folder from the password database, never from `$HOME`,
/// which is attacker-controlled (§6).
#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn home_dir(uid: u32) -> Result<PathBuf, Error> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: `pwd` and `result` are valid out-pointers, and `buf` is a
        // writable buffer of the length we pass. On success `result` points
        // at `pwd`, whose strings live in `buf`, which outlives their use.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return Err(Error::Private(format!("no home folder for uid {uid}")));
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buf`.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Ok(PathBuf::from(OsStr::from_bytes(dir.to_bytes())));
    }
}
