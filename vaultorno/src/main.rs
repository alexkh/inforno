//! `vaultorno`: one executable, run in modes (design doc §3). What it does
//! depends only on its first argument; what it may do depends only on how
//! it was started.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use vaultorno::activation;
use vaultorno::identity::{Inspector, ManifestIdentity, SignedIdentity, SocketInspector};
use vaultorno::master_key::{CREDENTIAL_FILE, MasterKey};
use vaultorno::server::{DEFAULT_BACKOFF_BASE, Server};
use vaultorno::signing::{KeyFingerprint, KeyListFiles, edit_revoked_keys, parse_key_line};
use vaultorno::store::Store;
use vaultorno::sweep::sweep;
use vaultorno::time::{Clock, SystemClock};
use vaultorno_inspect::{DEFAULT_INSPECT_SOCKET, serve_inspect};

const USAGE: &str = "\
usage: vaultorno <mode> [options]

modes in this build (Milestone 4):
  init [--state-dir DIR]
      create the state folder and seal a new master key (as root, once)
  daemon [--state-dir DIR] [--inspect-socket PATH]
      serve apps on the socket vaultorno.socket passes as standard input, with
      the master key systemd unseals into $CREDENTIALS_DIRECTORY
  inspect [--state-dir DIR]
      inspect callers for the daemon, on the socket vaultorno_inspect.socket
      passes; only the owner of the state folder may ask
  sweep [--state-dir DIR]
      delete expired lockers and stale vaults
  revoke [--undo] [--state-dir DIR] <public key or .pub file>
      refuse statements signed by a key from now on (as root)

later milestones: status, check, vaults, log, forget

development builds only (--features test_identity):
  daemon --test-identity [--socket PATH] [--in-process-inspector]
  inspect [--socket PATH]
      --socket binds PATH instead of taking a socket from systemd

defaults:
  --state-dir      /var/lib/vaultorno
  --inspect-socket /run/vaultorno_inspect.sock";

fn main() -> ExitCode {
    let args: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect()
    {
        Ok(a) => a,
        Err(_) => return fail("arguments must be UTF-8"),
    };
    let Some(mode) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match mode.as_str() {
        "daemon" => daemon(rest),
        "inspect" => inspect(rest),
        "sweep" => run_sweep(rest),
        "revoke" => revoke(rest),
        "init" => init(rest),
        "status" | "check" | "vaults" | "log" | "forget" => {
            fail(&format!("`vaultorno {mode}` arrives in a later milestone"))
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("vaultorno: unknown mode {other:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("vaultorno: {msg}");
    ExitCode::FAILURE
}

struct Options {
    state_dir: PathBuf,
    inspect_socket: PathBuf,
    test_identity: bool,
    /// Bind this socket instead of taking one from systemd.
    #[cfg(feature = "test_identity")]
    socket: Option<PathBuf>,
    /// Inspect callers in the daemon's own process, never through whatever
    /// inspector runs on this machine.
    #[cfg(feature = "test_identity")]
    in_process_inspector: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        state_dir: PathBuf::from("/var/lib/vaultorno"),
        inspect_socket: PathBuf::from(DEFAULT_INSPECT_SOCKET),
        test_identity: false,
        #[cfg(feature = "test_identity")]
        socket: None,
        #[cfg(feature = "test_identity")]
        in_process_inspector: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => opts.state_dir = it.next().ok_or("--state-dir needs a folder")?.into(),
            "--inspect-socket" => {
                opts.inspect_socket = it.next().ok_or("--inspect-socket needs a path")?.into()
            }
            "--test-identity" => opts.test_identity = true,
            #[cfg(feature = "test_identity")]
            "--socket" => opts.socket = Some(it.next().ok_or("--socket needs a path")?.into()),
            #[cfg(feature = "test_identity")]
            "--in-process-inspector" => opts.in_process_inspector = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(opts)
}

fn daemon(args: &[String]) -> ExitCode {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    serve(opts)
}

fn serve(opts: Options) -> ExitCode {
    if opts.test_identity {
        #[cfg(not(feature = "test_identity"))]
        {
            return fail(
                "`--test-identity` requires a development build (`--features test_identity`)",
            );
        }
        #[cfg(feature = "test_identity")]
        eprintln!(
            "vaultorno daemon: WARNING: test identity enabled. Unsigned statements are trusted when no package manifest matches. \
             Development only."
        );
    }

    // Before anything touches the state or the socket: no master key, no
    // daemon (§6).
    let key = match MasterKey::from_credentials_directory() {
        Ok(k) => k,
        Err(e) => return fail(&e),
    };
    eprintln!(
        "vaultorno daemon: master key: {}",
        key.sealing().describe()
    );
    // No socket from systemd, no daemon: refused before the state is touched.
    let listener = match daemon_listener(&opts) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let store = match Store::open(&opts.state_dir, key) {
        Ok(s) => s,
        Err(e) => return fail(&format!("can't open {}: {e}", opts.state_dir.display())),
    };

    let inspector: Arc<dyn Inspector> = Arc::new(SocketInspector::new(&opts.inspect_socket));
    #[cfg(feature = "test_identity")]
    let inspector: Arc<dyn Inspector> = if opts.in_process_inspector {
        Arc::new(vaultorno::identity::InProcessInspector)
    } else {
        inspector
    };
    let identity = SignedIdentity::new(
        ManifestIdentity::new(inspector, opts.test_identity),
        store.root(),
    );
    let server = Server::with_backoff(
        store,
        Box::new(identity),
        Arc::new(SystemClock),
        DEFAULT_BACKOFF_BASE,
    );
    match server.listen(listener) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("serving stopped: {e}")),
    }
}

/// The daemon's listening socket: the one systemd passes (§14) or, in a
/// development build given `--socket`, one it binds itself.
#[cfg_attr(not(feature = "test_identity"), allow(unused_variables))]
fn daemon_listener(opts: &Options) -> Result<UnixListener, String> {
    #[cfg(feature = "test_identity")]
    if let Some(path) = &opts.socket {
        return activation::bind(path, 0o666)
            .map_err(|e| format!("can't listen on {}: {e}", path.display()));
    }
    activation::from_systemd(Path::new(vaultorno_client::protocol::DEFAULT_SOCKET))
}

fn init(args: &[String]) -> ExitCode {
    let mut state_dir = PathBuf::from("/var/lib/vaultorno");
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => match it.next() {
                Some(d) => state_dir = d.into(),
                None => return fail("--state-dir needs a folder"),
            },
            other => return fail(&format!("unknown option {other:?}")),
        }
    }
    match vaultorno::init::init(&state_dir) {
        Ok(sealing) => {
            println!(
                "sealed a new master key into {} ({})",
                state_dir.join(CREDENTIAL_FILE).display(),
                sealing.describe()
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn inspect(args: &[String]) -> ExitCode {
    // The daemon's user is whoever owns its state folder (§3).
    let mut state_dir = PathBuf::from("/var/lib/vaultorno");
    #[cfg(feature = "test_identity")]
    let mut socket: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => match it.next() {
                Some(d) => state_dir = d.into(),
                None => return fail("--state-dir needs a folder"),
            },
            #[cfg(feature = "test_identity")]
            "--socket" => match it.next() {
                Some(p) => socket = Some(p.into()),
                None => return fail("--socket needs a path"),
            },
            other => return fail(&format!("unknown option {other:?}")),
        }
    }
    let systemd = || activation::from_systemd(Path::new(DEFAULT_INSPECT_SOCKET));
    #[cfg(feature = "test_identity")]
    let listener = match &socket {
        Some(path) => activation::bind(path, 0o660)
            .map_err(|e| format!("can't listen on {}: {e}", path.display())),
        None => systemd(),
    };
    #[cfg(not(feature = "test_identity"))]
    let listener = systemd();
    let listener = match listener {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    match serve_inspect(listener, &state_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("the inspector stopped: {e}")),
    }
}

fn run_sweep(args: &[String]) -> ExitCode {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    // Headers only: the sweep never needs the master key (§6).
    let store = match Store::open_without_key(&opts.state_dir) {
        Ok(s) => s,
        Err(e) => return fail(&format!("can't open {}: {e}", opts.state_dir.display())),
    };
    match sweep(&store, SystemClock.now()) {
        Ok(r) => {
            println!(
                "swept: {} expired lockers, {} stale vaults, {} leftover temp files",
                r.expired_lockers, r.stale_vaults, r.temp_files
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("sweep failed: {e}")),
    }
}

fn revoke(args: &[String]) -> ExitCode {
    let mut state_dir = PathBuf::from("/var/lib/vaultorno");
    let mut undo = false;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--undo" => undo = true,
            "--state-dir" => match it.next() {
                Some(d) => state_dir = d.into(),
                None => return fail("--state-dir needs a folder"),
            },
            other if other.starts_with("--") => return fail(&format!("unknown option {other:?}")),
            word => words.push(word),
        }
    }
    if words.is_empty() {
        return fail("revoke needs a public key, or the path of a .pub file");
    }
    // One argument naming a file is a .pub file; otherwise the words are the
    // key line itself, quoted or not.
    let text = match words.as_slice() {
        [one] if Path::new(one).is_file() => match fs::read_to_string(one) {
            Ok(t) => t,
            Err(e) => return fail(&format!("can't read {one}: {e}")),
        },
        _ => words.join(" "),
    };
    let Some(line) = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
    else {
        return fail("no public key given");
    };
    let key = match parse_key_line(line) {
        Ok(k) => k,
        Err(e) => return fail(&e),
    };
    if !state_dir.is_dir() {
        return fail(&format!("no state folder at {}", state_dir.display()));
    }
    let fingerprint = KeyFingerprint::of(&key);
    match edit_revoked_keys(&state_dir, &key, undo) {
        Ok(true) if undo => println!(
            "no longer revoked: {fingerprint}; its statements work again"
        ),
        Ok(true) => println!(
            "revoked {fingerprint} (key id {}); new connections refuse its statements at once",
            fingerprint.key_id()
        ),
        Ok(false) if undo => println!("{fingerprint} wasn't revoked"),
        Ok(false) => println!("{fingerprint} was already revoked"),
        Err(e) => return fail(&e),
    }
    if !undo {
        warn_about_successors(&state_dir, &fingerprint);
    }
    ExitCode::SUCCESS
}

/// A stolen key may have handed its vault on before it was revoked (§8):
/// name the keys that keep that vault through it.
fn warn_about_successors(state_dir: &Path, key: &KeyFingerprint) {
    let Ok(lists) = KeyListFiles::new(state_dir).load() else {
        return;
    };
    let successors = lists.successors_of(key);
    if successors.is_empty() {
        return;
    }
    println!("note: successions hand its vault on to these keys, which keep it:");
    for successor in successors {
        println!("  {successor}");
    }
    println!("revoke them too unless they are yours");
}
