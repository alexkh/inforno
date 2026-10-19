//! `vaultorno`: one executable, run in modes (design doc §3). What it does
//! depends only on its first argument; what it may do depends only on how
//! it was started.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use vaultorno::identity::{InProcessInspector, Inspector, ManifestIdentity, SocketInspector};
use vaultorno::server::{DEFAULT_BACKOFF_BASE, Server};
use vaultorno::store::Store;
use vaultorno::sweep::sweep;
use vaultorno::time::{Clock, SystemClock};
use vaultorno_inspect::{DEFAULT_INSPECT_SOCKET, serve_inspect};

const USAGE: &str = "\
usage: vaultorno <mode> [options]

modes in this build (Milestone 2):
  daemon [--state-dir DIR] [--socket PATH] [--inspect-socket PATH] [--test-identity]
      serve apps
  inspect [--socket PATH] [--daemon-uid UID]
      run the process identity inspector
  sweep [--state-dir DIR]
      delete expired lockers and stale vaults

later milestones: init, status, check, vaults, log, revoke, forget

defaults:
  --state-dir      /var/lib/vaultorno
  --socket         /run/vaultorno.sock (daemon) or /run/vaultorno_inspect.sock (inspect)
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
        "init" | "status" | "check" | "vaults" | "log" | "revoke" | "forget" => {
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
    socket: PathBuf,
    inspect_socket: PathBuf,
    test_identity: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        state_dir: PathBuf::from("/var/lib/vaultorno"),
        socket: PathBuf::from(vaultorno_client::protocol::DEFAULT_SOCKET),
        inspect_socket: PathBuf::from(DEFAULT_INSPECT_SOCKET),
        test_identity: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => opts.state_dir = it.next().ok_or("--state-dir needs a folder")?.into(),
            "--socket" => opts.socket = it.next().ok_or("--socket needs a path")?.into(),
            "--inspect-socket" => {
                opts.inspect_socket = it.next().ok_or("--inspect-socket needs a path")?.into()
            }
            "--test-identity" => opts.test_identity = true,
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

    let store = match Store::open(&opts.state_dir) {
        Ok(s) => s,
        Err(e) => return fail(&format!("can't open {}: {e}", opts.state_dir.display())),
    };

    let inspector: Arc<dyn Inspector> = if opts.test_identity && !opts.inspect_socket.exists() {
        Arc::new(InProcessInspector)
    } else {
        Arc::new(SocketInspector::new(&opts.inspect_socket, None))
    };
    let identity = ManifestIdentity::new(inspector, opts.test_identity);
    let server = Server::with_backoff(
        store,
        Box::new(identity),
        Arc::new(SystemClock),
        DEFAULT_BACKOFF_BASE,
    );
    match server.listen(&opts.socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("can't listen on {}: {e}", opts.socket.display())),
    }
}

fn inspect(args: &[String]) -> ExitCode {
    let mut socket = PathBuf::from(DEFAULT_INSPECT_SOCKET);
    let mut daemon_uid: Option<u32> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--socket" => match it.next() {
                Some(p) => socket = p.into(),
                None => return fail("--socket needs a path"),
            },
            "--daemon-uid" => match it.next().and_then(|s| s.parse::<u32>().ok()) {
                Some(uid) => daemon_uid = Some(uid),
                None => return fail("--daemon-uid needs a numeric uid"),
            },
            other => return fail(&format!("unknown option {other:?}")),
        }
    }
    let allowed_uid = daemon_uid.unwrap_or_else(|| rustix::process::geteuid().as_raw());
    match serve_inspect(&socket, allowed_uid) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!(
            "can't serve inspector on {}: {e}",
            socket.display()
        )),
    }
}

fn run_sweep(args: &[String]) -> ExitCode {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    let store = match Store::open(&opts.state_dir) {
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
