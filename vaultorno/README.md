# vaultorno

A locker room for disposable secrets. `DESIGN.md` is the authority;
section numbers (§) refer to it.

Milestones 1 to 6 are done (`DESIGN.md` §16).

## What's here

| Crate | What |
|---|---|
| `vaultorno_client` | The only API apps use: `Vault::open`, `get`, `put`, `update`, `list`. Encrypts every locker inside the app's process, with a key made of the daemon's half and the user's half (§6), so the daemon only sees ciphertext. Also the `Local` backend, for when no daemon is installed, and `startup`, which an app calls first thing in `main` (§13). |
| `vaultorno` | One executable, run in modes: `daemon`, `inspect`, `sweep`, and the admin commands `init`, `status`, `check`, `vaults`, `log`, `forget`, `revoke` (§3, §12). Depends on `vaultorno_client` with default features off, so none of the key code is linked into it. |
| `vaultorno_inspect` | Which executable a process runs and which files it maps as code (§7). Depends only on `rustix` and `sha2`. |

`dist/` holds the units, the timer and the `sysusers.d` entry (§14), and
`packaging/` at the workspace root holds the Arch packages.
`cargo xtask build`, in the workspace, builds the release binaries and
signs their statements (§8).

## Tests

```sh
cargo test --release --workspace --all-features
cargo test --release -p vaultorno
```

As root: `tests/hardening.rs`, and
`cargo test --release -p vaultorno --test end_to_end -- --ignored real_init`.

## Installing

### From the packages (Arch)

From the workspace root:

```sh
(cd packaging/vaultorno && makepkg -si)
sudo systemctl enable --now vaultorno.socket vaultorno_sweep.timer
(cd packaging/inforno && makepkg -si)
```

`vaultorno`'s install script runs `vaultorno init` once, and skips it if a
master key exists. `init` seals a new master key with `systemd-creds`, in
the TPM2 when there is one. The package enables no unit. Removing it
leaves `/var/lib/vaultorno` in place, so a reinstall finds the vaults (§14).

`inforno` builds Inforno and autorno from source and installs a package
manifest: level `watertight`, vault `pkg_inforno`. That needs
`ptrace_scope` 1, which is stock Arch's setting. `inforno-bin`, upstream's
signed binaries (`airtight`), will need 2.

### By hand (Debian 13)

Everything goes where the packages put it, under `/usr`. Nothing goes in
`/etc` except the sysctl setting below.

```sh
cargo build --release -p vaultorno -p inforno -p autorno
sudo install -m 0755 target/release/vaultorno /usr/bin/vaultorno
sudo install -m 0644 vaultorno/dist/vaultorno.conf /usr/lib/sysusers.d/vaultorno.conf
sudo systemd-sysusers
sudo install -m 0644 vaultorno/dist/*.service vaultorno/dist/*.socket \
    vaultorno/dist/*.timer /usr/lib/systemd/system/
sudo vaultorno init
sudo systemctl daemon-reload
sudo systemctl enable --now vaultorno.socket vaultorno_sweep.timer

# Inforno, as the `inforno` package installs it
sudo install -m 0755 target/release/inforno target/release/autorno /usr/bin/
sudo install -D -m 0644 packaging/inforno/inforno.json \
    /usr/share/vaultorno/manifests/inforno.json
```

Debian 13 ships `ptrace_scope` 0, which vaultorno can't serve: apps get
`Unavailable` (§9). Raise it to 1, now and at every boot:

```sh
echo 1 | sudo tee /proc/sys/kernel/yama/ptrace_scope
echo 'kernel.yama.ptrace_scope = 1' | sudo tee /etc/sysctl.d/60-ptrace-scope.conf
```

What each setting allows:
- **1:** a program may debug only its own children.
- **2:** only root may attach a debugger.
- **3:** can't be lowered again before a reboot, and nothing here needs it.

`vaultorno status` shows the current setting and what it allows.

### A development build of Inforno

Apps always connect to `/run/vaultorno.sock`. Sign the build first with
`cargo xtask build` (statements valid 7 days; `--key` and `--days` to
change). It opens its own vault, named by the development key, and needs
`ptrace_scope` 2.

## Admin commands

All are modes of the same `vaultorno` (§12). None reads a user's half, so
none can show contents, even as root.

| Command | Root | What it does |
|---|---|---|
| `vaultorno status` | no | Version, constants, reject list, `ptrace_scope` and how to raise it, how the master key is sealed. As root, it also unseals the master key and lists revoked keys and successions. |
| `vaultorno check BINARY` | no | Which statement or manifest a binary would use, its vault, level and lockers, and which check fails. |
| `vaultorno vaults --user U` | yes | A user's vaults and their lockers: size classes, revisions, expiries. |
| `vaultorno log --user U` | yes | A user's log (§11), as printable ASCII only. |
| `vaultorno forget VAULT [--locker L] [--user U]` | yes | Delete one locker, or a whole vault. Best done while the app is closed. |
| `vaultorno revoke KEY` | yes | Refuse statements signed by a key; `--undo` takes it back. |
| `vaultorno sweep` | yes | What the daily timer does: delete expired lockers and stale vaults. |
| `vaultorno init` | yes | Create the state and the sealed master key. Run by the install script. |

Users are given by ID, or by a name in `/etc/passwd`.

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

- Windows (Milestone 7).
- macOS (Milestone 8).
- The `fido2` option (Milestone 9).
- `Vault::reset`.
- The release pipeline for `inforno-bin`: the release key, and statements
  refreshed monthly.
- Inforno's password mode, for machines vaultorno can't serve.
