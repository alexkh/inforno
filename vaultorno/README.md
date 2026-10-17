# vaultorno, Milestone 1

A locker room for disposable secrets. See the design document for the why;
this is the first milestone of the how.

## What's here

| Crate | What |
|---|---|
| `vaultorno_client` | The only API apps use: `Vault::open`, `get`, `put`, `update`, `list`. Encrypts every locker inside the app's process with the app key (the daemon's half and the user's half, combined with HKDF), so the daemon only ever sees ciphertext. Also the `Local` backend for when no daemon is installed. |
| `vaultorno` | One executable, run in modes. In this milestone: `vaultorno daemon` (with a test-only identity) and `vaultorno sweep`. Depends on `vaultorno_client` with default features off, so none of the key code is linked into it. |

## Try it

```sh
cargo test --workspace --all-features      # everything, including the end-to-end tests

# a development daemon (it trusts unsigned statements: development only)
cargo run -p vaultorno --features test_identity -- \
    daemon --test-identity --state-dir /tmp/vaultorno_dev/state --socket /tmp/vaultorno_dev/vaultorno.sock

# an app's round trip, in another terminal
cargo run -p vaultorno_client --example round_trip -- /tmp/vaultorno_dev/vaultorno.sock
```

A build without `--features test_identity` refuses to serve: it has no
identity checks yet.

## Milestone 1, done when…

| Criterion | Where |
|---|---|
| Round trip from an example client | `examples/round_trip.rs`; `tests/end_to_end.rs` against the real binary |
| Per-user separation | `users_never_see_each_other`, `a_locker_copied_to_another_user_fails_to_open` |
| Conflicting writers detected | the shared suite's `conflicting_writers_detected` |
| Lockers expire; writes don't extend them | `expiry_set_at_fill_and_not_extended`, `expired_locker_reads_empty`, the sweep test |
| Limits enforced | locker and frame size, 16 lockers per app, 32 connections, 64 vaults |
| No plaintext canary reaches the daemon | `no_plaintext_canary_reaches_the_daemon` checks every frame both ways (base64 fields decoded) and every state file |
| Same tests pass on `Local` | `vaultorno_client::backend_suite!` runs 14 cases on both backends |

## Moving it into the Inforno workspace

Copy both crate folders, add them to the workspace's `members`, and drop this
workspace's `Cargo.toml`. The crypto (XChaCha20-Poly1305, HKDF-SHA256) and
the `Local` backend were written fresh here; swap in `inforno_core::vault`'s
tested code where you prefer it.

## Not in this milestone

Identity checks (package manifests in M2, signed statements in M3), the
inspector, the master key and outer encryption (M4), the log and admin
commands (M6), Windows (M7), macOS (M8). `Vault::reset` and refused-request
back-off come with the identities.

