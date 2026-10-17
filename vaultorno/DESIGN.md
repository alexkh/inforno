# Vaultorno: a locker room for disposable secrets

> **Vaultorno is a locker room for disposable secrets.** Every app gets a
> row of lockers for each user. A locker holds one blob of bytes, at most
> 256 KiB, for at most 90 days, and an app has at most 16 lockers. At the
> door, vaultorno checks who is asking (which user, which genuine binary) and
> opens only the lockers that binary's signed statement lists, for reading or
> for writing. **It never holds the key to what is inside:** each app's key
> is made of two halves, one kept by vaultorno and one kept in the user's
> home folder, and only the app's own process ever combines them. Vaultorno
> only ever sees ciphertext. It never parses, indexes, logs or acts on it.
> Everything else is the app's business.

> **Only disposable data belongs here.** Lockers expire, are bound to one
> machine, and have no backup, export or recovery. Store what you could
> re-issue (API keys, tokens, a cached unlock for another store), never
> anything you would mind losing (§1).

Status: Milestone 1 is implemented (the client, a daemon skeleton with a
test-only identity, and the `Local` backend); everything else is design. The
first user is Inforno (`inforno`, `autorno`). This revision:

- makes the daemon blind with a split key (§2, §6);
- makes every locker expire, and limits vaultorno to disposable data (§1, §6);
- replaces the root-owned per-app policy with statements each app signs
  itself, which also expire (§5, §8);
- removes the configuration file entirely: behaviour is built in, and
  everything vaultorno keeps is visible state in the `vaultorno` user's home
  (§5, §6);
- puts the daemon, the inspector and the admin commands in one executable
  (§3, §14);
- uses JSON only where programs exchange structured data, and simpler
  formats everywhere else (§3);
- names every file in lowercase with underscores (§3);
- moves Realm configs out of vaultorno, into a proposal for a separate store
  (§15).

Pieces:

- **`vaultorno`**: one executable. Run by systemd as the **daemon**
  (`vaultorno daemon`) and the identity **inspector** (`vaultorno inspect`),
  and by people as the admin command (§12, §14).
- **`vaultorno_client`**: the only API apps use. It encrypts and decrypts
  lockers itself, with a key the daemon only half knows (§6). It talks to
  the daemon when it's installed, and otherwise falls back to per-user OS
  sealing (no per-app protection) or, on macOS, the Keychain.

---

## 1. What vaultorno is, and is not

### For disposable data only

The goal is to protect sensitive data **until it's irrelevant**, not to keep
it. Data belongs in a locker only if losing it costs no more than re-issuing
it.

| Belongs in a locker | Doesn't |
|---|---|
| API keys and access tokens the user can re-issue | configs, approvals, documents |
| session tokens | anything needed after reinstalling or losing the machine |
| a cached unlock for another store, whose lasting unlock lives elsewhere (e.g. Inforno's realm store, §15) | recovery material (recovery phrases, backup keys) |

By design, therefore:

- **Every locker expires,** at most 90 days after it was filled (§6).
  Rewriting it doesn't extend that.
- **Lockers are bound to one machine** and have no backup, export, sync or
  recovery. Backups and snapshots should leave vaultorno's state out
  entirely (§14).
- **Expiry at the source matters more than expiry here.** Deleting a local
  copy doesn't make a stolen copy irrelevant; a key that expires at its
  provider does. Apps should make the two match (§13).

### It is

- **Per user, per app.** A **vault** is one app's lockers for one user.
  Users never see each other's vaults, and apps never see each other's.
- **Blind.** Lockers reach the daemon already encrypted by the app's client.
  The key is derived from two halves: the daemon holds one, the user's home
  folder the other (§6). The daemon stores and serves ciphertext and can't
  derive the key.
- **Short-lived.** Lockers expire; so do the statements that grant access
  (§5).
- **Declared by the app.** Each binary carries a statement, signed by the
  app's own key, listing the lockers it may read or write (§5).
- **Without configuration.** There is no config file. Behaviour is built
  in, access comes from apps' statements or their packages' manifests, and
  everything vaultorno keeps is state in the `vaultorno` user's home,
  `/var/lib/vaultorno`, shown by `vaultorno status` and `vaultorno vaults`
  (§6, §12).
- **One executable, plain files.** Daemon, inspector and admin command are
  the same file run in different modes. JSON is used only where programs
  exchange structured data; key lists are in OpenSSH's own format, and
  everything else is fixed binary layouts or plain text (§3).
- **Opaque.** A locker is bytes. Its name, size class, revision and expiry
  are all the daemon knows about it.
- **Small and bounded.** At most 16 lockers per app and 256 KiB per locker;
  messages are at most 512 KiB (§10).
- **Four requests:** `Hello`, `List`, `Get`, `Put`. Writes are
  compare-and-swap (§9).
- **Fail closed.** No valid statement or manifest, no access.

### It is not

- a store for anything that must last: no backup, export, sync or recovery;
- a key or password manager: no entries, kinds or metadata;
- a policy engine: no roles, patterns, delegation or approvals (Appendix A);
- a crypto service: no signing or HMAC (Appendix A). Being blind, it never
  holds a key it could use for one;
- a file store or a sync service; it has no network access at all.

---

## 2. Threat model

### Protects against

- Other programs running as the user (generic malware, scripts looking for
  API keys) reading or changing an app's lockers.
- A binary of an app doing more than its statement allows, e.g. autorno
  rewriting API keys.
- **Old binaries, after a while.** Statements expire (§5), so an old build
  with a since-fixed bug stops opening the vault once its statement runs
  out.
- **Vaultorno itself.** A compromised daemon, anyone who controls the
  `vaultorno` system user, or a copy of its state together with its master
  key, has at most the daemon's half of each key and can't decrypt anything
  (§6).
- **Data outliving its use.** Lockers expire and are deleted (§6).
- Leaks of the stored files (backups, git, stolen disks): a copy of the home
  folder holds only the user's half; the daemon's half and the lockers are
  sealed to the machine, with TPM2 when available.
- Other users on the machine.

### Does not protect against

- Root, or anyone who can change the unit files, the package manifests or
  the installed binaries (including the client library inside apps).
- A compromised genuine binary: it gets exactly what its statement allows.
- **A still-valid old statement.** Until it expires, an old build keeps its
  access. Keep statement lifetimes short (§8).
- **A stolen signing key.** Whoever has it can sign fresh statements for
  that app's vault until root revokes the key with `vaultorno revoke` (§8).
  Short statement lifetimes don't help here.
- What an app does with contents after `Get` (e.g. autorno placing a token
  in a Realm).
- Anything that steers the genuine app beyond what the daemon checks (§7):
  config files, arguments, environment variables not on the reject list, and
  code the app loads from user-writable places. Apps must handle those
  (§13).
- Malware written for one setup that wins a race, e.g. replacing a freshly
  built binary before the developer signs it (§8).
- On Windows, programs running as the user reading or changing the app's
  memory (§7).
- **Copies made before expiry.** Snapshots, backups or a stolen disk taken
  while a locker was live still hold it, under both encryption layers.
  Expiry bounds how long data sits there to be stolen; it doesn't erase
  earlier copies. That is acceptable only because the data must be
  irrelevant by then (§1).
- **A compromised daemon working together with a program running as the
  user.** The daemon still decides who gets its half and the ciphertext, and
  any process of the user can read the user's half (§6). Blind means the
  daemon alone learns nothing, not that a compromised daemon is harmless. A
  later `fido2` option narrows this to programs that also get the user to
  touch a security key (§18).
- **A compromised daemon withholding or rolling back lockers.** It can refuse
  service, serve a wrong half (which makes lockers unreadable, not readable)
  or serve an older ciphertext of the same locker, which still decrypts. It
  can't forge contents or swap them between lockers (§6).
- **Metadata.** The daemon sees locker names, size classes, revisions,
  expiries, and which binary reads or writes what, and when.
- **The inspector below `hermetic`.** It holds `CAP_SYS_PTRACE`, which could
  read an app's memory while the key or plaintext is there. Only
  `ptrace_scope = 3` removes that (§4).

### The daemon is blind

Each vault's key, the **app key**, is split into two halves that live on
opposite sides of a boundary the OS already enforces:

- **The daemon's half:** 32 random bytes per user and app, sealed under the
  daemon's master key. The daemon sends it only to binaries that pass the
  identity check (§7).
- **The user's half:** 32 random bytes per app in a private file in the
  user's home folder (§6). Anything running as the user can read it; the
  `vaultorno` system user can't, because the file is private to the user and
  the daemon runs with `ProtectHome=yes`.

The client combines the two halves into the app key inside the app's
process (§6). It encrypts each locker before `Put` and decrypts it after
`Get`, so the daemon only ever stores and serves ciphertext. Apps don't
notice: `get` and `put` still return and take plaintext.

Each half is guarded by a different gate. The daemon's identity checks keep
other programs running as the user away from its half; file permissions keep
the daemon away from the user's half. Neither half alone gives anything.

An earlier version of this design rejected blindness for two reasons. The
split answers both:

- **"The app would need a safe place for the key."** Neither half needs to
  be safe from everyone. The user's half only needs to be out of the
  daemon's reach, which file permissions provide; the daemon's half only
  needs to be out of reach of other programs running as the user, which the
  identity checks provide.
- **"It wouldn't stop the parties who could abuse the daemon's view."** Root
  remains out of scope. The inspector's `CAP_SYS_PTRACE` remains a way into
  app memory while `ptrace_scope` is below 3, which is why the full guarantee
  holds only at `hermetic` and why `describe()` reports it (§9).

What each part of vaultorno can see:

| Part | Sees | Can it read contents? |
|---|---|---|
| The daemon, the `vaultorno` user, its state and master key | its halves, ciphertext, metadata | No, at every level |
| The inspector | which executable a process runs; its capability *could* read app memory | No at `hermetic`; below it, only if compromised |
| The admin commands | names, size classes, revisions, expiries | No |
| Root | everything | Yes; out of scope |

Defence in depth around that:

- **Privilege separation (§14).** The process that holds the lockers and
  talks to apps (the daemon) has no capabilities. A separate process (the
  inspector) holds `CAP_SYS_PTRACE` and only answers "which executable is
  this process running?". A bug in the request handling can hand ciphertext
  or the daemon's half to the wrong process, but can't expose contents or
  other processes' memory.
- **No plaintext in the daemon.** It removes only its own outer layer, which
  yields the client's ciphertext. Core dumps are off anyway.
- **Identity bound into both layers.** Each layer's key and associated data
  include the user, vault and locker name, so a mix-up bug fails to decrypt
  instead of handing one user's locker to another (§6).
- **The daemon can't choose the key.** Serving a different half only yields
  a key nobody can use, so a compromised daemon can make lockers unreadable
  but never readable to itself. No pinning or trust-on-first-use is needed.
- **The client always encrypts.** Nothing the daemon sends can turn that off.

---

## 3. One executable, formats and names

### One executable, three crates

The executable is one file, `/usr/bin/vaultorno`. What it does depends only
on its first argument; what it may do depends only on how it was started:

| Mode | Started by | Runs as | Privileges (from the unit, §14) |
|---|---|---|---|
| `vaultorno daemon` | `vaultorno.service` | `vaultorno` | none |
| `vaultorno inspect` | `vaultorno_inspect.service` | `vaultorno_inspect` | `CAP_SYS_PTRACE` |
| `vaultorno sweep` | `vaultorno_sweep.timer`, or root | `vaultorno` | none |
| `vaultorno init`, `status`, `check`, `vaults`, `log`, `revoke`, `forget` | people (§12) | the person running it | their own |

Rules:

- **Privileges come only from the units.** No file capabilities (`setcap`),
  no setuid. Then running the file by hand is harmless in any mode: a user's
  `vaultorno inspect` has no capability, and a user's `vaultorno daemon` has
  no master key and can't take over `/run/vaultorno.sock`, which systemd
  owns.
- **The mode is chosen first,** from the first argument only, never from an
  environment variable, before any other code runs.
- **Each side checks its peer** on the inspector socket: the inspector
  accepts only the `vaultorno` user, and the daemon expects the
  `vaultorno_inspect` user. With the same code in both processes, user IDs
  and socket ownership are what tell them apart from an impostor.
- **The inspector stays small and separate in the source,** even though it
  ships in the same file: its own crate, depending only on `rustix` and
  `sha2`, so a reviewer can see everything that mode can reach.

| Crate | Kind | Depends on | Used by |
|---|---|---|---|
| `vaultorno` | the executable | `vaultorno_client` (protocol and statement types, without the sealing code), `vaultorno_inspect`, `serde_json`, minimal crypto, SSHSIG and OpenSSH key parsing, OS bindings | the system services and admins |
| `vaultorno_inspect` | library | `rustix`, `sha2` | `vaultorno` |
| `vaultorno_client` | library | `serde`, `serde_json`, `secrecy`, `zeroize`, `thiserror`, `chacha20poly1305`, `hkdf`, `sha2`, OS sealing bindings; later a FIDO2 library | apps (`inforno_core`, `autorno`), `vaultorno` |

- `vaultorno` must **not** depend on `inforno_core` or any GUI crate. Each
  mode must stay small enough to review completely.
- Protocol and statement types live in `vaultorno_client::protocol`, shared
  by both sides so they can't drift apart.
- **Locker encryption lives only in the client** (`vaultorno_client::seal`,
  behind a default feature). `vaultorno` depends on `vaultorno_client` with
  default features off, so the code that reads the user's half and derives
  app keys isn't even linked into it.
- **The client is the only API apps see.** Its backends have the same
  semantics (lockers, revisions, expiry, limits), so apps don't care which
  one they get:
  - `Daemon`: vaultorno, with per-app protection; blind.
  - `Local`: per-user OS sealing (`systemd-creds --user`, DPAPI), one file
    per locker in `~/.local/share/vaultorno/local_<app>/<locker>.cred`, in the same
    fixed layout as the daemon's files (§6) with the OS sealer in place of
    the master key. Writes take a file lock and compare the revision; the
    client deletes expired files. This replaces today's
    `inforno_core::vault`. Any program running as the user can open these
    files; used when the daemon isn't installed, and reported as such.
  - `Keychain`: macOS, later.
- `describe()` tells the app which backend and level it got, how blind the
  daemon is, and when its statement expires, for its security report.

### Formats

JSON only where programs exchange structured data; everything else uses the
simplest format that does the job:

| Data | Format | Why |
|---|---|---|
| Protocol messages | JSON | Nested, optional and versioned fields; binary fields as base64 |
| Statements, succession statements, package manifests | JSON | The same types as the protocol, so no second parser; signatures cover the exact bytes, so formatting doesn't matter |
| Revoked keys | OpenSSH public keys, one per line, as in sshd's `RevokedKeys` | Parsed by the same library that verifies signatures; root can edit it by hand; `ssh-keygen -lf` prints the fingerprints vaultorno uses for vault names |
| Successions | one line per rotation: old key's fingerprint, then the new public key | Same key parser; readable at a glance |
| Locker and `daemon_half` files | fixed binary layout (§6) | Nobody reads ciphertext by hand; no base64 overhead; fixed offsets need no parser |
| The user's half | 32 raw bytes | Nothing to describe |
| Log | plain text, one event per line (§11) | Readable with `less` and `grep` |

### File names

Every file and directory vaultorno names uses **lowercase ASCII letters,
digits and underscores**, with dots only before extensions:
`master_key.cred`, `revoked_keys`, `daemon_half`, `user_half`,
`inforno_statement.json`. Such names mean the same on every OS and
filesystem, case-sensitive or not, and need no quoting in a shell.

- **Names that become file names follow the same rule:** app names, binary
  names and locker names are `[a-z0-9_]{1,32}`. The daemon refuses anything
  else.
- **The units, users and sockets follow it too:** `vaultorno_inspect`,
  `vaultorno_sweep.timer`, `/run/vaultorno_inspect.sock`.
- **One exception:** `CACHEDIR.TAG`, whose name is fixed by the convention
  backup tools look for (§14).

---

## 4. Concepts

- **App**: a set of binaries released together, identified by the key that
  signs their statements. Its name (e.g. `inforno`) is only a label: two
  keys are two apps, even with the same name.
- **Vault**: one app's lockers for one user, identified by the user and the
  app's key fingerprint (§6).
- **Statement**: a small JSON file, signed with the app's key, naming one
  executable (by hash), the lockers that binary may read or write, and until
  when the statement is valid (§5).
- **Package manifest**: the unsigned equivalent for binaries installed by
  the package manager, trusted because only root can write it (§5).
- **Locker**: a named blob in a vault, declared by the statements that use
  it. Names: `[a-z0-9_]{1,32}`.
- **Revision**: a random 64-bit value that changes on every write. `0`
  means the locker is empty.
- **Expiry**: when a locker is emptied automatically. It is set when the
  locker is filled, from the lifetime in the writer's statement, at most 90
  days later (§6).
- **App key**: the key that encrypts a vault's lockers. It exists only
  inside app processes, derived from two halves (§6).
- **Daemon's half**: 32 random bytes per vault, held by the daemon and
  sealed under its master key.
- **User's half**: 32 random bytes per vault, in a private file in the
  user's home folder.
- **Level**: the identity checks a binary must pass.

### Levels

From loosest to strictest:

| Level | Identity | Other checks |
|---|---|---|
| `watertight` | **package manifest**: a root-owned binary at the path a root-owned manifest lists (trusts the package manager) | binary and every parent directory root-owned, not group/world-writable; environment check; `ptrace_scope >= 1` |
| `airtight` | **signed statement**: valid, unexpired, by the app's key, for this exact executable (§8) | environment check; `ptrace_scope >= 2` |
| `hermetic` (later) | as airtight | `ptrace_scope = 3`, so nothing (not even the inspector) can read another process's memory; master key must be TPM2-sealed. The only level at which no part of vaultorno can read contents (§2) |

The identity decides the level: a package manifest gives `watertight`, a
signed statement `airtight`. A statement may ask for `hermetic`. There is no
level without an identity; development builds are signed too (§8).

With `ptrace_scope = 1`, a program running as the user can start the genuine
binary as its own child and attach to it. Watertight therefore stops generic
malware, not malware written against it.

Blindness toward the daemon doesn't depend on the level: the client always
encrypts.

---

## 5. Statements and package manifests

Access is declared by the app, not configured on the machine. There is no
configuration file.

### Statements

Every binary that opens a vault presents a statement, signed with the app's
key (§8). `autorno_statement.json`:

```json
{
  "format": 1,
  "app": "inforno",
  "binary": "autorno",
  "sha256": "9f2c…",
  "issued": "2026-10-16T00:00:00Z",
  "expires": "2027-01-14T00:00:00Z",
  "lockers": {
    "api_keys":        { "access": "read" },
    "realm_store_key": { "access": "read" }
  }
}
```

`inforno_statement.json`:

```json
{
  "format": 1,
  "app": "inforno",
  "binary": "inforno",
  "sha256": "41ab…",
  "issued": "2026-10-16T00:00:00Z",
  "expires": "2027-01-14T00:00:00Z",
  "level": "airtight",
  "reject_env": [],
  "lockers": {
    "api_keys":        { "access": "write", "lifetime_days": 30 },
    "realm_store_key": { "access": "write", "lifetime_days": 7 }
  }
}
```

Rules:

- **The signing key is the app.** The vault is identified by the fingerprint
  of the key that signed the statement; `app` is only a label shown to
  users. A statement signed by any other key opens a different, empty vault,
  so malware can't sign its way into Inforno's. Releases and development
  builds signed with different keys get separate vaults, which keeps
  development builds away from real secrets.
- **A statement covers one executable,** by SHA-256. The daemon compares it
  with the hash the inspector computed (§7).
- **It expires.** The daemon refuses a statement before `issued`, after
  `expires`, or valid for more than 180 days. Recommended: 90 days for
  releases, 7 days for development builds. To revoke a version, stop
  re-signing its statement.
- **It lists the binary's lockers:** `read`, or `write` (which includes
  read) with `lifetime_days` (default 30, at most 90). A binary can touch
  only the lockers its statement names, and must name at least one. An app
  may use at most 16 locker names; the daemon checks this when a locker is
  first filled.
- **`app`, `binary` and locker names** follow the naming rule (§3).
- **`level`** is optional, `airtight` or `hermetic`. **`reject_env`**
  (optional) adds the app's own code-loading variables, e.g. a plugin path,
  to the built-in reject list (§7).
- **Several statements may exist for one executable,** e.g. an older one
  with broader access. Each stays valid until it expires, so narrowing
  access takes effect when the older statement runs out.
- **Where statements are kept doesn't matter for security:** they are signed
  and bound to one hash. A statement is `<binary>_statement.json` with its
  signature in `<binary>_statement.json.sig`, where `<binary>` is the
  executable's file name without extension. The client looks next to its
  executable, then in `~/.local/share/vaultorno/statements/<app>/` (fresh
  copies the app fetched itself), then in
  `/usr/share/vaultorno/statements/<app>/`, and sends the newest valid one.

### Package manifests

For binaries the package manager installs without signing (e.g. a
distribution's own build, whose hash no upstream statement can match), the
package adds a root-owned manifest, `/usr/share/vaultorno/manifests/<app>.json`:
the same locker declarations, with a path per binary instead of a hash, and
no expiry.

```json
{
  "format": 1,
  "app": "inforno",
  "binaries": {
    "inforno": { "path": "/usr/bin/inforno",
                 "lockers": { "api_keys": { "access": "write", "lifetime_days": 30 } } },
    "autorno": { "path": "/usr/bin/autorno",
                 "lockers": { "api_keys": { "access": "read" } } }
  }
}
```

It is trusted because only root can write it, and gives `watertight`
identity (§7). Its vault is `pkg_<app>`, separate from any signed vault. A
manifest is part of the package that ships it, like a `.desktop` file; it is
not machine configuration.

### Parsing

- **JSON** with `serde_json` into typed structs that refuse unknown fields.
  Times are RFC 3339 in UTC, in exactly the form `YYYY-MM-DDTHH:MM:SSZ`.
- **Signature first, parsing second.** The daemon verifies a statement's
  signature over the file's exact bytes before it parses them, so the parser
  only ever sees input signed by some key. (The key may still be anyone's;
  the parser must be robust anyway, and is fuzzed, §17.)
- **Key lists** (`revoked_keys`, `successions`) are parsed line by line with
  the same OpenSSH key parser that verifies signatures. Blank lines and lines
  starting with `#` are ignored; any other line that doesn't parse makes the
  daemon refuse everything, rather than guess (§9).

### No configuration

Each setting the former `/etc/vaultorno/policy.yml` had is now built in, part
of the app's statement, or unnecessary:

| Former setting | Now |
|---|---|
| per-app binaries, keys and locker lists | statements and package manifests, shipped by the app |
| minimum level | decided by the identity; a statement may ask for `hermetic` |
| extra rejected variables | `reject_env` in the app's statement or manifest |
| allowed users | none: every user may have vaults, each separate |
| denied keys | `revoked_keys` in the `vaultorno` user's home, written by `vaultorno revoke` or by hand (§6, §12) |
| hash exceptions for unsigned test builds | sign the test build with a development key (§8) |
| the `open` level | removed: every binary has an identity |
| level overrides | removed: levels are built in |

Every limit and list that remains (lifetimes, sizes, the built-in reject
list) is a constant in the code, and `vaultorno status` prints them all.

---

## 6. Encryption, state and expiry

Every locker is encrypted twice:

| Layer | Done by | Key | Protects against |
|---|---|---|---|
| **Client** (inner) | `vaultorno_client`, inside the app's process | the app key: the daemon's half and the user's half combined | the daemon, its user, its files and its master key |
| **Daemon** (outer) | the daemon | derived from the daemon's master key | copies of the state used on another machine; mix-ups between users, vaults and lockers |

The client layer is what makes the daemon blind. The master key also
protects the daemon's halves, so it is required; encrypting the lockers a
second time with it is defence in depth (§18).

### State

Everything vaultorno keeps lives in the `vaultorno` user's home, which is
its state directory:

```
/var/lib/vaultorno/               ~vaultorno; owner vaultorno, 0700
├── CACHEDIR.TAG                  keeps backup tools out (§14); name fixed by that convention
├── master_key.cred               the master key, sealed by systemd-creds (TPM2 when available)
├── revoked_keys                  refused signing keys, one OpenSSH public key per line
├── successions                   key rotations, one per line (§8)
└── users/<uid>/
    ├── log, log.1, log.2         plain text, one event per line (§11)
    └── <vault>/
        ├── lock                  serialises the daemon's threads and the sweep
        ├── daemon_half           the daemon's half, sealed under the master key
        └── lockers/<locker>      one locker: the client's envelope, sealed under the master key
```

And on each user's side:

```
~/.local/share/vaultorno/
├── <app>_<key id>/user_half      the user's half: 32 bytes, 0600, in a 0700 directory
├── statements/<app>/             statements the app fetched itself (§5)
└── local_<app>/                  the Local backend's lockers, when there is no daemon (§3)
```

Nothing else exists: no configuration, no cache, no database. On Windows
the same tree lives in `%ProgramData%\vaultorno\`, with users by SID and the
master key sealed with DPAPI under the service account, and the user's side
in `%LocalAppData%\vaultorno\`.

`revoked_keys` and `successions` look like this:

```
# revoked_keys: one OpenSSH public key per line, as in sshd's RevokedKeys
ssh-ed25519 AAAAC3Nza… inforno_release_2026 stolen 2026-11-02

# successions: the old key's fingerprint, then the new key as in a .pub file
SHA256:3f9a… ssh-ed25519 AAAAC3Nza… inforno_release_2027
```

The daemon rereads either file whenever it changes, so root may also edit
them by hand.

### Vault names

A vault is named by the full SHA-256 fingerprint of the app's signing key,
in lowercase hex, or `pkg_<app>` for a package manifest. Where a shorter
form helps people, the **key id** is the first 16 hex digits of the
fingerprint. The daemon only ever uses the full fingerprint to decide which
vault a binary opens.

### The two halves

- **The daemon's half** is generated by the daemon the first time a binary
  passes `Hello` for a vault, and stored as `daemon_half` in that vault's
  directory, sealed under the master key like a locker. It is sent in
  `Welcome` (§9) to every binary that passes the identity check for that
  vault. It is never logged and never shown by the admin commands.
- **The user's half** is `~/.local/share/vaultorno/<app>_<key id>/user_half`
  on Linux and `%LocalAppData%\vaultorno\<app>_<key id>\user_half` on
  Windows (`<app>_pkg` for a package manifest): 32 random bytes. A key id
  shared by two keys would only make their vaults share a user's half, which
  is harmless because their daemon's halves differ. The client:
  - finds the home folder with `getpwuid_r` (Windows: the user's profile
    folder API), never from `$HOME`;
  - creates the directory `0700` and the file `0600` with
    `O_CREAT | O_EXCL`. If two binaries race, the loser reads the winner's
    file;
  - refuses the file unless it is a regular file owned by the user, not
    readable or writable by group or others, and exactly 32 bytes. On
    Windows, its ACL grants only the user (plus SYSTEM and Administrators,
    which are root-equivalent anyway).

  These permissions are the boundary. They keep out the `vaultorno` user
  even outside its sandbox, and `CAP_SYS_PTRACE` doesn't override them.
  `ProtectHome=yes` hides users' home folders from the daemon entirely, as a
  second wall.
- **Combining:** the app key is HKDF-SHA256 with input key material
  `daemon's half ‖ user's half`, salt `vaultorno_split/v1`, and info
  `<uid or SID> ‖ 0x00 ‖ <vault>`, the uid in decimal (neither part can
  contain a NUL byte). Not XOR: with XOR, a compromised daemon could
  shift the key by a value it chooses.
- **Key check:** `key_id` is the first 16 bytes of HKDF over the app key with
  info `vaultorno_split/v1/key_id`. Every envelope carries it, so a wrong
  half shows up as `KeyMismatch` rather than as a decryption failure.
- **Missing user's half:** if the file is missing while any locker is
  non-empty (`List` shows sizes), `Vault::open` fails with `KeyUnavailable`;
  the client never creates a new half then. That happens when the file is
  deleted, or when a home folder is restored on another machine. The lockers
  are lost; for disposable data, that means re-entering it.
- **Reset:** `Vault::reset`, by a binary that can write every locker, empties
  the lockers and replaces the user's half, which gives a new app key.
  `sudo vaultorno forget` deletes the whole vault, the daemon's half
  included.
- **The user's half is not a secret from the user's own programs.** Any
  process running as the user can read it; against those, the daemon's
  identity checks are the protection.

### Expiry

- **Set when a locker is filled:** the first `Put` into an empty locker sets
  `expires = now + lifetime`, from the writer's statement. Later writes keep
  that expiry; they never extend it.
- **When it passes, the locker is emptied:** the daemon deletes the file and
  treats the locker as empty (revision 0). It checks on every access, and the
  daily `vaultorno sweep` (§14) deletes expired lockers even if no app asks.
  The sweep needs no key: it reads the expiry at a fixed offset in each
  file's header (below), which only the `vaultorno` user can change.
- **Apps see it:** `List` and `Get` return `expires`, so an app can warn the
  user before a key lapses.
- **Refilling is the app's choice.** An app could copy contents into a new
  period by emptying and refilling a locker. Vaultorno doesn't prevent that;
  apps must not do it without the user (§13).
- **Expiry deletes; it doesn't shred.** Copies made earlier (snapshots,
  backups) aren't affected. Leave `/var/lib/vaultorno` out of backups and
  filesystem snapshots (§14): lockers are disposable and bound to the
  machine, so there is nothing to restore, and copies only widen the window.
  Destroying keys instead of files wouldn't add much: the daemon's halves sit
  in the same directory as the lockers, so the same copies would hold both.
- **Stale vaults:** a vault with no filled locker for 90 days is deleted, its
  daemon's half included. The user's half becomes unused; the client deletes
  it when it finds no daemon's half for it.
- **The clock** is the system clock, which only root can change.

### Locker envelope (client layer)

What the client sends in `Put` and gets back from `Get`:

| Bytes | Field |
|---|---|
| 4 | magic `VLT1` |
| 16 | `key_id` |
| 24 | nonce, random per write |
| rest | XChaCha20-Poly1305 ciphertext of `length (u32, big-endian) ‖ data ‖ zero padding`, followed by the 16-byte tag |

- **Key:** HKDF-SHA256 over the app key, with info
  `vaultorno_client/v1/<vault>/<locker>`.
- **Associated data:** magic ‖ `key_id` ‖ `<uid or SID> ‖ 0x00 ‖ <vault>` ‖
  0x00 ‖ locker name. A ciphertext moved to another locker, vault or user fails to decrypt,
  so a compromised daemon can't swap contents around.
- **Padding:** the whole envelope is padded to a power of two between 1 KiB
  and 256 KiB, so the daemon learns only a size class. The overhead is 64
  bytes, so a locker holds up to 262,080 bytes of app data.
- **The daemon checks the shape** of every non-empty `Put`: the magic, and a
  size that is a power of two in range. That catches a client bug that skips
  encryption; it can't, and needn't, prove the bytes are encrypted.
- **The revision is not in the associated data:** the daemon assigns it after
  the client has encrypted. A rolled-back locker therefore still decrypts
  (§2, §18).

### Daemon files (outer layer)

Every `daemon_half` and locker file has the same fixed layout:

| Bytes | Field |
|---|---|
| 4 | magic `VLTD` |
| 1 | layout version, `1` |
| 8 | revision, big-endian (`0` for `daemon_half`) |
| 8 | expiry, Unix seconds, big-endian (`0` for none) |
| 24 | nonce, random per write |
| rest | XChaCha20-Poly1305 ciphertext of the contents, followed by the 16-byte tag |

- **Milestone 1 writes layout version `0`:** the header, then the contents
  as they are, with no nonce, outer encryption or tag. Version `1` adds them
  in Milestone 4. Each build reads only the layout it writes.
- **Per-file key:** HKDF-SHA256 over the master key, with info
  `vaultorno/v1/<uid or SID>/<vault>/<file name>`.
- **Associated data:** the first 21 bytes (magic, version, revision, expiry),
  then the user ID or SID, vault and file name. A file moved to another
  user, vault or name fails to decrypt, and so does an edited revision or
  expiry.
- **The header is readable without the key,** so `List`, the sweep and the
  admin commands read revisions and expiries without decrypting anything.
  The daemon verifies them when it decrypts.
- **Writes are atomic:** temp file, sync, rename, sync the directory.
  Emptying a locker deletes its file.
- **Keyed by user ID / SID, never by user name:** IDs come from the
  kernel/OS and survive renames.
- **Owner:** the `vaultorno` system user; files `0600`, directories `0700`,
  via `StateDirectory=vaultorno`. Windows: `NT SERVICE\vaultorno` only (ACL).

### Master key setup (Linux)

1. **At install:** the package's install script runs `sudo vaultorno init`,
   which creates the state directory (with `CACHEDIR.TAG`) and seals 32
   random bytes with
   `systemd-creds encrypt --name=master_key - /var/lib/vaultorno/master_key.cred`
   (TPM2 when available, otherwise the host key). It refuses if a master key
   already exists: replacing it would make every vault unreadable.
2. **In the unit:** `LoadCredentialEncrypted=master_key:/var/lib/vaultorno/master_key.cred`.
   At every start, systemd unseals it and places it in
   `$CREDENTIALS_DIRECTORY/master_key`, readable only by the daemon.
3. **The daemon** derives each file's outer key from it. Its files are
   therefore useless on any other machine, and so is a copy of
   `master_key.cred`. Even with the master key, they hold only the client's
   ciphertext and the daemon's halves, never a user's half.

The XChaCha20 + HKDF code was already written and tested for an earlier
version of `inforno_core::vault`; both layers reuse it.

---

## 7. Identifying the caller

### Linux

1. **Transport:** Unix socket `/run/vaultorno.sock`, mode `0666`. Access is
   decided per connection, not by file permissions.
2. **Process:** `SO_PEERCRED` gives the user and process ID; `SO_PEERPIDFD`
   (Linux 6.5+) pins the check to that exact process.
3. **Inspection:** the daemon passes the pidfd to the inspector over
   `/run/vaultorno_inspect.sock` (§14), and the inspector:
   - opens `/proc/<pid>/exe`, which is the inode actually running even if
     the path was replaced since, and hashes it (SHA-256);
   - reports its path, device and inode;
   - reads `/proc/<pid>/environ` (the environment at exec time) and reports
     which variables are set, by name only, so the daemon can check them
     against the reject list;
   - confirms through the pidfd that the process is still alive, which
     rules out process-ID reuse.
4. **Identity, in this order:**
   - **Signed statement:** the client sends a statement and its signature
     in `Hello` (§5). The daemon verifies the signature for namespace
     `vaultorno/statement`, refuses keys listed in `revoked_keys`, checks the
     validity window, and checks that the statement's `sha256` equals the
     inspector's hash. The vault is the signing key's.
   - **Package manifest:** the executable is at a path a manifest in
     `/usr/share/vaultorno/manifests/` lists (same device and inode), and
     it, the manifest and every parent directory of both are root-owned and
     not writable by group or others.
   - **Executables must be world-readable** (`0755`, as cargo builds them),
     or the inspector can't hash them.
5. **Environment:** refuse if a rejected variable is set. The built-in list
   is variables that make a process load code:
   - `LD_*` (preload, audit, library path);
   - `VK_*`: Vulkan layers and drivers (`VK_LAYER_PATH`, `VK_ADD_LAYER_PATH`,
     `VK_INSTANCE_LAYERS`, `VK_ICD_FILENAMES`, `VK_DRIVER_FILES`, …).
     Inforno renders with wgpu on Vulkan.
   - `__EGL_VENDOR_LIBRARY_DIRS`, `__EGL_VENDOR_LIBRARY_FILENAMES`,
     `LIBGL_DRIVERS_PATH`, `GBM_BACKENDS_PATH`;
   - `GTK_MODULES`, `GTK_PATH`, `GIO_MODULE_DIR`, `GIO_EXTRA_MODULES`,
     `QT_PLUGIN_PATH`, `QT_QPA_PLATFORM_PLUGIN_PATH`;
   - plus the app's own `reject_env` from its statement or manifest. A
     trailing `*` matches a prefix.

   A denylist is never complete: this stops generic malware, not malware
   written for one app. Code loaded from user-writable **files** rather than
   variables (e.g. Vulkan layers in `~/.local/share/vulkan/`) is invisible
   to the daemon; see §13.
6. **Debugging protection:** read `/proc/sys/kernel/yama/ptrace_scope` and
   compare it with the level's minimum.
7. **Vault, binary and lockers** all come from the identity; the client
   doesn't choose them. A binary opens only its own app's vault (access to
   another app's vault is deferred, Appendix A).

### Windows

- **Transport:** named pipe `\\.\pipe\vaultorno`. The service runs
  `vaultorno daemon` as the virtual account `NT SERVICE\vaultorno`, not
  LocalSystem.
- **Process:** `GetNamedPipeClientProcessId`, then `OpenProcess` with only
  `PROCESS_QUERY_LIMITED_INFORMATION`, and `QueryFullProcessImageNameW` for
  the executable. These need no special privileges, so there is no inspector
  mode on Windows.
- **User, at identification level only:** the client opens the pipe with
  `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`. The daemon's
  `ImpersonateNamedPipeClient` then yields a token it can read the user's SID
  from, but can't act with. With the default impersonation level, a
  compromised daemon could open the user's files as the user, including the
  user's half. The daemon also refuses any client whose token allows more
  than identification, which catches a client that forgot the flag.
- **Identity:** the same signed statements, sent in `Hello`; Authenticode can
  come later. Package manifests mean binaries under `Program Files`, with
  ACLs allowing writes only to Administrators, SYSTEM and TrustedInstaller.
- **Limit:** any program running as the user can open the app's process and
  read or change its memory. Until a defence is designed, vaultorno on
  Windows only protects the stored lockers from programs that don't go that
  far, and the security report says so. Environment and DLL checks are still
  to be designed.

### macOS (later)

No daemon. The client's `Keychain` backend stores each locker as a Keychain
item; the Keychain enforces per-app access for signed apps.

---

## 8. Signatures and statement lifecycle

- **One format:** OpenSSH signatures (SSHSIG) with SHA-256, namespace
  `vaultorno/statement`, over the statement file's exact bytes. The
  statement in turn names the executable's hash.
- **Release builds:** a plain `ssh-ed25519` key, kept offline. Statements
  valid 90 days, re-signed regularly (e.g. monthly) for every version still
  supported, and shipped with updates or fetched by the app.
- **Development builds:** a FIDO2 key on the developer's YubiKey
  (`sk-ssh-ed25519@openssh.com`). Each signature needs a touch, and the
  private key can't leave the YubiKey. Statements valid 7 days. Because it's
  a different key, development builds get their own vault and never see the
  release vault's keys.
- For `sk-` keys, verification must also require the "user present" flag.

```sh
# once
ssh-keygen -t ed25519-sk -O application=ssh:vaultorno_signing -f ~/.ssh/vaultorno_sign_sk

# every build (wrapped in `cargo xtask build`)
sha=$(sha256sum target/release/inforno | cut -d' ' -f1)
cat > target/release/inforno_statement.json <<EOF
{
  "format": 1,
  "app": "inforno",
  "binary": "inforno",
  "sha256": "$sha",
  "issued": "$(date -u +%FT%TZ)",
  "expires": "$(date -u -d '+7 days' +%FT%TZ)",
  "lockers": { "api_keys": { "access": "write", "lifetime_days": 30 } }
}
EOF
ssh-keygen -Y sign -f ~/.ssh/vaultorno_sign_sk -n vaultorno/statement \
    target/release/inforno_statement.json   # → target/release/inforno_statement.json.sig
```

- **Build and sign out of reach of the user's other programs** (CI, a
  separate user account or a container). Malware that swaps the binary
  after it was hashed only gets a mismatch; malware that swaps it before
  gets a signed copy of itself.
- **Revoking a version:** stop re-signing its statement; it lapses within
  one lifetime.
- **A stolen key:** root runs `vaultorno revoke <public key or .pub file>`,
  which appends it to `revoked_keys`; statements signed by it stop working
  at once. The developer then rotates to a new key.
- **Key rotation:** since the key is the app, a new key is a new vault. To
  carry the vault over, the old key signs a succession statement:

  ```json
  {
    "format": 1,
    "succession": {
      "from": "SHA256:…",
      "to": "ssh-ed25519 AAAA… inforno_release_2027"
    },
    "issued": "2027-01-01T00:00:00Z"
  }
  ```

  The client sends it in `Hello` with a statement signed by the new key; the
  daemon then serves the old vault to the new key and records the rotation
  as a line in `successions`. Without one, users simply start with an empty
  vault and re-enter their keys, which is acceptable for disposable data. A
  key that was stolen shouldn't be carried over: revoke it and start fresh.
- **Open item (M0):** confirm a Rust crate (likely `ssh-key`) can verify
  `sk-ssh-ed25519@openssh.com` SSHSIG signatures, including the flag.

---

## 9. Protocol (`vaultorno_client::protocol`)

- **Framing:** 4-byte big-endian length, then UTF-8 JSON; binary fields are
  base64. At most 512 KiB; a violation closes the connection.
- **Requests carry an `id`; responses echo it.** Identity is checked once,
  at `Hello`. One connection opens one vault.
- **On the wire,** a frame is `{"id": 1, "body": {"type": "get", "locker":
  "api_keys"}}`: the variant name in snake_case as `type`, unknown fields
  refused.
- **A new connection has 10 seconds to say `Hello`.** Over the connection
  limit, the daemon answers the first request with `Busy` and closes.

```rust
struct Frame<T> { id: u32, body: T }

enum Request {
    Hello { protocol: u32,
            statement: Option<String>,                     // the statement file (§5), if signed
            signature: Option<String>,                     // its SSHSIG
            succession: Option<String> },                  // with its signature, after a key rotation (§8)
    List,
    Get { locker: String },
    Put { locker: String, data: Vec<u8>, expected: u64 },  // a client envelope (§6); empty data empties the locker
}

enum Response {
    Welcome { protocol: u32, app: String, vault: String, binary: String, level: String,
              sealer: String,
              half: Vec<u8>,                               // the daemon's half of the app key (§6)
              statement_expires: Option<u64>,              // Unix seconds; none for manifests
              refused: u32 },                              // refused opens since the last success
    Lockers { lockers: Vec<LockerInfo> },
    Locker { data: Vec<u8>, revision: u64, expires: Option<u64> },  // empty locker: no data, revision 0
    Stored { revision: u64, expires: u64 },
    Error { code: ErrorCode, message: String },
}

struct LockerInfo { name: String, revision: u64, size: u32,       // size: stored size class
                    expires: Option<u64>, read: bool, write: bool }

enum ErrorCode {
    ProtocolMismatch, NotAllowed, UnknownLocker,
    Conflict, TooLarge, NotSealed, Storage, Busy,
}
```

- **`List`** returns the lockers the caller's statement names, with
  revisions, size classes and expiries. An app notices changes cheaply this
  way, e.g. autorno before starting a harness.
- **`Put`** succeeds only if `expected` equals the current revision (`0` to
  create), and returns the new revision and the locker's expiry. Otherwise
  `Conflict`: re-read, merge, retry. Data that isn't shaped like a client
  envelope gets `NotSealed` (§6). The first `Put` into an empty locker sets
  its expiry (§6).
- **`Welcome.half`** is the daemon's half of this vault's app key, sent only
  after every identity check has passed. **`Welcome.sealer`** names the
  daemon's outer layer. Nothing in `Welcome` can turn the client's
  encryption off.
- **`Welcome.statement_expires`** lets the app warn the user before its
  statement runs out (§13).
- **`Welcome.refused`** counts refused attempts to open this vault since its
  last successful open. The app can warn the user that other programs tried.
- **`NotAllowed`** says which check failed ("statement expired on
  2027-01-14", "VK_LAYER_PATH set", "ptrace_scope is 1, airtight needs 2").
  Users need that to fix their setup; it gives an attacker nothing new.
- **`Storage`** also covers unreadable state, e.g. a line in `revoked_keys`
  that isn't a public key; the daemon then refuses everything rather than
  guess.

### Client API

```rust
use vaultorno_client::Vault;

let vault = Vault::open("inforno")?;             // daemon or local fallback; sends a statement; combines the halves
println!("{}", vault.describe());                // "vaultorno, airtight, blind, statement until 2027-01-14"

let keys = vault.get("api_keys")?;               // decrypted here: Locker { data: SecretBox<[u8]>, revision, expires }
vault.put("api_keys", &bytes, keys.revision)?;   // encrypted here; Err(Conflict) if it changed meanwhile
vault.update("api_keys", |old| Ok(edit(old)))?;  // get + put, retried on conflict

for locker in vault.list()? { /* name, revision, size, expires, read, write */ }
```

- **Client errors** beyond the protocol's: `NoStatement` (no valid statement
  found for this executable), `KeyUnavailable` (the user's half is missing
  while lockers aren't empty), `KeyMismatch` (an envelope's `key_id` isn't
  this app key's, i.e. one of the halves changed), `Corrupt` (an envelope
  fails to decrypt).
- **`describe()` states the blindness exactly,** e.g. "blind; inspector
  could read app memory: ptrace_scope is 2" below `hermetic`.
- Contents are `SecretBox` in the client and zeroized on drop, as are both
  halves, the app key, padded buffers and decoded responses. The daemon never
  has plaintext to zeroize.

---

## 10. Limits

| Limit | Value |
|---|---|
| Lockers per app | 16 names across its statements (checked when a locker is first filled) |
| Locker size | 256 KiB stored, i.e. 262,080 bytes of app data (§6) |
| Stored sizes | powers of two from 1 KiB to 256 KiB |
| Locker lifetime | default 30 days, at most 90 days |
| Statement validity | at most 180 days |
| Vaults per user | 64; a vault with no filled locker for 90 days is deleted |
| Message size | 512 KiB |
| Connections per user | 32 |
| Refused requests | progressive back-off per process |

These are constants in the code, printed by `vaultorno status`; changing one
is a deliberate code change.

---

## 11. Log

- **What:** per user, in `~vaultorno/users/<uid>/log`, one event per line:
  time, then `key=value` fields for vault (label and key id), binary,
  executable hash prefix, statement expiry, request, locker, result, and the
  failed check if refused. Expiry deletions are logged too. **Never
  contents**, which the daemon doesn't have anyway, and never the daemon's
  halves.

  ```
  2026-10-16T05:12:03Z vault=inforno/3f9a1c2e7b4d5a60 binary=autorno exe=9f2c4b1a request=get locker=api_keys result=ok
  2026-10-16T05:12:09Z vault=- binary=- exe=77d01e2f request=hello result=refused check="VK_LAYER_PATH set"
  ```
- **Size:** capped and rotated (`log`, `log.1`, `log.2`; 1 MiB each).
- **Reading:** `sudo vaultorno log --user U`, or any text tool as root. Apps
  see only the `refused` count in `Welcome`, which keeps the log away from
  malware running as the user.

---

## 12. Admin commands

All are modes of the same `vaultorno` executable (§3). None reads a user's
half, so none can read contents, even as root.

| Command | Root | What it does |
|---|---|---|
| `vaultorno status` | no | Everything vaultorno does on this machine: version, built-in constants and reject list, platform checks (`ptrace_scope`, TPM2, how the master key is sealed). As root, also revoked keys and recorded successions. Also the input for apps' security reports. |
| `vaultorno check <binary>` | no | Dry run: which statement or manifest it would use, which vault, level and lockers, and which check fails. |
| `vaultorno vaults --user U` | yes | Vaults (label, key fingerprint) and their lockers: size classes, revisions, expiries. Never contents or halves. |
| `vaultorno log --user U` | yes | Show the log (§11). |
| `vaultorno revoke <public key or .pub file>` | yes | Append a key to `revoked_keys`; statements signed by it stop working at once. `--undo` removes it. |
| `vaultorno forget <vault> [--locker L] [--user U]` | yes | Delete lockers. Without `--locker`, deletes the whole vault, the daemon's half included. |
| `vaultorno sweep` | yes | Delete expired lockers and stale vaults now (the daily timer does the same, §14). |
| `vaultorno init` | yes | Create the state directory and the sealed master key (§6). Run once, by the package's install script. |

`vaultorno daemon` and `vaultorno inspect` are run only by their systemd
units (§14).

---

## 13. What apps must still do

Vaultorno checks who is asking. Everything else that decides what the
genuine app does is the app's responsibility:

- **Store only disposable data** (§1). Keep anything that must last in your
  own store with its own recovery, and at most a cached unlock for it in a
  locker, as Inforno's realm store does (§15).
- **Make expiry at the provider match.** Where a provider supports expiring
  or scoped keys, use them, with a lifetime no longer than the locker's.
- **Don't extend expiry behind the user's back** by emptying and refilling a
  locker with the same contents. When a locker expires, ask the user.
- **Refresh statements before they expire,** and warn the user well ahead
  (e.g. 14 days) when you can't.
- **Use only `vaultorno_client`** to talk to vaultorno: it is what encrypts.
  A hand-written protocol client would send plaintext, and the daemon's shape
  check catches only accidents.
- **Treat everything the daemon doesn't check as attacker-controlled:**
  config files, arguments, the working directory, and environment variables
  not on the reject list (e.g. `SSL_CERT_FILE`, `SSL_CERT_DIR`, proxy
  variables).
- **Run helpers by absolute path with a clean environment,** as the client
  does for `systemd-creds` in the `Local` backend.
- **Don't load code chosen by user-writable files.** The Vulkan loader, for
  example, loads layers and drivers from `~/.local/share/vulkan/` and
  `~/.config/vulkan/` with no environment variable involved. A GUI that
  renders with Vulkan must, before creating its instance, set
  `VK_LOADER_LAYERS_DISABLE=~implicit~` and point `VK_DRIVER_FILES` at the
  system driver manifests only, or keep secrets out of the GPU process.
- **Make the process non-dumpable at startup**
  (`prctl(PR_SET_DUMPABLE, 0)`, as `ssh-agent` does), so other programs
  running as the user can't attach to it or read its memory, even where
  `ptrace_scope` would allow it. Release builds only; it also blocks
  debuggers and core dumps. (It doesn't stop holders of `CAP_SYS_PTRACE`,
  such as the inspector; only `hermetic` does.)
- **Keep whatever decides where secrets may go inside a locker**, e.g. an
  API key's allowed hosts, not in a config file.
- **Offer no way to export secrets** that a config file or argument can
  trigger without the user.
- **Keep plaintext briefly** and zeroize it.
- **Never hand the vaultorno socket to less trusted code** (sandboxes,
  agents); give them only what they need.

---

## 14. Installation (Linux)

- **One executable:** `/usr/bin/vaultorno`, with no file capabilities and no
  setuid bit (§3).
- **System users** (`/usr/lib/sysusers.d/vaultorno.conf`). The `vaultorno`
  user's home is its state directory, so everything it keeps is under
  `~vaultorno`:

  ```
  u vaultorno          - "vaultorno daemon"    /var/lib/vaultorno  /usr/bin/nologin
  u vaultorno_inspect  - "vaultorno inspector" -                   /usr/bin/nologin
  ```
- **Units:** `vaultorno.socket` → `vaultorno.service`, and
  `vaultorno_inspect.socket` → `vaultorno_inspect.service`, both
  socket-activated, on `/run/vaultorno.sock` and
  `/run/vaultorno_inspect.sock`. `vaultorno_sweep.timer` starts
  `vaultorno_sweep.service` daily.
- **No configuration files.** The package installs the executable, the
  units, the timer and the `sysusers.d` entry, and its install script runs
  `vaultorno init` (§6). Apps' own packages install their statements or
  manifests in `/usr/share/vaultorno/`.
- **No backups or snapshots of the state:** `~vaultorno` carries a
  `CACHEDIR.TAG`, which backup tools can be told to honour (e.g. the
  exclude-caches options of borg, restic and tar). The package also documents
  keeping `/var/lib/vaultorno` out of filesystem snapshots, e.g. as its own
  btrfs subvolume excluded from snapper. Lockers are disposable and
  machine-bound, so there is nothing to restore.

### Privilege separation

| | Daemon (`vaultorno daemon`) | Inspector (`vaultorno inspect`) |
|---|---|---|
| Talks to | apps (any process) | the daemon only: socket `0660`, group `vaultorno`, and it checks the peer's user |
| Holds | the master key; each vault's daemon half and lockers, sealed under it (the lockers are client ciphertext) | nothing |
| Capabilities | none | `CAP_SYS_PTRACE` |
| Input | protocol messages from anyone | one pidfd per request |
| Output | its half and ciphertext, to verified binaries | process and user ID, executable path, device, inode and SHA-256, names of the environment variables set |

- **The inspector mode is a few hundred lines,** in its own crate, with a
  fixed-size request (§3). The rest of the executable is in its process too,
  but never runs there: mode dispatch happens before anything else.
- **A compromised daemon** can hand its halves and ciphertext to the wrong
  process, refuse service or roll lockers back, but can't decrypt lockers or
  read other processes' memory: it never has a user's half.
- **A compromised inspector** holds no lockers. It can read process memory,
  including the app key and plaintext inside apps, unless
  `ptrace_scope = 3` (the `hermetic` level).
- **Neither can read a user's half:** they run as their own users, the file
  is `0600` in a `0700` directory, and `CAP_SYS_PTRACE` doesn't override file
  permissions. `ProtectHome=yes` also hides users' home folders from the
  daemon; it doesn't affect `~vaultorno`, which is under `/var/lib`.

```ini
# vaultorno.service
[Service]
ExecStart=/usr/bin/vaultorno daemon
User=vaultorno
Group=vaultorno
StateDirectory=vaultorno
StateDirectoryMode=0700
LoadCredentialEncrypted=master_key:/var/lib/vaultorno/master_key.cred
CapabilityBoundingSet=
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
PrivateNetwork=yes
RestrictAddressFamilies=AF_UNIX
SystemCallFilter=@system-service
MemoryDenyWriteExecute=yes
LimitCORE=0
```

```ini
# vaultorno_inspect.service
[Service]
ExecStart=/usr/bin/vaultorno inspect
User=vaultorno_inspect
AmbientCapabilities=CAP_SYS_PTRACE
CapabilityBoundingSet=CAP_SYS_PTRACE
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only        # verify whether `yes` still lets /proc/<pid>/exe open (§18)
ReadOnlyPaths=/proc          # blocks writes through /proc/<pid>/mem (verify, §18)
PrivateDevices=yes
PrivateNetwork=yes
RestrictAddressFamilies=AF_UNIX
SystemCallFilter=@system-service
SystemCallFilter=~ptrace process_vm_readv process_vm_writev
MemoryDenyWriteExecute=yes
LimitCORE=0
```

```ini
# vaultorno_sweep.service (started daily by vaultorno_sweep.timer)
[Service]
Type=oneshot
ExecStart=/usr/bin/vaultorno sweep
User=vaultorno
Group=vaultorno
StateDirectory=vaultorno
CapabilityBoundingSet=
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateDevices=yes
PrivateNetwork=yes
RestrictAddressFamilies=AF_UNIX
SystemCallFilter=@system-service
```

- **Packaging:** an Arch PKGBUILD first.
- **Windows:** one service running `vaultorno daemon` as
  `NT SERVICE\vaultorno`; no inspector (§7).

---

## 15. Inforno's use

### Lockers

| Locker | Contents (Inforno's own format) | Read | Write | Lifetime |
|---|---|---|---|---|
| `api_keys` | provider keys, each with its allowed hosts | inforno, autorno | inforno | 30 days |
| `realm_store_key` | a cached copy of the realm store's key (proposal below) | inforno, autorno | inforno | 7 days |

- **Formats are Inforno's** (versioned serde JSON in `inforno_core`).
  Vaultorno never sees them in plaintext: the client encrypts them inside
  Inforno's and autorno's processes.
- **Inforno and autorno share keys** because both statements name
  `api_keys`; only Inforno's says `write`. Both combine the same two halves.
- **When `api_keys` expires,** Inforno asks the user to enter keys again and
  suggests issuing fresh ones. Where a provider allows expiring keys,
  Inforno sets the expiry to match.
- **Allowed hosts live inside `api_keys`**, so editing Inforno's normal
  config can't send a key elsewhere.
- **"Set temporarily"** stays in Inforno's memory.

### Realm configs: outside vaultorno (proposal, not settled)

Realm configs map a Folio's realm-relative paths to real folders, hosts and
secrets, and their approvals decide what agents may do. They aren't
disposable, so they don't belong in vaultorno. The current idea:

- **The realm store** is one encrypted file in Inforno's data folder (e.g.
  `~/.local/share/inforno/realm_store.db`), holding drafts and approved
  configs with their approval records and Realm secrets. Inforno encrypts
  each row with the store key (XChaCha20-Poly1305, associated data = Realm id
  and row kind) before SQLite sees it, so SQLite's journal never holds
  plaintext. SQLCipher is the alternative if queries over contents are ever
  needed.
- **The store key** is 32 random bytes, wrapped three ways. Any one unlocks
  the store:

  | Slot | How it unlocks | Lasts | For |
  |---|---|---|---|
  | YubiKey | FIDO2 `hmac-secret`, with a touch | until replaced | re-unlocking when the cached copy has expired |
  | vaultorno | the key itself, in the `realm_store_key` locker | 7 days | Inforno and autorno opening the store without a touch |
  | Paper | a 24-word recovery phrase (256 bits) | until replaced | a lost YubiKey, or a new machine |

  When `realm_store_key` expires, the user taps the YubiKey in Inforno, which
  refills the locker. With the YubiKey or the paper phrase, the store can be
  opened on a new machine, unlike any locker.
- **Because any one slot unlocks, the weakest slot sets the bar.** The
  vaultorno slot is as strong as vaultorno's identity checks, limited to 7
  days at a time.
- **Encryption isn't authorization.** Any binary that can open the store
  could write a row marked approved, so that mark carries no weight. An
  approval is a YubiKey signature over the Realm id, version, config hash and
  an expiry; autorno uses only configs whose approval signature checks
  against the approval public key. The FIDO2 credential for unwrapping and
  the signing key can live on the same YubiKey.
- **The recovery phrase** is generated by Inforno (never chosen by the user),
  shown once, confirmed by asking for a few of its words back, and never
  stored digitally. With 256 bits of entropy, a fast key derivation is
  enough.
- **Migration:** on first start, Inforno imports existing Realm files as
  drafts, the user approves them once, and the originals are deleted.
  Inforno warns that backups and dotfile repositories may still hold copies.
- **Export** to a file stays available, as an explicit action with a
  warning.
- **Open points** (§18): where the approval key is pinned, rollback to an
  older store file, and what autorno does when the cached key has expired.

### Secrets for agents

autorno reads a Realm's secrets from the realm store and places only those
the Realm's role was approved for into the harness (e.g. a file on a tmpfs),
as part of setting up the Realm. Agents never connect to vaultorno. Neither
the socket, the user's halves (`~/.local/share/vaultorno/`) nor the realm
store may be visible inside a Realm; add a test for that.

### Startup and migration

1. `Vault::open("inforno")`. The client picks the daemon or the local
   fallback and, with the daemon, combines the half from `Welcome` with the
   user's half from the home folder; the key manager and the security report
   show which backend it got.
2. One-time moves: `~/.local/share/inforno/api-keys.cred` (today's file) and
   the old keyring entry into `api_keys`. The client encrypts them like any
   other `Put`, and their 30 days start then.
3. If `Welcome.refused` is above zero, tell the user that other programs
   tried to open Inforno's vault.
4. If `api_keys` is empty or about to expire, ask for keys; if
   `realm_store_key` is, ask for a YubiKey touch. If the statement expires
   within 14 days, say so.

### Related Inforno work (not part of vaultorno)

- Enforce allowed hosts wherever a key is sent. Check that key-carrying
  requests ignore `SSL_CERT_FILE`, `SSL_CERT_DIR` and proxy variables, or
  pin their roots.
- At startup, before wgpu: disable Vulkan implicit layers, pin the system
  drivers, and make the process non-dumpable in release builds (§13).
- Remove `.env` support and `OPENROUTER_API_KEY`; `OPENROUTER_API_URLS`
  stays for now.
- The realm store (above), once settled.
- Statements in the release flow: re-signed monthly, shipped with updates,
  and refreshable by the app.
- A `spans` crate for Realm secret powers (`secret: { span, caps }`),
  internal to Inforno and autorno.
- Folio path translation: home-folder paths in pasted text become Realm
  paths, otherwise `~/…`, with an Undo.
- Security report: backend and level, blindness (including whether the
  inspector could read memory), statement expiry, locker expiries, sealer,
  refused count, Realm approval mode.

---

## 16. Milestones

| | What | Done when |
|---|---|---|
| M0 | Spike: verify YubiKey `ssh-keygen -Y sign` signatures in Rust | Example program verifies a real signature over a JSON statement; rejects a tampered statement, the wrong namespace and a missing "user present" flag |
| M1 | `vaultorno_client` protocol and `Vault` API; client envelope and split key (the user's half in a test home folder, the daemon's half from the skeleton); `vaultorno daemon` skeleton with a test-only identity, locker files in the fixed layout without the outer encryption, revisions, expiry and `sweep`, limits, shape check; `Local` backend with expiry | Round trip from an example client; per-user separation; conflicting writers detected; lockers expire and writes don't extend them; limits enforced; no plaintext canary ever reaches the daemon; same tests pass on `Local` |
| M2 | Package manifests (path identity), levels, environment and ptrace checks, `vaultorno inspect` | Integration tests with test manifests and test binaries: unlisted binary refused, read-only binary can't `Put`, rejected variables refused |
| M3 | Signed statements (identity, lockers, expiry, `reject_env`), vaults by key fingerprint, `successions`, `revoked_keys` and `vaultorno revoke`; the `cargo xtask build` statement-and-signing flow | Signed development build of Inforno opens its own vault with no root configuration; a statement for another hash, an expired one, one signed by another key, or one signed by a revoked key gets no access to that vault |
| M4 | `vaultorno init` and the master key via `LoadCredentialEncrypted=`, the daemon's halves and outer encryption sealed under it, hardened units, two system users with `~vaultorno` as the state directory, sweep timer, `CACHEDIR.TAG` | Locker files and halves unreadable on another machine; a process running as `vaultorno`, with the state directory and the unsealed master key, can't decrypt any locker or read any user's half; the daemon runs with no capabilities; running any mode by hand as a user gains nothing; hardening items in §18 verified |
| M5 | Inforno on vaultorno: `api_keys` with expiry, key migration, statements in Inforno's release flow | No API key left in `~/.config` or `~/.local/share`; an expired `api_keys` makes Inforno ask again |
| M6 | PKGBUILD, admin commands, log | Clean install on Arch from the package, with no configuration file anywhere |
| M7 | Windows service, named pipe at identification level, DPAPI-sealed master key, the user's half under an ACL | Same tests pass on Windows; the service can't open the user's half |
| M8 | macOS `Keychain` backend in the client, with expiry | — |
| M9 | `fido2` option: the user's half wrapped by a security key (§18) | The vault opens only with the security key and a touch |

M1 is done. Not yet in it, by design: refused-request back-off (with real
identities, M2), the log (M6), `Vault::reset`, and deleting a user's half
whose vault was swept as stale.

Inforno's realm store (§15) is planned separately, once settled.

---

## 17. Testing

- **Unit tests** with a fake sealer, as in `inforno_core::vault` today.
- **Daemon integration tests:** run the daemon in-process on a socket in a
  temp directory, with a fake or in-process inspector, a fake clock, test
  manifests, a test state directory, a test home folder, and small test
  executables with statements signed by keys generated in the test.
- **One executable:** each mode runs only its own code; `vaultorno inspect`
  and `vaultorno daemon` started by hand as a user have no capability and
  can't take over either socket; the inspector refuses peers other than the
  `vaultorno` user.
- **Identity:** bad signature, wrong namespace, statement for another hash
  (tampered or renamed binary), expired or not-yet-valid statement, validity
  over 180 days, revoked key, manifest or binary not root-owned,
  `LD_PRELOAD`, `VK_LAYER_PATH` or a variable from the app's `reject_env`
  set.
- **Formats and names:** statements and manifests with unknown fields, bad
  times, duplicate locker names, or names outside `[a-z0-9_]{1,32}` are
  refused; the statement and manifest parsers are fuzzed; a line in
  `revoked_keys` or `successions` that doesn't parse makes the daemon refuse
  everything; a daemon file with a bad magic, version or length is refused.
- **Vaults and lockers:** a statement signed by another key opens a
  different, empty vault; a locker not named in the statement is refused; a
  read-only binary can't `Put`; a 17th locker name is refused when first
  filled; a succession statement carries the vault to a new key.
- **Expiry:** a filled locker expires at fill time plus its lifetime; later
  writes don't extend it; an expired locker reads as empty and its file is
  gone; the sweep deletes expired lockers and stale vaults without any app
  asking; an edited revision or expiry in a file's header fails to decrypt.
- **Blindness:**
  - a canary stored through every request type never appears in frames the
    daemon receives, its state directory, its log or a dump of its memory;
  - a process running as `vaultorno` can't read a user's half (real file
    permissions);
  - a non-envelope `Put` gets `NotSealed`;
  - an envelope moved between lockers, vaults or users, or tampered with,
    fails to decrypt;
  - a different daemon's half gives `KeyMismatch`;
  - a user's half with the wrong owner, mode or size is refused; two
    binaries creating it at once end up with one file;
  - a missing user's half with non-empty lockers gives `KeyUnavailable`,
    never a new half;
  - stored sizes are always size classes;
  - on Windows, the service's token for a client is identification-level and
    can't open the user's half.
- **Revisions:** concurrent writers get `Conflict`; an empty `Put` deletes;
  every write changes the revision.
- **Limits:** oversized locker and message refused; connection cap; vault
  cap.
- **Crash safety:** killing the daemon during a write leaves the old or the
  new contents, never a mix.
- **Local backend:** the same suite, minus identity and the daemon-side
  blindness tests.
- **Real-hardware tests** (TPM, YubiKey) are `#[ignore]` and run manually,
  like `real_systemd_creds` and `real_dpapi` today.

---

## 18. Open questions

- **Process-ID reuse on older kernels:** is `pidfd_open(pid)` right after
  accept good enough, or should the daemon require Linux 6.5+
  (`SO_PEERPIDFD`)? The inspector gets the process ID from the pidfd through
  `/proc/self/fdinfo`, or `PIDFD_GET_INFO` on Linux 6.13+.
- **Hardening to verify (M4):**
  - `ReadOnlyPaths=/proc` should block **writes** through
    `/proc/<pid>/mem`. It does **not** block reads; that's why the inspector
    is a separate process.
  - Does Yama `ptrace_scope = 3` block `/proc/<pid>/mem` while still
    allowing `exe` and `environ`, as `hermetic` assumes?
  - Does `ProtectHome=yes` still let the inspector open `/proc/<pid>/exe`
    for binaries under `/home`?
  - Can either process open a user's half by another route, e.g. through an
    app's `/proc/<pid>/cwd`, `/proc/<pid>/root` or `/proc/<pid>/fd/*`? File
    permissions should still apply; test it.
  - Does `StateDirectory=` re-own `master_key.cred` to `vaultorno`, and does
    that matter? The file is sealed, so it shouldn't.
  - Write a test that tries each path from inside the services.
- **Log in the journal instead?** Writing to the systemd journal would
  remove the log file and its rotation, and `journalctl -u vaultorno` would
  read it. But members of groups such as `adm` or `systemd-journal`, often
  the everyday user on a desktop, can read the system journal. The log holds
  no secrets, only who opened what and which attempts were refused.
- **Lifetimes:** are 30 days for lockers by default, 90 at most, and 180 for
  statements (90 recommended for releases, 7 for development) right?
- **Statement distribution:** ship refreshed statements only with package
  updates, or also let apps download them (they are signed, so the source
  doesn't matter)? What should an app do when its statement has expired and
  it's offline?
- **Distribution builds:** a distribution that rebuilds Inforno can't use
  upstream statements. A package manifest gives it `watertight`; a
  distribution signing key would give `airtight`, but as a different app
  with its own vault. Is that the right split?
- **Access to another app's vault** (e.g. a key-adding proxy reading
  `api_keys`): a grant statement signed by the vault owner's key, naming the
  other app's key and binary. Deferred until needed (Appendix A).
- **Seal the user's half too?** Sealing it with `systemd-creds --user` (Linux)
  or DPAPI (Windows) would mean a copy of the home folder doesn't hold it in
  usable form. It costs one unseal per process start, and the daemon's
  blindness wouldn't depend on it.
- **Rollback:** whoever controls the `vaultorno` user, or root, could put
  back an older locker file within its lifetime, and it still decrypts. The
  client could keep the highest revision it has seen per locker in a file
  next to the user's half, which stops rollback by the daemon alone. Worth
  it, given lockers are short-lived?
- **Keep the daemon's layer on lockers?** The master key is required for the
  daemon's halves; encrypting the lockers a second time with it duplicates
  protection. Keeping it costs little because the code exists.
- **`fido2` option:** the user's half could be stored wrapped by a FIDO2
  security key (`hmac-secret`), so reading a vault also needs the key and a
  touch. That would narrow the "compromised daemon plus malware" case (§2).
  autorno is headless, so it would need a touch each time it starts;
  acceptable, or only for some lockers?
- **Rekeying:** replacing the user's half and re-encrypting every locker,
  e.g. after a suspected exposure. `Vault::rekey`, later; with short
  lifetimes, a reset may be enough.
- **Keeping keys out of the GUI:** should key-carrying requests be made only
  by autorno (headless, no GPU stack), with the GUI never holding API keys?
  That would sidestep the Vulkan-layer problem in §13.
- **Root install with setgid binaries:** installing `inforno` and `autorno`
  setgid to an empty group would put them in the kernel's secure-execution
  mode. The dynamic linker then ignores `LD_*` variables, the Vulkan loader
  ignores the home-folder paths and `VK_*` path variables, and the process
  is non-dumpable from its first instruction. Worth testing as the "proper
  root install" option; check that nothing Inforno uses refuses to run
  setgid (GTK does).
- **Inforno's realm store (§15):**
  - Where is the approval public key pinned, so that neither malware nor a
    compromised Inforno can replace it? A root-owned file
    (`/etc/inforno/approval_keys`) works but needs root once; inside the
    store, a compromised Inforno could swap it.
  - Rollback: restoring an older store file brings back older approved
    configs whose approvals haven't expired. Short approval lifetimes bound
    it; autorno could also remember the highest approved version per Realm,
    though only in a locker, which expires.
  - When `realm_store_key` has expired, how does headless autorno get the
    user to tap the YubiKey: through Inforno, or a notification?
  - Store format: SQLite with encrypted rows, SQLCipher, or one encrypted
    file?
- **Limits:** are 16 × 256 KiB right as apps put more in lockers?
- **Windows:** a defence against same-user memory access (e.g. running the
  app as a separate low-rights user); environment and DLL checks; renamed
  executables.

---

## Appendix A. Deferred ideas

An earlier version of this design had much more in vaultorno. Each idea
below was taken out to keep it a small locker room; none is lost.

| Idea | Why taken out | Instead, for now | Would come back if |
|---|---|---|---|
| A configuration file (`/etc/vaultorno/policy.yml`) | Access belongs to apps, and the machine-wide settings weren't needed; without one, vaultorno has nothing to misconfigure and everything it does is printed by `vaultorno status` | Built-in constants; statements and manifests; `revoked_keys` as state (§5) | A machine needs a setting no statement can express, e.g. an admin requiring `hermetic` for every app |
| Separate executables for daemon, inspector and admin | One file is simpler to build, package and review as a whole; separation is between processes, not files | One `vaultorno` with modes; privileges from the units (§3) | — |
| YAML, and JSON outside messages and statements | Each format should be the simplest that does the job | JSON for messages, statements and manifests; OpenSSH key lines, fixed binary layouts and plain text elsewhere (§3) | — |
| Lasting data (configs, approvals, recovery material) | Vaultorno is for disposable data: lockers expire and have no backup or recovery | The app's own store with its own recovery, e.g. Inforno's realm store (§15), with at most a cached unlock in a locker | — |
| Structured entries (kinds, allowed hosts, expiry per entry, exportable flag) | Puts the app's meaning into the daemon | The app's own format inside lockers; expiry per locker | — |
| Partitions, spans, caps and roles in the daemon | Permissions can't be finer than what the daemon hands out: one locker is one boundary | Locker access in statements | An app needs per-entry permissions it can't get by splitting lockers |
| Delegation to child processes (passed connections, revocation, expiry) | Duplicates autorno's Realm enforcement; gives untrusted code a line to the daemon; a secret once read can't be revoked | autorno places secrets into Realms itself | A child must use a key it may not read (see non-exportable keys) |
| Approvals checked by the daemon | Protects little while autorno can read the secrets anyway; a blind daemon couldn't read what it approves | autorno verifies Realm approvals (§15) | Delegation comes back |
| Access to another app's vault | Needs a grant signed by the vault owner's key; no current user | — | The key-adding proxy below needs `api_keys` |
| Session entries | The app can keep temporary values in memory | App memory | — |
| `Watch` notifications | Revisions in `List` are enough | Polling `List` | Many apps with frequent changes |
| Non-exportable keys and operations (HMAC, Ed25519 signing, TOTP) | The one idea with no app-side equivalent: the key never leaves the daemon, even for the app. It can't live in the daemon now that it's blind: an operation needs the key | Nothing | Agents need to sign or authenticate without seeing a key. It would be a separate component with its own key, trusted the way the daemon no longer is |
| Key-adding proxy for agents | A separate app, not the daemon | — | A Realm needs an API without seeing its key; the proxy would be its own app, granted read access to `api_keys` |
| The `spans` crate in the daemon | Vaultorno has no patterns | Internal to Inforno and autorno | Delegation comes back |
| Per-key audit | The daemon sees lockers, not keys | Per-locker log | — |
| macOS per-user agent for delegation | There is no delegation | `Keychain` backend | — |

