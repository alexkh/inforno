//! vaultorno: a locker room for disposable secrets.
//!
//! One executable, run in modes (design doc §3). Milestone 1 has the daemon
//! (`vaultorno daemon`, with a test-only identity) and the sweep
//! (`vaultorno sweep`). The daemon is blind: it stores and serves the
//! client's envelopes and never sees a key to them.

#![forbid(unsafe_code)]

pub mod identity;
pub mod server;
pub mod store;
pub mod sweep;

pub use vaultorno_client::time;
