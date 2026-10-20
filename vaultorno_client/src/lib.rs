//! The client side of vaultorno: the only API apps use.
//!
//! The client encrypts every locker inside the app's process before it is
//! stored, with a key made of two halves: one from the daemon, one from a
//! private file in the user's home folder. The daemon only ever sees
//! ciphertext (design doc §2, §6).
//!
//! Without the default `seal` feature, this crate is just the protocol and
//! statement types shared with the daemon.

#![deny(unsafe_code)]

pub mod names;
pub mod protocol;
pub mod time;

#[cfg(all(feature = "seal", unix))]
mod daemon_backend;
// Where there is no daemon yet (Windows: Milestone 7), a stand-in that can't
// be constructed, so `Vault` compiles unchanged.
#[cfg(all(feature = "seal", not(unix)))]
#[path = "daemon_stub.rs"]
mod daemon_backend;
#[cfg(feature = "seal")]
mod error;
#[cfg(feature = "seal")]
mod files;
#[cfg(feature = "seal")]
pub mod local;
#[cfg(feature = "seal")]
pub mod seal;
#[cfg(all(feature = "seal", unix))]
mod user_half;
#[cfg(feature = "seal")]
mod vault;

#[cfg(feature = "testing")]
pub mod testing;

pub use protocol::LockerInfo;

#[cfg(feature = "seal")]
pub use error::Error;
#[cfg(all(feature = "seal", windows))]
pub use local::Dpapi;
#[cfg(all(feature = "seal", target_os = "linux"))]
pub use local::SystemdCredsUser;
#[cfg(feature = "seal")]
pub use local::{FakeSealer, OsSealer, default_sealer};
/// Re-exported so apps can read `Locker::data` without depending on
/// `secrecy` themselves.
#[cfg(feature = "seal")]
pub use secrecy::{ExposeSecret, SecretBox};
#[cfg(all(feature = "seal", unix))]
pub use vault::DaemonOptions;
#[cfg(feature = "seal")]
pub use vault::{
    LocalOptions, Locker, StatementFiles, Stored, SuccessionFiles, Vault, find_statement,
    find_succession, user_data_dir,
};
