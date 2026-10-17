//! A round trip, as an app would do it: through the daemon if
//! `/run/vaultorno.sock` exists, otherwise the `Local` backend
//! (`systemd-creds --user` on Linux, DPAPI on Windows).
//!
//! For the daemon on Linux (Milestone 1 has only the test identity):
//!
//! ```sh
//! cargo build --release -p vaultorno --features test_identity
//! sudo ./target/release/vaultorno daemon --test-identity --state-dir /var/tmp/vaultorno_dev
//! ```
//!
//! then, in another terminal, on either platform:
//!
//! ```sh
//! cargo run --release -p vaultorno_client --example round_trip
//! ```
//!
//! With the daemon, the user's half is in
//! `~/.local/share/vaultorno/example_test/user_half`; with `Local`, the
//! locker is in `local_example/` under the same folder (Windows:
//! `%LocalAppData%\vaultorno`).

use vaultorno_client::time::{Clock, DAY, SystemClock, format_date, format_rfc3339};
use vaultorno_client::{ExposeSecret, StatementFiles, Vault};

fn main() -> Result<(), Box<dyn std::error::Error>> {

    // In Milestone 1 the daemon trusts this unsigned statement. From
    // Milestone 3, it comes from `round_trip_statement.json` next to the
    // executable, signed with the app's key.
    let now = SystemClock.now();
    let statement = serde_json::json!({
        "format": 1,
        "app": "example",
        "binary": "round_trip",
        "sha256": "0".repeat(64),
        "issued": format_rfc3339(now - 60),
        "expires": format_rfc3339(now + 7 * DAY),
        "lockers": { "greeting": { "access": "write", "lifetime_days": 1 } }
    })
    .to_string();

    let vault = Vault::open_with(
        "example",
        Some(StatementFiles {
            statement,
            signature: None,
        }),
    )?;
    println!("opened: {}", vault.describe());
    if vault.refused() > 0 {
        println!(
            "note: {} refused attempts to open this vault since the last open",
            vault.refused()
        );
    }

    let current = vault.get("greeting")?;
    let stored = vault.put(
        "greeting",
        b"hello from the example client",
        current.revision,
    )?;
    let back = vault.get("greeting")?;
    assert_eq!(back.data.expose_secret(), b"hello from the example client");
    println!(
        "round trip ok: revision {:016x}, expires {}",
        stored.revision,
        stored.expires.map(format_date).unwrap_or_default()
    );
    Ok(())
}
