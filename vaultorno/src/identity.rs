//! Who is asking (design doc §7). [`SignedIdentity`] is the daemon's check:
//! a signed statement first (level `airtight`, Milestone 3), then a package
//! manifest through [`ManifestIdentity`] (level `watertight`, Milestone 2),
//! with one inspection of the caller, environment and `ptrace_scope` checks.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ssh_key::PublicKey;
use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::{
    DEFAULT_MANIFESTS_DIR, Grant, Manifest, Statement, SuccessionStatement, find_rejected_env,
};
use vaultorno_client::time::format_rfc3339;
use vaultorno_inspect::{
    DEFAULT_INSPECT_SOCKET, Inspected, inspect_pidfd, request_inspection,
};

use crate::signing::{
    KeyFingerprint, KeyListFiles, KeyLists, STATEMENT_NAMESPACE, SUCCESSION_NAMESPACE, SignedBy,
    parse_key_line, verify_signature,
};

/// Level reported for package manifests (§4).
pub const WATERTIGHT_LEVEL: &str = "watertight";
/// Level reported for signed statements (§4).
pub const AIRTIGHT_LEVEL: &str = "airtight";
/// The `ptrace_scope` a signed statement needs (§4).
pub const AIRTIGHT_PTRACE_SCOPE: u32 = 2;
/// Default path to Yama's `ptrace_scope` sysctl (§7.6).
pub const DEFAULT_PTRACE_SCOPE_PATH: &str = "/proc/sys/kernel/yama/ptrace_scope";

/// The process at the other end of a connection.
#[derive(Debug, Clone)]
pub struct Peer {
    /// From `SO_PEERCRED`.
    pub uid: u32,
    pub pid: Option<i32>,
    /// Pinned process handle for the inspector.
    pub pidfd: Option<Arc<OwnedFd>>,
}

impl Peer {
    pub fn without_pidfd(uid: u32, pid: Option<i32>) -> Peer {
        Peer {
            uid,
            pid,
            pidfd: None,
        }
    }
}

/// What an identity check decided: the vault, the binary and its lockers.
#[derive(Debug, Clone)]
pub struct Identity {
    pub app: String,
    pub vault: String,
    pub binary: String,
    pub level: String,
    pub lockers: BTreeMap<String, Grant>,
    pub statement_expires: Option<u64>,
}

/// Why an identity check failed. `vault` is set when the caller's vault is
/// known, so the refusal can be counted for `Welcome.refused`.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub vault: Option<String>,
    pub message: String,
}

fn refused(vault: Option<String>, message: impl Into<String>) -> Refusal {
    Refusal {
        vault,
        message: message.into(),
    }
}

/// An identity check.
pub trait Identify: Send + Sync {
    /// Whether the state the check relies on is readable. If not, the daemon
    /// answers every `Hello` with `Storage` rather than guess (§5, §9).
    fn check_state(&self) -> Result<(), String> {
        Ok(())
    }

    /// `succession` is a succession statement and its signature, sent with a
    /// statement signed by the new key after a key rotation (§8).
    fn identify(
        &self,
        peer: &Peer,
        statement: Option<&str>,
        signature: Option<&str>,
        succession: Option<(&str, &str)>,
        now: u64,
    ) -> Result<Identity, Refusal>;
}

// ---------------------------------------------------------------------------
// Inspector backends

/// How the daemon inspects a caller process (§7.3, §14).
pub trait Inspector: Send + Sync {
    fn inspect(&self, peer: &Peer) -> Result<Inspected, String>;
}

/// Connects to `vaultorno inspect` over a Unix socket (`SCM_RIGHTS`).
pub struct SocketInspector {
    socket: PathBuf,
    expected_uid: Option<u32>,
}

impl SocketInspector {
    pub fn new(socket: impl Into<PathBuf>, expected_uid: Option<u32>) -> Self {
        SocketInspector {
            socket: socket.into(),
            expected_uid,
        }
    }

    pub fn default_socket() -> Self {
        SocketInspector::new(DEFAULT_INSPECT_SOCKET, None)
    }
}

impl Inspector for SocketInspector {
    fn inspect(&self, peer: &Peer) -> Result<Inspected, String> {
        let pidfd = peer
            .pidfd
            .as_ref()
            .ok_or_else(|| "caller connection has no pidfd".to_owned())?;
        request_inspection(&self.socket, pidfd.as_fd(), self.expected_uid)
    }
}

/// Runs `inspect_pidfd` in the current process (for dev/test use when the
/// separate `vaultorno inspect` service isn't running).
pub struct InProcessInspector;

impl Inspector for InProcessInspector {
    fn inspect(&self, peer: &Peer) -> Result<Inspected, String> {
        let pidfd = peer
            .pidfd
            .as_ref()
            .ok_or_else(|| "caller connection has no pidfd".to_owned())?;
        inspect_pidfd(pidfd.as_fd())
    }
}

/// A controllable inspector for in-process integration tests (§17).
#[derive(Default)]
pub struct FakeInspector {
    by_pid: Mutex<HashMap<i32, Inspected>>,
    default: Mutex<Option<Inspected>>,
}

impl FakeInspector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_default(&self, inspected: Inspected) {
        *self.default.lock().unwrap() = Some(inspected);
    }

    pub fn insert(&self, pid: i32, inspected: Inspected) {
        self.by_pid.lock().unwrap().insert(pid, inspected);
    }
}

impl Inspector for FakeInspector {
    fn inspect(&self, peer: &Peer) -> Result<Inspected, String> {
        if let Some(pid) = peer.pid {
            if let Some(ins) = self.by_pid.lock().unwrap().get(&pid) {
                return Ok(ins.clone());
            }
        }
        self.default
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "fake inspector has no entry for caller".into())
    }
}

// ---------------------------------------------------------------------------
// Package manifest identity (`watertight`, §4, §5, §7)

/// Identifies callers through package manifests in `/usr/share/vaultorno/manifests/`,
/// with an optional fallback to [`TestIdentity`] for `--test-identity`.
pub struct ManifestIdentity {
    manifests_dir: PathBuf,
    root_dir: PathBuf,
    trusted_uid: u32,
    ptrace_scope_path: PathBuf,
    inspector: Arc<dyn Inspector>,
    test_fallback: bool,
}

impl ManifestIdentity {
    /// Production constructor: trusts only uid `0` (root), walks parent
    /// directories all the way to `/`, reads `/usr/share/vaultorno/manifests`,
    /// and checks `/proc/sys/kernel/yama/ptrace_scope`.
    pub fn new(inspector: Arc<dyn Inspector>, test_fallback: bool) -> Self {
        ManifestIdentity {
            manifests_dir: PathBuf::from(DEFAULT_MANIFESTS_DIR),
            root_dir: PathBuf::from("/"),
            trusted_uid: 0,
            ptrace_scope_path: PathBuf::from(DEFAULT_PTRACE_SCOPE_PATH),
            inspector,
            test_fallback,
        }
    }

    /// Test constructor: allows tests running as an unprivileged user to
    /// place manifests and test binaries inside a temporary root directory
    /// owned by `trusted_uid` (§17).
    pub fn for_testing(
        manifests_dir: impl Into<PathBuf>,
        root_dir: impl Into<PathBuf>,
        trusted_uid: u32,
        ptrace_scope_path: impl Into<PathBuf>,
        inspector: Arc<dyn Inspector>,
        test_fallback: bool,
    ) -> Self {
        ManifestIdentity {
            manifests_dir: manifests_dir.into(),
            root_dir: root_dir.into(),
            trusted_uid,
            ptrace_scope_path: ptrace_scope_path.into(),
            inspector,
            test_fallback,
        }
    }

    fn match_manifest(&self, inspected: &Inspected) -> Result<Option<Identity>, Refusal> {
        let rd = match fs::read_dir(&self.manifests_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(Refusal {
                    vault: None,
                    message: format!("can't read {}: {e}", self.manifests_dir.display()),
                });
            }
            Ok(rd) => rd,
        };

        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if !is_valid_name(stem) {
                continue;
            }
            let vault_hint = Some(format!("pkg_{stem}"));

            // Check manifest file ownership & permissions before trusting it.
            check_safe_path(&path, &self.root_dir, self.trusted_uid, false).map_err(|msg| {
                Refusal {
                    vault: vault_hint.clone(),
                    message: msg,
                }
            })?;

            let text = fs::read_to_string(&path).map_err(|e| Refusal {
                vault: vault_hint.clone(),
                message: format!("can't read {}: {e}", path.display()),
            })?;
            let manifest = Manifest::parse(&text)
                .and_then(|m| m.check())
                .map_err(|e| Refusal {
                    vault: vault_hint.clone(),
                    message: format!("{}: {}", path.display(), e.0),
                })?;
            if manifest.app != stem {
                return Err(Refusal {
                    vault: vault_hint,
                    message: format!(
                        "{}: manifest app {:?} doesn't match file name",
                        path.display(),
                        manifest.app
                    ),
                });
            }
            let vault = format!("pkg_{}", manifest.app);

            for (bin_name, bin) in &manifest.binaries {
                let bin_path = Path::new(&bin.path);
                let Ok(meta) = fs::symlink_metadata(bin_path) else {
                    continue;
                };
                if !meta.file_type().is_file() {
                    continue;
                }
                if meta.dev() != inspected.dev || meta.ino() != inspected.ino {
                    continue;
                }

                // Matched! Now verify binary ownership/permissions, env, and ptrace_scope.
                check_safe_path(bin_path, &self.root_dir, self.trusted_uid, true).map_err(
                    |msg| Refusal {
                        vault: Some(vault.clone()),
                        message: msg,
                    },
                )?;

                let mut extra_env = manifest.reject_env.clone();
                extra_env.extend(bin.reject_env.iter().cloned());
                if let Some(bad_var) = find_rejected_env(&inspected.env_names, &extra_env) {
                    return Err(Refusal {
                        vault: Some(vault),
                        message: format!("{bad_var} set"),
                    });
                }

                check_ptrace_scope(&self.ptrace_scope_path, WATERTIGHT_LEVEL, 1).map_err(
                    |msg| Refusal {
                        vault: Some(vault.clone()),
                        message: msg,
                    },
                )?;

                return Ok(Some(Identity {
                    app: manifest.app,
                    vault,
                    binary: bin_name.clone(),
                    level: WATERTIGHT_LEVEL.into(),
                    lockers: bin.lockers.clone(),
                    statement_expires: None,
                }));
            }
        }
        Ok(None)
    }
}

impl ManifestIdentity {
    /// Inspects the caller. `None` means there is nothing to inspect and the
    /// test fallback applies (a development daemon serving a connection
    /// without a pidfd).
    fn inspect_peer(&self, peer: &Peer) -> Result<Option<Inspected>, Refusal> {
        match self.inspector.inspect(peer) {
            Ok(inspected) => {
                if inspected.uid != peer.uid {
                    return Err(Refusal {
                        vault: None,
                        message: format!(
                            "inspected process uid {} doesn't match peer uid {}",
                            inspected.uid, peer.uid
                        ),
                    });
                }
                Ok(Some(inspected))
            }
            Err(inspect_err) => {
                if self.test_fallback && peer.pidfd.is_none() {
                    return Ok(None);
                }
                Err(Refusal {
                    vault: None,
                    message: format!("inspection failed: {inspect_err}"),
                })
            }
        }
    }

    /// The manifest check, and the test fallback, for an inspected caller.
    fn identify_inspected(
        &self,
        peer: &Peer,
        inspected: &Inspected,
        statement: Option<&str>,
        signature: Option<&str>,
        now: u64,
    ) -> Result<Identity, Refusal> {
        if let Some(ident) = self.match_manifest(inspected)? {
            return Ok(ident);
        }
        if self.test_fallback {
            // Even under test fallback, enforce the environment reject
            // list when we have an inspected process.
            if let Some(bad_var) = find_rejected_env(&inspected.env_names, &[]) {
                return Err(Refusal {
                    vault: None,
                    message: format!("{bad_var} set"),
                });
            }
            return TestIdentity.identify(peer, statement, signature, None, now);
        }
        Err(Refusal {
            vault: None,
            message: format!(
                "executable {} isn't listed in any package manifest",
                inspected.exe_path.display()
            ),
        })
    }
}

impl Identify for ManifestIdentity {
    fn identify(
        &self,
        peer: &Peer,
        statement: Option<&str>,
        signature: Option<&str>,
        _succession: Option<(&str, &str)>,
        now: u64,
    ) -> Result<Identity, Refusal> {
        match self.inspect_peer(peer)? {
            Some(inspected) => self.identify_inspected(peer, &inspected, statement, signature, now),
            None => TestIdentity.identify(peer, statement, signature, None, now),
        }
    }
}

// ---------------------------------------------------------------------------
// Signed statement identity (`airtight`, §4, §5, §7, §8)

/// The daemon's identity check (§7.4): a signed statement first, then a
/// package manifest, with one inspection of the caller. The vault is named by
/// the fingerprint of the key that signed the statement, or of the first key
/// in its line of `successions` (§6, §8). Every `Hello` is refused while
/// `revoked_keys` or `successions` doesn't parse.
pub struct SignedIdentity {
    manifests: ManifestIdentity,
    key_lists: KeyListFiles,
    /// Serialises this daemon's writes to `successions` (§8).
    recording: Mutex<()>,
}

/// A succession from `Hello` that checked out and isn't recorded yet.
struct NewSuccession {
    from: KeyFingerprint,
    /// The new key, as it will be written to `successions`.
    to: PublicKey,
}

impl SignedIdentity {
    /// `state_dir` holds `revoked_keys` and `successions` (§6).
    pub fn new(manifests: ManifestIdentity, state_dir: impl Into<PathBuf>) -> Self {
        SignedIdentity {
            manifests,
            key_lists: KeyListFiles::new(state_dir),
            recording: Mutex::new(()),
        }
    }

    /// Checks a succession sent with a statement `signer` signed (§8):
    /// signed by the key it's from, handing over to `signer`, from a key
    /// that isn't revoked, and fitting the recorded successions. `None` if
    /// it's recorded already.
    fn check_succession(
        &self,
        lists: &KeyLists,
        signer: &SignedBy,
        text: &str,
        signature: &str,
        now: u64,
    ) -> Result<Option<NewSuccession>, Refusal> {
        let bad = |m: String| refused(None, format!("succession: {m}"));
        // Signature first, parsing second (§5).
        let by = verify_signature(SUCCESSION_NAMESPACE, text.as_bytes(), signature)
            .map_err(|m| bad(format!("signature: {m}")))?;
        let checked = SuccessionStatement::parse(text)
            .and_then(|s| s.check(now))
            .map_err(|e| bad(e.0))?;
        let from = KeyFingerprint::parse(&checked.from)
            .ok_or_else(|| bad(format!("from {:?} isn't a SHA256: fingerprint", checked.from)))?;
        if by.fingerprint != from {
            return Err(bad(format!(
                "signed by {}, not by the key it's from, {from}",
                by.fingerprint
            )));
        }
        let to = parse_key_line(&checked.to).map_err(|m| bad(format!("to: {m}")))?;
        let to_fingerprint = KeyFingerprint::of(&to);
        if to_fingerprint != signer.fingerprint {
            return Err(bad(format!(
                "it hands over to {to_fingerprint}, but the statement is signed by {}",
                signer.fingerprint
            )));
        }
        if lists.is_revoked(&from) {
            return Err(bad(format!("from revoked key {from}")));
        }
        match lists.check_new_succession(&from, &to_fingerprint) {
            Ok(true) => Ok(Some(NewSuccession { from, to })),
            Ok(false) => Ok(None),
            Err(m) => Err(bad(m)),
        }
    }

    /// Appends a checked succession to `successions` (§8). Another
    /// connection may have recorded it meanwhile, which is fine; one that
    /// now conflicts with it is refused.
    fn record(&self, succession: &NewSuccession, now: u64) -> Result<(), String> {
        let _writer = self.recording.lock().unwrap_or_else(|p| p.into_inner());
        let note = format!(
            "recorded {} from a succession statement",
            format_rfc3339(now)
        );
        self.key_lists
            .record_succession(&succession.from, &succession.to, &note)
            .map(drop)
            .map_err(|m| format!("can't record the succession: {m}"))
    }

    fn check_statement(
        &self,
        lists: &KeyLists,
        inspected: &Inspected,
        text: &str,
        signature: &str,
        succession: Option<(&str, &str)>,
        now: u64,
    ) -> Result<Identity, Refusal> {
        // Signature first, parsing second (§5).
        let signer = verify_signature(STATEMENT_NAMESPACE, text.as_bytes(), signature)
            .map_err(|m| refused(None, format!("statement signature: {m}")))?;
        if lists.is_revoked(&signer.fingerprint) {
            return Err(refused(
                Some(lists.vault_of(&signer.fingerprint).hex()),
                format!("statement signed by revoked key {}", signer.fingerprint),
            ));
        }
        let new_succession = match succession {
            Some((s_text, s_signature)) => {
                self.check_succession(lists, &signer, s_text, s_signature, now)?
            }
            None => None,
        };
        // Until it's recorded, a new succession carries the old key's vault.
        let vault = match &new_succession {
            Some(s) => lists.vault_of(&s.from),
            None => lists.vault_of(&signer.fingerprint),
        }
        .hex();
        let hint = || Some(vault.clone());
        let checked = Statement::parse(text)
            .and_then(|s| s.check(now))
            .map_err(|e| refused(hint(), e.0))?;
        let actual = inspected.sha256_hex();
        if checked.sha256 != actual {
            return Err(refused(
                hint(),
                format!(
                    "the statement is for executable {}…, not {}… ({})",
                    &checked.sha256[..8],
                    &actual[..8],
                    inspected.exe_path.display()
                ),
            ));
        }
        if let Some(bad_var) = find_rejected_env(&inspected.env_names, &checked.reject_env) {
            return Err(refused(hint(), format!("{bad_var} set")));
        }
        if checked.level.is_some_and(|l| l != AIRTIGHT_LEVEL) {
            // `hermetic` also needs the TPM2-sealed master key (§4).
            return Err(refused(
                hint(),
                "hermetic needs a TPM2-sealed master key, which this build doesn't have (Milestone 4)",
            ));
        }
        check_ptrace_scope(
            &self.manifests.ptrace_scope_path,
            AIRTIGHT_LEVEL,
            AIRTIGHT_PTRACE_SCOPE,
        )
        .map_err(|m| refused(hint(), m))?;
        // Recorded only once everything else has passed (§8).
        if let Some(succession) = &new_succession {
            self.record(succession, now).map_err(|m| refused(hint(), m))?;
        }
        Ok(Identity {
            app: checked.app,
            vault,
            binary: checked.binary,
            level: AIRTIGHT_LEVEL.into(),
            lockers: checked.lockers,
            statement_expires: Some(checked.expires),
        })
    }
}

impl Identify for SignedIdentity {
    fn check_state(&self) -> Result<(), String> {
        self.key_lists.load().map(|_| ())
    }

    fn identify(
        &self,
        peer: &Peer,
        statement: Option<&str>,
        signature: Option<&str>,
        succession: Option<(&str, &str)>,
        now: u64,
    ) -> Result<Identity, Refusal> {
        let lists = self.key_lists.load().map_err(|m| refused(None, m))?;
        let Some(inspected) = self.manifests.inspect_peer(peer)? else {
            return TestIdentity.identify(peer, statement, signature, None, now);
        };
        // A succession only matters with a signed statement: a package
        // manifest doesn't care which key an app uses.
        let (Some(text), Some(signature)) = (statement, signature) else {
            return self
                .manifests
                .identify_inspected(peer, &inspected, statement, signature, now);
        };
        match self.check_statement(&lists, &inspected, text, signature, succession, now) {
            Ok(ident) => Ok(ident),
            // A package manifest may still vouch for the binary (§7.4). If
            // none does, the statement's reason is the one worth reporting;
            // a signed statement never falls back to the test identity.
            Err(refusal) => match self.manifests.match_manifest(&inspected) {
                Ok(Some(ident)) => Ok(ident),
                _ => Err(refusal),
            },
        }
    }
}

/// Checks that `path` is a regular file (not a symlink), owned by root (0) or
/// `trusted_uid`, not writable by group or others (`mode & 0o022 == 0`), and
/// if `executable` is true, world-readable (`mode & 0o004 != 0`), and that
/// every parent directory up to `root_dir` is a real directory owned by root
/// or `trusted_uid` and not writable by group or others (§7.4).
pub fn check_safe_path(
    path: &Path,
    root_dir: &Path,
    trusted_uid: u32,
    executable: bool,
) -> Result<(), String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("can't stat {}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} isn't a regular file", path.display()));
    }
    check_owner_and_not_writable(path, &meta, trusted_uid)?;
    if executable && (meta.mode() & 0o004 == 0) {
        return Err(format!(
            "{} has mode {:o}; executables must be world-readable",
            path.display(),
            meta.mode() & 0o777
        ));
    }

    let mut cur = path.parent();
    while let Some(dir) = cur {
        let dir_meta = fs::symlink_metadata(dir)
            .map_err(|e| format!("can't stat {}: {e}", dir.display()))?;
        if !dir_meta.file_type().is_dir() {
            return Err(format!("{} isn't a directory", dir.display()));
        }
        check_owner_and_not_writable(dir, &dir_meta, trusted_uid)?;
        if dir == root_dir {
            break;
        }
        cur = dir.parent();
    }
    Ok(())
}

fn check_owner_and_not_writable(
    path: &Path,
    meta: &fs::Metadata,
    trusted_uid: u32,
) -> Result<(), String> {
    let uid = meta.uid();
    if uid != 0 && uid != trusted_uid {
        return Err(format!(
            "{} is owned by uid {uid}, not root",
            path.display()
        ));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(format!(
            "{} has mode {mode:o}; must not be writable by group or others",
            path.display()
        ));
    }
    Ok(())
}

/// Checks `/proc/sys/kernel/yama/ptrace_scope` against a level's minimum (§7.6).
pub fn check_ptrace_scope(path: &Path, level: &str, min_scope: u32) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let scope = text
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("invalid ptrace_scope {:?} in {}", text.trim(), path.display()))?;
    if scope < min_scope {
        return Err(format!(
            "ptrace_scope is {scope}, {level} needs {min_scope}"
        ));
    }
    Ok(())
}

/// **Milestone 1 only, for development and tests.** Trusts the statement in
/// `Hello` without checking a signature or the executable's hash, and opens
/// the vault `test_<app>`. Everything else about the statement is checked:
/// format, names, validity window, lockers, lifetimes.
pub struct TestIdentity;

/// The level reported for [`TestIdentity`].
pub const TEST_LEVEL: &str = "test identity (development only)";

impl Identify for TestIdentity {
    fn identify(
        &self,
        _peer: &Peer,
        statement: Option<&str>,
        _signature: Option<&str>,
        _succession: Option<(&str, &str)>,
        now: u64,
    ) -> Result<Identity, Refusal> {
        let text = statement.ok_or_else(|| Refusal {
            vault: None,
            message: "hello carries no statement".into(),
        })?;
        let parsed = Statement::parse(text).map_err(|e| Refusal {
            vault: None,
            message: e.0,
        })?;
        let vault = is_valid_name(&parsed.app).then(|| format!("test_{}", parsed.app));
        let checked = parsed.check(now).map_err(|e| Refusal {
            vault: vault.clone(),
            message: e.0,
        })?;
        Ok(Identity {
            vault: format!("test_{}", checked.app),
            app: checked.app,
            binary: checked.binary,
            level: TEST_LEVEL.into(),
            lockers: checked.lockers,
            statement_expires: Some(checked.expires),
        })
    }
}
