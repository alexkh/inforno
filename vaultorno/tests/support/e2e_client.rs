//! The end-to-end tests' client (`tests/end_to_end.rs`): one vault
//! operation through the real daemon, in a process of its own, so the tests
//! can run it with an empty environment. Built only with `test_identity`;
//! its statement is unsigned, so only a `--test-identity` daemon accepts it.
//!
//! ```text
//! vaultorno_e2e_client --socket PATH --home DIR --binary NAME
//!     [--locker NAME:read | --locker NAME:write[:DAYS]]... -- OP
//! OP: describe | get LOCKER | put LOCKER HEX EXPECTED
//! ```
//!
//! Prints the result on stdout (`get`: `<revision>:<hex>`, `put`:
//! `stored <revision>`). A refusal prints the error and exits 1; bad usage
//! exits 2.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::slice;

use vaultorno_client::testing::statement;
use vaultorno_client::time::{Clock, SystemClock};
use vaultorno_client::{DaemonOptions, Error, ExposeSecret, StatementFiles, Vault};

/// The app every end-to-end vault belongs to.
const APP: &str = "e2e";

fn main() -> ExitCode {
    match run() {
        Ok(Ok(out)) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Ok(Err(err)) => {
            println!("{err:?}");
            ExitCode::from(1)
        }
        Err(usage) => {
            eprintln!("vaultorno_e2e_client: {usage}");
            ExitCode::from(2)
        }
    }
}

/// `Err` is bad usage; `Ok(Err)` is the vault refusing.
fn run() -> Result<Result<String, Error>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut socket, mut home, mut binary) = (None, None, None);
    let mut lockers = Vec::new();
    let mut it = args.iter();
    let op: Vec<&str> = loop {
        let Some(arg) = it.next() else {
            return Err("missing `-- OP`".into());
        };
        match arg.as_str() {
            "--socket" => socket = Some(PathBuf::from(value(&mut it, arg)?)),
            "--home" => home = Some(PathBuf::from(value(&mut it, arg)?)),
            "--binary" => binary = Some(value(&mut it, arg)?),
            "--locker" => lockers.push(parse_locker(value(&mut it, arg)?)?),
            "--" => break it.map(String::as_str).collect(),
            other => return Err(format!("unknown option {other:?}")),
        }
    };
    let (Some(socket), Some(home), Some(binary)) = (socket, home, binary) else {
        return Err("--socket, --home and --binary are required".into());
    };

    let text = statement(APP, binary, SystemClock.now() - 60, &lockers);
    let opened = Vault::connect(DaemonOptions {
        socket,
        home: Some(home),
        statement: Some(StatementFiles {
            statement: text,
            signature: None,
        }),
        ..DaemonOptions::new(APP)
    });
    let vault = match opened {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    Ok(match op.as_slice() {
        ["describe"] => Ok(vault.describe()),
        ["get", locker] => vault
            .get(locker)
            .map(|l| format!("{}:{}", l.revision, hex(l.data.expose_secret()))),
        ["put", locker, data, expected] => {
            let data = unhex(data)?;
            let expected = expected
                .parse::<u64>()
                .map_err(|_| format!("bad revision {expected:?}"))?;
            vault
                .put(locker, &data, expected)
                .map(|s| format!("stored {}", s.revision))
        }
        other => return Err(format!("unknown operation {other:?}")),
    })
}

fn value<'a>(it: &mut slice::Iter<'a, String>, flag: &str) -> Result<&'a str, String> {
    it.next()
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// `NAME:read` or `NAME:write[:DAYS]`.
fn parse_locker(spec: &str) -> Result<(&str, &str, Option<u32>), String> {
    let bad = || format!("--locker {spec:?} isn't NAME:ACCESS[:DAYS]");
    let mut parts = spec.split(':');
    let (Some(name), Some(access)) = (parts.next(), parts.next()) else {
        return Err(bad());
    };
    let days = parts
        .next()
        .map(|d| d.parse::<u32>().map_err(|_| bad()))
        .transpose()?;
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok((name, access, days))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err(format!("{s:?} isn't hex"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("{s:?} isn't hex")))
        .collect()
}
