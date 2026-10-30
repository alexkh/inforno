# vaultorno

A locker room for disposable secrets. `DESIGN.md` is the authority;
section numbers (§) refer to it.

Milestones 1 to 4 are done. Milestone 5, Inforno on vaultorno, is under way.

## What's here

| Crate | What |
|---|---|
| `vaultorno_client` | The only API apps use: `Vault::open`, `get`, `put`, `update`, `list`. Encrypts every locker inside the app's process, with a key made of the daemon's half and the user's half (§6), so the daemon only sees ciphertext. Also the `Local` backend, for when no daemon is installed, and `startup`, which an app calls first thing in `main` (§13). |
| `vaultorno` | One executable, run in modes: `daemon`, `inspect`, `sweep`, `init`, `revoke` (§3, §12). Depends on `vaultorno_client` with default features off, so none of the key code is linked into it. |
| `vaultorno_inspect` | Which executable a process runs and which files it maps as code (§7). Depends only on `rustix` and `sha2`. |

`dist/` holds the units, the timer and the `sysusers.d` entry (§14).
`cargo xtask build`, in the workspace, builds the release binaries and
signs their statements (§8).

## Tests

```sh
cargo test --release --workspace --all-features
cargo test --release -p vaultorno
```

As root: `tests/hardening.rs`, and
`cargo test --release -p vaultorno --test end_to_end -- --ignored real_init`.

## Installing on this machine

Until the PKGBUILD (Milestone 6), from the workspace root:

```sh
cargo build --release -p vaultorno
sudo install -m 0755 target/release/vaultorno /usr/bin/vaultorno
sudo install -m 0644 vaultorno/dist/vaultorno.conf /usr/lib/sysusers.d/vaultorno.conf
sudo systemd-sysusers
sudo install -m 0644 vaultorno/dist/*.service vaultorno/dist/*.socket \
    vaultorno/dist/*.timer /etc/systemd/system/
sudo vaultorno init
sudo systemctl daemon-reload
sudo systemctl enable --now vaultorno.socket vaultorno_sweep.timer
```

`vaultorno init` seals a new master key with `systemd-creds`, in the TPM2
when there is one, and refuses to replace an existing one.

Apps always connect to `/run/vaultorno.sock`, so this is the only way to
run Inforno on vaultorno. Sign its build first with `cargo xtask build`
(statements valid 7 days; `--key` and `--days` to change).

## A development daemon

For test clients only: it trusts unsigned statements (vaults
`test_<app>`), binds a socket of its own, and needs an unsealed master key
where systemd would put the sealed one.

```sh
cargo build --release -p vaultorno --features test_identity
D=/tmp/vaultorno_dev
mkdir -p -m 0700 $D/credentials
# 32 random bytes, then the label `unsealed` (§6)
{ head -c 32 /dev/urandom; printf unsealed; } > $D/credentials/master_key
chmod 0600 $D/credentials/master_key
CREDENTIALS_DIRECTORY=$D/credentials target/release/vaultorno daemon \
    --test-identity --in-process-inspector \
    --state-dir $D/state --socket $D/vaultorno.sock
```

- `--in-process-inspector` inspects callers in the daemon itself, running as
  you, instead of through an inspector service. It can therefore inspect
  only your own dumpable processes. A client that calls `startup()` is
  non-dumpable and gets refused.
- Clients connect with `DaemonOptions::socket`. Run them outside cargo,
  e.g. with `env -i`: cargo sets `LD_LIBRARY_PATH`, which the daemon
  refuses (§7.5).
- A release build has no `--test-identity`, `--socket` or
  `--in-process-inspector`, and starts only from its units.

## Not done yet

The admin commands `status`, `check`, `vaults`, `log` and `forget`, and the
log (Milestone 6); Windows (Milestone 7); macOS (Milestone 8);
`Vault::reset`.
