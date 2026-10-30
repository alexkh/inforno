//! Inforno's API keys, kept in vaultorno's `api_keys` locker (vaultorno
//! design doc §15). The locker holds this module's versioned JSON, which
//! vaultorno_client encrypts inside Inforno's process: the daemon only sees
//! ciphertext. Without the daemon (Windows, or Linux without vaultorno
//! running), the `Local` backend seals it per user with the OS
//! (`systemd-creds --user`, DPAPI).
//!
//! Each key carries the hosts it may be sent to, so nothing outside the
//! locker (a config file, a variable) can send it elsewhere (§13). Whatever
//! sends a key checks the URL with [`check_host`] first.
//!
//! `api_keys` expires 90 days after it is filled. Adding or changing a key
//! keeps that expiry. Renewing ([`renew_key`]) empties the locker and fills
//! it with a key the user has just entered, which starts a new 90 days;
//! nothing carries old contents into a new period (§13).
//!
//! Inforno opens its vault once, at startup, and keeps it ([`KeyStore`]):
//! each open makes the daemon hash the executable, and each refused open is
//! counted against the vault and reported to the next successful one.
//!
//! The statement comes from `<binary>_statement.json` and its `.sig`, found
//! by vaultorno_client next to the executable or in the statement folders,
//! along with `inforno_succession.json` after a key rotation. If there is
//! none, Inforno builds an unsigned one at startup: the development daemon
//! (`--test-identity`) trusts it and `Local` never verifies signatures, but
//! a release daemon refuses it, and Inforno then falls back to keys entered
//! for the session only.

use std::collections::BTreeMap;
use std::sync::Mutex;

use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use vaultorno_client::ExposeSecret as _;
use vaultorno_client::startup::Startup;
use vaultorno_client::time::{Clock, DAY, SystemClock, format_rfc3339};
use vaultorno_client::{StatementFiles, Vault};

pub use vaultorno_client::time::format_date;

/// The app's name in vaultorno.
pub const APP: &str = "inforno";
/// The locker holding the keys.
pub const LOCKER: &str = "api_keys";
/// The OpenRouter key's entry.
pub const OPENROUTER: &str = "openrouter";
/// The API every saved OpenRouter key is sent to; set explicitly on the
/// client, never left to a library's default.
pub const OPENROUTER_API: &str = "https://openrouter.ai/api/v1";
/// Days a filled `api_keys` lives (design doc §15): the most vaultorno
/// allows. The key itself should expire at its provider no later.
pub const LIFETIME_DAYS: u32 = 90;
/// Inforno asks for keys again once they expire within this many days.
pub const RENEW_DAYS: u64 = 3;
/// It warns when its statement expires within this many days (§13, §15).
pub const STATEMENT_WARN_DAYS: u64 = 14;
/// Version of the JSON inside the locker.
const FORMAT: u32 = 2;
/// How often a write retries after another process wrote meanwhile.
const ATTEMPTS: usize = 8;

/// Variables Inforno removes at startup, besides vaultorno_client's
/// network ones: wgpu's switches that make it ask the Vulkan loader for
/// validation layers, which the loader also looks for in the home folder.
const REMOVE_ENV: &[&str] = &["WGPU_VALIDATION", "WGPU_DEBUG", "WGPU_GPU_BASED_VALIDATION"];

/// Inforno's startup steps (vaultorno design doc §13). Call it first thing
/// in `main`, before the Tokio runtime or anything else starts a thread.
pub fn startup() -> Result<(), String> {
    vaultorno_client::startup::startup(&Startup {
        non_dumpable: true,
        pin_vulkan: true,
        remove_network_env: true,
        remove_env: REMOVE_ENV,
    })
    .map_err(|e| e.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("{0}")]
    Vault(#[from] vaultorno_client::Error),
    #[error("the saved keys are damaged: {0}")]
    Format(String),
    /// Opening the vault was refused earlier in this run; Inforno doesn't
    /// try again until it restarts, since every refusal is counted.
    #[error("{0}")]
    Unavailable(String),
}

impl KeysError {
    /// Worth reconnecting for: the daemon went away or was busy.
    fn is_transient(&self) -> bool {
        matches!(
            self,
            KeysError::Vault(
                vaultorno_client::Error::Io(_)
                    | vaultorno_client::Error::Protocol(_)
                    | vaultorno_client::Error::Busy(_)
            )
        )
    }
}

/// One saved key and the hosts it may be sent to.
#[derive(Clone)]
pub struct SavedKey {
    pub key: SecretString,
    pub hosts: Vec<String>,
}

impl SavedKey {
    /// `key` for `provider`, allowed to that provider's hosts.
    pub fn new(provider: &str, key: SecretString) -> SavedKey {
        SavedKey {
            key,
            hosts: default_hosts(provider),
        }
    }
}

/// The hosts a provider's key goes to; none for a provider Inforno doesn't
/// know, so its key goes nowhere.
pub fn default_hosts(provider: &str) -> Vec<String> {
    match provider {
        OPENROUTER => vec!["openrouter.ai".to_owned()],
        _ => Vec::new(),
    }
}

/// A host a key may go to: a lowercase DNS name, without port or wildcard.
pub fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Whether a key allowed to `hosts` may be sent to `url`: only over HTTPS,
/// on the default port, to exactly one of its hosts. A URL with anything
/// unusual in its authority (user info, a port, an IP literal, uppercase)
/// is refused rather than interpreted.
pub fn check_host(hosts: &[String], url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("{url} isn't an https:// URL; no key is sent to it"))?;
    let authority = rest.split(&['/', '?', '#'][..]).next().unwrap_or("");
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    if !is_valid_host(host) {
        return Err(format!("can't tell which host {url} is; no key is sent to it"));
    }
    if hosts.iter().any(|h| h == host) {
        Ok(())
    } else if hosts.is_empty() {
        Err("this key isn't allowed to any host".to_owned())
    } else {
        Err(format!(
            "this key may only be sent to {}, not {host}",
            hosts.join(", ")
        ))
    }
}

/// Whether `expires` (Unix seconds) falls within `days` from now.
pub fn expires_within(expires: Option<u64>, days: u64) -> bool {
    expires.is_some_and(|t| t <= SystemClock.now().saturating_add(days * DAY))
}

/// What the vault said when it was opened, for the startup report (§15).
pub struct VaultStatus {
    /// Backend and level, from `describe()`.
    pub description: String,
    /// Refused opens since the last successful one.
    pub refused: u32,
    /// When this binary's statement expires, if the daemon checked one.
    pub statement_expires: Option<u64>,
}

pub fn status(vault: &Vault) -> VaultStatus {
    VaultStatus {
        description: vault.describe(),
        refused: vault.refused(),
        statement_expires: vault.statement_expires(),
    }
}

enum Slot {
    Open(Vault),
    /// Not open; the next use tries.
    Closed,
    /// Refused in this run; not tried again until Inforno restarts.
    Refused(String),
}

/// Inforno's vault, opened once and kept (see the module docs).
pub struct KeyStore {
    slot: Mutex<Slot>,
}

impl KeyStore {
    /// Opens the vault now. The store is usable whatever the result: after a
    /// transient failure, the next use tries again.
    pub fn open() -> (KeyStore, Result<(), KeysError>) {
        let store = KeyStore {
            slot: Mutex::new(Slot::Closed),
        };
        let opened = store.with_vault(|_| Ok(()));
        (store, opened)
    }

    /// Runs `f` on the vault, reconnecting once if the connection was lost.
    pub fn with_vault<T>(
        &self,
        mut f: impl FnMut(&Vault) -> Result<T, KeysError>,
    ) -> Result<T, KeysError> {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let first = match &*slot {
            Slot::Open(vault) => Some(f(vault)),
            Slot::Refused(why) => return Err(KeysError::Unavailable(why.clone())),
            Slot::Closed => None,
        };
        match first {
            Some(Err(e)) if e.is_transient() => *slot = Slot::Closed,
            Some(result) => return result,
            None => {}
        }
        match open_vault() {
            Ok(vault) => {
                let result = f(&vault);
                *slot = Slot::Open(vault);
                result
            }
            Err(e) => {
                if !e.is_transient() {
                    *slot = Slot::Refused(e.to_string());
                }
                Err(e)
            }
        }
    }
}

#[derive(Deserialize)]
struct Version {
    format: u32,
}

#[derive(Serialize)]
struct SavedOut<'a> {
    format: u32,
    keys: BTreeMap<&'a str, KeyOut<'a>>,
}

#[derive(Serialize)]
struct KeyOut<'a> {
    key: &'a str,
    hosts: &'a [String],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedInV1 {
    #[allow(dead_code)]
    format: u32,
    keys: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedInV2 {
    #[allow(dead_code)]
    format: u32,
    keys: BTreeMap<String, KeyIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyIn {
    key: String,
    hosts: Vec<String>,
}

/// Opens Inforno's vault: the daemon if it's running, otherwise `Local`.
/// With a statement on disk, `Vault::open` finds it again, together with the
/// succession beside it after a key rotation (design doc §8); without one,
/// Inforno sends its unsigned development statement.
pub fn open_vault() -> Result<Vault, KeysError> {
    match vaultorno_client::find_statement(APP, &SystemClock) {
        Ok(_) => Ok(Vault::open(APP)?),
        Err(vaultorno_client::Error::NoStatement(_)) => Ok(Vault::open_with(
            APP,
            Some(dev_statement(SystemClock.now())),
        )?),
        Err(e) => Err(e.into()),
    }
}

/// All saved keys, and when they expire (none if nothing is saved).
pub fn read_keys(vault: &Vault) -> Result<(BTreeMap<String, SavedKey>, Option<u64>), KeysError> {
    let locker = vault.get(LOCKER)?;
    Ok((decode(locker.data.expose_secret())?, locker.expires))
}

/// Saves `provider`'s key, or removes it (`None`), and returns when the saved
/// keys expire, which this never changes. Removing the last key empties the
/// locker. Keys that no longer decode are replaced rather than kept: they're
/// disposable.
pub fn set_key(
    vault: &Vault,
    provider: &str,
    key: Option<&SavedKey>,
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

/// Replaces every saved key with `key` alone, in a locker emptied and filled
/// again, so it lives a full 90 days. Only for a key the user has just
/// entered (design doc §13).
pub fn renew_key(vault: &Vault, provider: &str, key: &SavedKey) -> Result<Option<u64>, KeysError> {
    let mut keys = BTreeMap::new();
    keys.insert(provider.to_owned(), key.clone());
    let bytes = encode(&keys);
    for _ in 0..ATTEMPTS {
        let revision = vault
            .list()?
            .into_iter()
            .find(|l| l.name == LOCKER)
            .map_or(0, |l| l.revision);
        if revision != 0 {
            match vault.put(LOCKER, &[], revision) {
                Ok(_) => {}
                Err(vaultorno_client::Error::Conflict) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        match vault.put(LOCKER, &bytes, 0) {
            Ok(stored) => return Ok(stored.expires),
            Err(vaultorno_client::Error::Conflict) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(vaultorno_client::Error::Conflict.into())
}

fn encode(keys: &BTreeMap<String, SavedKey>) -> Zeroizing<Vec<u8>> {
    if keys.is_empty() {
        return Zeroizing::new(Vec::new());
    }
    let keys = keys
        .iter()
        .map(|(provider, saved)| {
            let key: &str = saved.key.expose_secret();
            (
                provider.as_str(),
                KeyOut {
                    key,
                    hosts: &saved.hosts,
                },
            )
        })
        .collect();
    Zeroizing::new(
        serde_json::to_vec(&SavedOut { format: FORMAT, keys })
            .expect("strings always serialize"),
    )
}

fn decode(bytes: &[u8]) -> Result<BTreeMap<String, SavedKey>, KeysError> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let bad = |e: serde_json::Error| KeysError::Format(e.to_string());
    let version: Version = serde_json::from_slice(bytes).map_err(bad)?;
    // Each String moves, buffer and all, into a SecretString that wipes it.
    let keys: BTreeMap<String, SavedKey> = match version.format {
        1 => serde_json::from_slice::<SavedInV1>(bytes)
            .map_err(bad)?
            .keys
            .into_iter()
            .map(|(provider, key)| {
                let hosts = default_hosts(&provider);
                (
                    provider,
                    SavedKey {
                        key: SecretString::from(key),
                        hosts,
                    },
                )
            })
            .collect(),
        2 => serde_json::from_slice::<SavedInV2>(bytes)
            .map_err(bad)?
            .keys
            .into_iter()
            .map(|(provider, k)| {
                (
                    provider,
                    SavedKey {
                        key: SecretString::from(k.key),
                        hosts: k.hosts,
                    },
                )
            })
            .collect(),
        other => {
            return Err(KeysError::Format(format!(
                "format {other} isn't supported"
            )));
        }
    };
    for (provider, saved) in &keys {
        if let Some(host) = saved.hosts.iter().find(|h| !is_valid_host(h)) {
            return Err(KeysError::Format(format!(
                "{provider}'s key names an invalid host {host:?}"
            )));
        }
    }
    Ok(keys)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_go_only_to_their_hosts() {
        let hosts = default_hosts(OPENROUTER);
        assert!(check_host(&hosts, OPENROUTER_API).is_ok());
        assert!(check_host(&hosts, "https://openrouter.ai:443/api/v1").is_ok());
        for bad in [
            "http://openrouter.ai/api/v1",
            "https://evil.example/api/v1",
            "https://openrouter.ai.evil.example/",
            "https://user@openrouter.ai/",
            "https://openrouter.ai:8443/",
            "https://OPENROUTER.AI/",
            "https://[::1]/",
            "openrouter.ai",
        ] {
            assert!(check_host(&hosts, bad).is_err(), "{bad}");
        }
        assert!(check_host(&[], OPENROUTER_API).is_err());
        assert!(default_hosts("someone_else").is_empty());
    }

    #[test]
    fn formats_read_and_write() {
        let keys = decode(br#"{"format":1,"keys":{"openrouter":"sk-1"}}"#).unwrap();
        assert_eq!(keys["openrouter"].key.expose_secret(), "sk-1");
        assert_eq!(keys["openrouter"].hosts, ["openrouter.ai"]);
        let again = decode(&encode(&keys)).unwrap();
        assert_eq!(again["openrouter"].key.expose_secret(), "sk-1");
        assert_eq!(again["openrouter"].hosts, ["openrouter.ai"]);
        assert!(encode(&BTreeMap::new()).is_empty());
        assert!(decode(br#"{"format":2,"keys":{"x":{"key":"k","hosts":["Bad Host"]}}}"#).is_err());
        assert!(decode(br#"{"format":2,"keys":{"x":{"key":"k"}}}"#).is_err());
        assert!(decode(br#"{"format":2,"keys":{},"extra":1}"#).is_err());
        assert!(decode(br#"{"format":3,"keys":{}}"#).is_err());
    }

    /// The expiry rules the key manager relies on, on the `Local` backend
    /// with a clock the test moves (vaultorno design doc §6, §15).
    #[test]
    fn keys_expire_and_only_renewal_extends_them() {
        use std::sync::Arc;
        use vaultorno_client::time::FakeClock;
        use vaultorno_client::{FakeSealer, LocalOptions};

        const NOW: u64 = 1_792_108_800; // 2026-10-16
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock::new(NOW));
        let vault = Vault::local(LocalOptions {
            base: Some(dir.path().join("vaultorno")),
            sealer: Arc::new(FakeSealer::new()),
            statement: Some(dev_statement(NOW).statement),
            clock: clock.clone(),
            ..LocalOptions::new(APP)
        })
        .unwrap();
        let key = |k: &str| SavedKey::new(OPENROUTER, SecretString::from(k.to_owned()));

        // The first key fills the locker: 90 days from now.
        let first = set_key(&vault, OPENROUTER, Some(&key("sk-1"))).unwrap();
        assert_eq!(first, Some(NOW + 90 * DAY));

        // Changing it, or adding another, keeps that expiry.
        clock.advance(10 * DAY);
        assert_eq!(set_key(&vault, OPENROUTER, Some(&key("sk-2"))).unwrap(), first);
        assert_eq!(set_key(&vault, "other", Some(&key("sk-x"))).unwrap(), first);
        let (keys, expires) = read_keys(&vault).unwrap();
        assert_eq!(keys[OPENROUTER].key.expose_secret(), "sk-2");
        assert_eq!(keys.len(), 2);
        assert_eq!(expires, first);

        // Renewing starts a new 30 days, with only the key just entered.
        clock.advance(18 * DAY);
        let renewed = renew_key(&vault, OPENROUTER, &key("sk-3")).unwrap();
        assert_eq!(renewed, Some(NOW + 118 * DAY));
        let (keys, _) = read_keys(&vault).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[OPENROUTER].key.expose_secret(), "sk-3");
        assert_eq!(keys[OPENROUTER].hosts, ["openrouter.ai"]);

        // Still there a second before it expires; gone once it has, which is
        // when Inforno asks for keys again.
        clock.set(NOW + 118 * DAY - 1);
        assert_eq!(read_keys(&vault).unwrap().0.len(), 1);
        clock.set(NOW + 118 * DAY);
        let (keys, expires) = read_keys(&vault).unwrap();
        assert!(keys.is_empty());
        assert_eq!(expires, None);

        // Removing the last key empties the locker.
        set_key(&vault, OPENROUTER, Some(&key("sk-4"))).unwrap();
        assert_eq!(set_key(&vault, OPENROUTER, None).unwrap(), None);
        assert!(read_keys(&vault).unwrap().0.is_empty());
    }
}
