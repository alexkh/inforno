//! Who is asking (design doc §7). Milestone 1 has only [`TestIdentity`];
//! package manifests arrive in Milestone 2 and signed statements in
//! Milestone 3.

use std::collections::BTreeMap;

use vaultorno_client::names::is_valid_name;
use vaultorno_client::protocol::{Grant, Statement};

/// The process at the other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// From `SO_PEERCRED`.
    pub uid: u32,
    pub pid: Option<i32>,
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

/// An identity check.
pub trait Identify: Send + Sync {
    fn identify(
        &self,
        peer: &Peer,
        statement: Option<&str>,
        signature: Option<&str>,
        now: u64,
    ) -> Result<Identity, Refusal>;
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
