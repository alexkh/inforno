//! `vaultorno`: one executable, run in modes (design doc §3). What it does
//! depends only on its first argument; what it may do depends only on how
//! it was started.

use std::path::PathBuf;
use std::process::ExitCode;

use vaultorno::store::Store;
use vaultorno::sweep::sweep;
use vaultorno::time::{Clock, SystemClock};

const USAGE: &str = "\
usage: vaultorno <mode> [options]

modes in this build (Milestone 1):
  daemon [--state-dir DIR] [--socket PATH] [--test-identity]
      serve apps; development builds only, see below
  sweep [--state-dir DIR]
      delete expired lockers and stale vaults

later milestones: inspect, init, status, check, vaults, log, revoke, forget

defaults: --state-dir /var/lib/vaultorno, --socket /run/vaultorno.sock

Milestone 1 has no identity checks yet. `vaultorno daemon` only serves when
built with `--features test_identity` and started with `--test-identity`,
which trusts unsigned statements: development only.";

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
        "sweep" => run_sweep(rest),
        "inspect" | "init" | "status" | "check" | "vaults" | "log" | "revoke" | "forget" => {
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
    test_identity: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        state_dir: PathBuf::from("/var/lib/vaultorno"),
        socket: PathBuf::from(vaultorno_client::protocol::DEFAULT_SOCKET),
        test_identity: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state-dir" => opts.state_dir = it.next().ok_or("--state-dir needs a folder")?.into(),
            "--socket" => opts.socket = it.next().ok_or("--socket needs a path")?.into(),
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

#[cfg(not(feature = "test_identity"))]
fn serve(_opts: Options) -> ExitCode {
    fail(
        "this build has no identity checks yet (they arrive in milestones 2 and 3), so it refuses to serve. \
         For development, build with `--features test_identity` and pass `--test-identity`.",
    )
}

#[cfg(feature = "test_identity")]
fn serve(opts: Options) -> ExitCode {
    use std::sync::Arc;
    use vaultorno::identity::TestIdentity;
    use vaultorno::server::Server;

    if !opts.test_identity {
        return fail(
            "this development build serves only with `--test-identity`, which trusts unsigned statements",
        );
    }
    eprintln!(
        "vaultorno daemon: WARNING: test identity. Unsigned statements are trusted and no executable is checked. \
         Development only."
    );
    let store = match Store::open(&opts.state_dir) {
        Ok(s) => s,
        Err(e) => return fail(&format!("can't open {}: {e}", opts.state_dir.display())),
    };
    let server = Server::new(store, Box::new(TestIdentity), Arc::new(SystemClock));
    match server.listen(&opts.socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("can't listen on {}: {e}", opts.socket.display())),
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
