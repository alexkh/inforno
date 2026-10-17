//! Stands in for the `Daemon` backend where there is no daemon yet (Windows:
//! Milestone 7). A `DaemonConn` can't be constructed here, so every
//! `Backend::Daemon` arm in `vault.rs` is unreachable.

#![allow(dead_code)]

use std::convert::Infallible;

use crate::Error;
use crate::protocol::LockerInfo;
use crate::vault::{Locker, Stored};

pub(crate) struct Welcome {
    pub vault: String,
    pub binary: String,
    pub level: String,
    pub sealer: String,
    pub statement_expires: Option<u64>,
    pub refused: u32,
}

pub(crate) struct DaemonConn {
    pub(crate) welcome: Welcome,
    never: Infallible,
}

impl DaemonConn {
    pub(crate) fn get(&self, _locker: &str) -> Result<Locker, Error> {
        match self.never {}
    }

    pub(crate) fn put(&self, _locker: &str, _data: &[u8], _expected: u64) -> Result<Stored, Error> {
        match self.never {}
    }

    pub(crate) fn list(&self) -> Result<Vec<LockerInfo>, Error> {
        match self.never {}
    }
}
