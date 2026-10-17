//! Inforno's API keys, kept in vaultorno's `api_keys` locker (vaultorno
//! design doc §15). The locker holds this module's versioned JSON, which
//! vaultorno_client encrypts inside Inforno's process: the daemon only sees
//! ciphertext. Without the daemon (Windows, or Linux without vaultorno
//! running), the `Local` backend seals it per user with the OS
//! (`systemd-creds --user`, DPAPI).
//!
//! `api_keys` expires 30 days after it is first filled. Adding or changing a
//! key later doesn't extend that; when it lapses, the user enters keys again.
//!
//! Milestone 1: no signed statements yet. If no `<binary>_statement.json` is
//! found next to the executable, Inforno builds an unsigned one at startup.
//! The development daemon (`--test-identity`) trusts it and `Local` never
//! verifies signatures; a Milestone 3 daemon will refuse it, and Inforno then
//! falls back to keys entered for the session only.

use std::collections::BTreeMap;

use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use vaultorno_client::ExposeSecret as _;
use vaultorno_client::time::{Clock, DAY, SystemClock, format_rfc3339};
use vaultorno_client::{StatementFiles, Vault};

pub use vaultorno_client::time::format_date;

/// The app's name in vaultorno.
pub const APP: &str = "inforno";
/// The locker holding the keys.
pub const LOCKER: &str = "api_keys";
/// The OpenRouter key's entry.
pub const OPENROUTER: &str = "openrouter";
/// Days a filled `api_keys` lives (design doc §15).
const LIFETIME_DAYS: u32 = 30;
/// Version of the JSON inside the locker.
const FORMAT: u32 = 1;
/// How often `set_key` retries after another process wrote meanwhile.
const ATTEMPTS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("{0}")]
    Vault(#[from] vaultorno_client::Error),
    #[error("the saved keys are damaged: {0}")]
    Format(String),
}

#[derive(Serialize)]
struct SavedOut<'a> {
    format: u32,
    keys: BTreeMap<&'a str, &'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedIn {
    format: u32,
    keys: BTreeMap<String, String>,
}

/// Opens Inforno's vault: the daemon if it's running, otherwise `Local`.
pub fn open_vault() -> Result<Vault, KeysError> {
    let statement = match vaultorno_client::find_statement(APP, &SystemClock) {
        Ok(found) => found,
        Err(vaultorno_client::Error::NoStatement(_)) => dev_statement(SystemClock.now()),
        Err(e) => return Err(e.into()),
    };
    Ok(Vault::open_with(APP, Some(statement))?)
}

/// All saved keys, and when they expire (none if nothing is saved).
pub fn read_keys(vault: &Vault) -> Result<(BTreeMap<String, SecretString>, Option<u64>), KeysError> {
    let locker = vault.get(LOCKER)?;
    Ok((decode(locker.data.expose_secret())?, locker.expires))
}

/// Saves `provider`'s key, or removes it (`None`), and returns when the saved
/// keys expire. Removing the last key empties the locker. Keys that no
/// longer decode are replaced rather than kept: they're disposable.
pub fn set_key(
    vault: &Vault,
    provider: &str,
    key: Option<&SecretString>,
) -> Result<Option<u64>, KeysError> {
    for _ in 0..ATTEMPTS {
        let current = vault.get(LOCKER)?;
        let mut keys = decode(current.data.expose_secret()).unwrap_or_default();
        match key {
            Some(key) => {
                keys.insert(provider.to_owned(), key.clone());
            }
            None => {
                keys.remove(provider);
            }
        }
        let bytes = encode(&keys);
        match vault.put(LOCKER, &bytes, current.revision) {
            Ok(stored) => return Ok(stored.expires),
            Err(vaultorno_client::Error::Conflict) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(vaultorno_client::Error::Conflict.into())
}

fn encode(keys: &BTreeMap<String, SecretString>) -> Zeroizing<Vec<u8>> {
    if keys.is_empty() {
        return Zeroizing::new(Vec::new());
    }
    let keys = keys
        .iter()
        .map(|(k, v)| {
            let value: &str = v.expose_secret();
            (k.as_str(), value)
        })
        .collect();
    Zeroizing::new(
        serde_json::to_vec(&SavedOut { format: FORMAT, keys })
            .expect("a string map always serializes"),
    )
}

fn decode(bytes: &[u8]) -> Result<BTreeMap<String, SecretString>, KeysError> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let saved: SavedIn =
        serde_json::from_slice(bytes).map_err(|e| KeysError::Format(e.to_string()))?;
    if saved.format != FORMAT {
        return Err(KeysError::Format(format!(
            "format {} isn't supported",
            saved.format
        )));
    }
    // Each String moves, buffer and all, into a SecretString that wipes it.
    Ok(saved
        .keys
        .into_iter()
        .map(|(k, v)| (k, SecretString::from(v)))
        .collect())
}

/// Milestone 1 only: an unsigned statement for this executable, valid for 7
/// days, naming `api_keys` for writing.
fn dev_statement(now: u64) -> StatementFiles {
    let mut lockers = serde_json::Map::new();
    lockers.insert(
        LOCKER.to_owned(),
        serde_json::json!({ "access": "write", "lifetime_days": LIFETIME_DAYS }),
    );
    let statement = serde_json::json!({
        "format": 1,
        "app": APP,
        "binary": binary_name(),
        "sha256": "0".repeat(64),
        "issued": format_rfc3339(now.saturating_sub(60)),
        "expires": format_rfc3339(now + 7 * DAY),
        "lockers": lockers,
    })
    .to_string();
    StatementFiles {
        statement,
        signature: None,
    }
}

/// This executable's name in vaultorno's naming rule (`inforno-gui.exe` →
/// `inforno_gui`), or the app's name.
fn binary_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase().replace('-', "_"))
        })
        .filter(|s| vaultorno_client::names::is_valid_name(s))
        .unwrap_or_else(|| APP.to_owned())
}
