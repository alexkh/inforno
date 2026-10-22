//! vaultorno: a locker room for disposable secrets.
//!
//! One executable, run in modes (design doc §3). The daemon is blind: it
//! stores and serves the client's envelopes, sealed again under its master
//! key, and never sees a key to them.

#![forbid(unsafe_code)]

pub mod activation;
pub mod identity;
pub mod init;
pub mod master_key;
pub mod server;
pub mod signing;
pub mod store;
pub mod sweep;

pub use vaultorno_client::time;
