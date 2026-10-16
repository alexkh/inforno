# Vaultorno: per-app, per-user secret storage

Design document for two new crates in the Inforno workspace:

- **`vaultorno`**: the daemon (root on Linux, a service on Windows) plus its
  admin command.
- **`vaultorno_client`**: the Rust library apps link to talk to the daemon.

Status: design, not yet implemented. The first user is Inforno
(`inforno_gui`, `autorno`, via `inforno_core`).

---

## 1. Why

Desktop Linux and Windows can encrypt secrets **per user** (`systemd-creds`,
DPAPI) but not **per app**: any program running as the user can decrypt
anything the user can. Only macOS (Keychain, for signed apps) and sandboxed
packages (Flatpak, MSIX) tell apps apart.

Vaultorno adds the missing per-app boundary, like a browser's per-site cookie
storage. Each app gets its own **vault** per user. Only that app's genuine
binaries can open it, even though they run as the same user as everything
else.

### Protects against

- Other programs running as the user (generic malware, scripts searching for
  API keys) reading an app's secrets.
- Code inside Inforno Realms (agents, their commands) reaching the secrets.
- Leaks of the stored files (backups, git, stolen disks): vaults are sealed
  to the machine (TPM2 when available).
- Other users on the machine.

### Does not protect against

- Root, or anyone who can change the policy file.
- Malware written specifically for one setup that wins a race, e.g. swapping
  a freshly built binary before the developer signs it (see §8).
- A compromised genuine app. Vaultorno vouches for the binary, not for what
  it does with the secrets once it has them.

---

## 2. Context in the Inforno workspace

Already in place, with tests passing on Linux and Windows:
`inforno_core::vault`.

- `Vault`: one map `name → SecretString`, stored as a single sealed blob.
  `load`, `save`, `get`, `set`, `remove`, `providers`. A missing file is an
  empty vault, and saving an empty vault deletes the file.
- `Sealer` trait with `description()`, `seal(name, data)`,
  `unseal(name, blob)`. `name` is bound into the blob.
  - `SystemdCreds`: `systemd-creds --user`, systemd 256 or newer, TPM2 or
    host key.
  - `Dpapi`: Windows, current user (`windows-sys` 0.61).
- `platform_sealer()` picks one per OS, `default_path()` gives the per-user
  file location, and `CREDENTIAL_NAME = "inforno-api-keys"`.
- `inforno_gui::key_manager` loads and saves the OpenRouter key through it,
  with a one-time move out of the system keyring.

Planned in Inforno, independent of vaultorno:

- A **`KeySource`** trait in front of secret storage, with two backends:
  - `LocalVault`: today's per-user `inforno_core::vault`. Used by
    development without vaultorno, and by Windows or Linux installs without
    the daemon.
  - `Vaultorno`: through `vaultorno_client`. Used when the daemon is
    installed.
- **Pairing each secret with its allowed hosts** (§6), so a changed config
  can't send a key to a foreign server.
- **Removing `.env` support and the `OPENROUTER_API_KEY` variable** once the
  vault is in daily use. `OPENROUTER_API_URLS` stays for now.

---

## 3. Crates

| Crate | Kind | Depends on | Used by |
|---|---|---|---|
| `vaultorno` | binaries: `vaultornod` (daemon), `vaultorno` (admin) | `vaultorno_client` (shared protocol types), minimal crypto, YAML, OS bindings | installed as root / as a service |
| `vaultorno_client` | library | `serde`, `serde_json`, `secrecy`, `thiserror`; nothing heavy | `inforno_core`, `autorno`, `vaultorno` |

Rules:

- `vaultorno` must **not** depend on `inforno_core` or any GUI crate.
  Everything in the daemon is attack surface; keep it small enough to review
  completely.
- The protocol types live in `vaultorno_client::protocol`. The daemon uses
  the same types, so the two can't drift apart.
- `vaultorno_client/examples/` may hold a small demo client, like the
  `yubitest` example.

---

## 4. Concepts

- **App**: a named set of binaries that share one vault per user. Example:
  `inforno` = `inforno` + `autorno`.
- **Vault**: one app's secrets for one user. Entries map a name to a secret
  plus optional metadata (§6).
- **Level**: a named bundle of identity checks. A caller must pass its app's
  level.
- **Policy**: the root-owned YAML file defining levels, apps, their
  binaries, keys, users and exceptions.

### Levels

From strictest to loosest; anything airtight is also watertight.

| Level | Identity | Checks |
|---|---|---|
| `hermetic` (later) | signature | airtight, plus a YubiKey touch for every write, TPM2 required (no host-key fallback) |
| `airtight` | signature: a valid signature by a registered key over the exact executable | no `LD_PRELOAD`/`LD_AUDIT`/`LD_LIBRARY_PATH`; `ptrace_scope >= 2` |
| `watertight` | path: a root-owned binary at a listed path (trusts the package manager) | binary and every parent directory owned by root and not group/world-writable; no preload variables; `ptrace_scope >= 1` |
| `open` | none (user ID only) | development only, always reported loudly |

---

## 5. Policy file

- **Location:** Linux `/etc/vaultorno/policy.yml`; Windows
  `%ProgramData%\vaultorno\policy.yml`.
- **Permissions:** readable by all, writable only by root / Administrators.
- **Fail closed:** if the file is missing, unreadable or invalid, refuse
  every request with an error that says why.
- **Parsing:** deserialize into typed structs (`serde_saphyr`, as Inforno
  already does for `realm2.yml`). Quote hashes and keys to avoid YAML's
  implicit typing.

```yaml
default_level: airtight

levels:                          # built-in definitions may be overridden here
  airtight:
    identity: signature
    reject_preload: true
    min_ptrace_scope: 2
  watertight:
    identity: path
    require_root_owned: true
    reject_preload: true
    min_ptrace_scope: 1

apps:
  inforno:
    level: airtight              # optional, defaults to default_level
    binaries: [inforno, autorno] # file names; with identity: path, absolute paths
    public_keys:
      - "ed25519:RWQ…"           # release signing key
      - "sk-ssh-ed25519@openssh.com AAAA…"   # developer's YubiKey (§8)
    users: [sv]                  # empty = every user (each still gets their own vault)
    exceptions:                  # escape hatch; listed by `vaultorno status`
      - binary: /opt/inforno-test/inforno
        identity: hash
        sha256: "…"
        expires: "2026-10-21"    # required; expired entries are ignored
        reason: "manual test build"
```

The goal is **no exceptions in normal use**: releases are signed with the
release key, development builds with the developer's YubiKey (§8).

---

## 6. Storage

### Vault files

| | Linux | Windows |
|---|---|---|
| Path | `/var/lib/vaultorno/users/<uid>/<app>.vault` | `%ProgramData%\vaultorno\users\<SID>\<app>.vault` |
| Permissions | root only, `0600`, directories `0700` | ACL: SYSTEM only |
| Sealing | system-level `systemd-creds encrypt` (TPM2 when present, else host key), credential name `vaultorno:<uid>:<app>` | DPAPI with `CRYPTPROTECT_LOCAL_MACHINE`, entropy `vaultorno:<SID>:<app>` |

- **Keyed by user ID / SID, never by user name:** IDs come from the
  kernel/OS and survive renames.
- **Writes are atomic:** temp file, sync, rename, as in
  `inforno_core::vault`.

### File format

A small JSON envelope, so the format and the sealer are visible without
decrypting:

```json
{ "version": 1, "sealer": "systemd-creds", "blob": "<base64>" }
```

Decrypted payload:

```json
{
  "entries": {
    "openrouter": { "value": "sk-or-…", "allowed_hosts": ["openrouter.ai"] },
    "endpoint:local-llm": { "value": "…", "allowed_hosts": ["10.0.0.5:8080"] }
  }
}
```

`allowed_hosts` is enforced by the **client app** (Inforno refuses to send a
key anywhere else). It's stored in the vault so that editing the app's normal
config can't redirect a key. Only a verified app can change entries.

---

## 7. Identifying the caller

### Linux

1. **Transport:** Unix socket `/run/vaultorno.sock`, mode `0666`. Access is
   decided per connection, not by file permissions.
2. **User and process:** `SO_PEERCRED` gives the user and process ID. Also
   take `SO_PEERPIDFD` (Linux 6.5+) so the check is pinned to that exact
   process. After inspecting the process, confirm through the pidfd that it's
   still alive, which rules out process-ID reuse. On older kernels, fall back
   to `pidfd_open(pid)` taken immediately after accept, with the same
   liveness check.
3. **User allowed:** must be listed in the app's `users`, or `users` is empty.
4. **Executable:** read `/proc/<pid>/exe`, opening the file through that link
   so it's the inode actually running, even if the path was replaced
   meanwhile. Hash it (SHA-256).
5. **Identity, per level:**
   - `signature`: find `<executable path>.sig`, verify it over the hash
     against the app's `public_keys`. Swapping the `.sig` file doesn't help an
     attacker; a valid signature is still required.
   - `path`: the executable path is in `binaries`, and it and all parent
     directories are root-owned and not writable by group or others.
   - `hash`: the hash matches an exception's `sha256`.
6. **Environment:** read `/proc/<pid>/environ`, the environment at exec
   time. Refuse if `LD_PRELOAD`, `LD_AUDIT` or `LD_LIBRARY_PATH` is set.
7. **Debugging protection:** read `/proc/sys/kernel/yama/ptrace_scope` and
   compare it with the level's minimum.
8. **App binding:** the binary decides which app the caller is. The app name
   in the request must match it; a binary can never open another app's vault.

The daemon needs root, or at least `CAP_SYS_PTRACE` and
`CAP_DAC_READ_SEARCH`, to inspect other users' processes in `/proc`.

### Windows

- **Transport:** named pipe `\\.\pipe\vaultorno`.
- **Process:** `GetNamedPipeClientProcessId`, then `OpenProcess` and
  `QueryFullProcessImageNameW` for the executable.
- **User:** impersonate the client (`ImpersonateNamedPipeClient`), read the
  token user SID, then revert.
- **Identity:**
  - `signature`: the same Ed25519/SSH signatures. Authenticode
    (`WinVerifyTrust`) can come later as an alternative.
  - `path`: under `Program Files`, with ACLs allowing writes only to
    Administrators, SYSTEM and TrustedInstaller.
- **DLL injection checks** (Windows' equivalent of preload): to be designed.
  Starting points are checking for unexpected modules and process
  mitigation policies.

### macOS (later)

No daemon. `vaultorno_client` stores vaults in the Keychain, which already
enforces per-app access for signed apps.

---

## 8. Signatures

- **Release builds:** Ed25519, minisign-style, with the release key kept
  offline.
- **Development builds:** OpenSSH signatures by a FIDO2 key on the
  developer's YubiKey (`sk-ssh-ed25519@openssh.com`). Each signature needs a
  touch, and the private key can't leave the YubiKey.

```sh
# once
ssh-keygen -t ed25519-sk -O application=ssh:inforno-signing -f ~/.ssh/inforno_sign_sk
# every build (wrapped in `cargo xtask build`)
ssh-keygen -Y sign -f ~/.ssh/inforno_sign_sk -n vaultorno target/release/inforno
#   → target/release/inforno.sig
```

- **Namespace:** always `vaultorno`, so these signatures can't be reused
  elsewhere.
- **The daemon verifies the signature over the executable's contents.**
  `ssh-keygen -Y sign` signs a hash of the file, so verifying against the
  hash computed in §7 step 4 is equivalent.
- **Key rotation:** `public_keys` holds several keys. A new release key is
  added before the old one is retired. Later: let an old key sign a new one,
  so installed policies can accept it automatically.

**Open item (milestone M0):** confirm a Rust crate (likely `ssh-key`) can
verify `sk-ssh-ed25519@openssh.com` signatures in the SSHSIG format. The
verification must also check the "user present" flag in the signature.

---

## 9. Protocol (`vaultorno_client::protocol`)

- **Framing:** 4-byte big-endian length, then a UTF-8 JSON message. Maximum
  1 MiB; the connection is closed on violation.
- **One request per frame.** A connection may carry many requests; identity
  is checked once, at the first request.

```rust
enum Request {
    Hello { protocol: u32, app: String },
    List,
    Get { name: String },
    Set { name: String, value: String, allowed_hosts: Vec<String> },
    Remove { name: String },
}

enum Response {
    Welcome { protocol: u32, level: String, sealer: String },
    Names(Vec<String>),
    Entry { value: String, allowed_hosts: Vec<String> },
    Done,
    Error { code: ErrorCode, message: String },
}

enum ErrorCode {
    ProtocolMismatch, NotAllowed, UnknownApp, NotFound,
    PolicyInvalid, Storage, TooLarge,
}
```

- `NotAllowed` messages must say which check failed (e.g. "binary not
  signed", "LD_PRELOAD set", "ptrace_scope is 1, level airtight needs 2").
  Users need that to fix their setup; it gives an attacker nothing new.
- **Client API:**

  ```rust
  let vault = vaultorno_client::Vault::open("inforno")?;  // Hello
  let names = vault.names()?;
  let entry = vault.get("openrouter")?;                   // Option<Entry>
  vault.set("openrouter", secret, &["openrouter.ai"])?;
  vault.remove("openrouter")?;
  ```

- Secrets are `SecretString` in the client API and zeroized on drop. Buffers
  holding a decoded response are zeroized after parsing.

---

## 10. Admin command (`vaultorno`)

| Command | Root | What it does |
|---|---|---|
| `vaultorno status` | no | Policy summary, levels, apps, active and expired exceptions, platform checks (`ptrace_scope`, TPM2 present, sealer in use). Also the input for Inforno's security report. |
| `vaultorno check <binary>` | no | Dry run: which app and level the binary would get, and which check fails. |
| `vaultorno pin <binary> --days N` | yes | Add a hash exception with an expiry. |
| `vaultorno trust-key <app> <public key>` | yes | Add a signing key to an app. |
| `vaultorno forget <app> [--user U]` | yes | Delete vaults. |

The admin command is not an app identity. It can't read any vault's
contents, only list app names and sizes.

---

## 11. Installation (Linux)

- **Units:** `vaultorno.socket` (`ListenStream=/run/vaultorno.sock`,
  `SocketMode=0666`) and `vaultorno.service`, socket-activated.
- **Hardening:** `ProtectSystem=strict`, `ReadWritePaths=/var/lib/vaultorno`,
  `ProtectHome=read-only` (it must read executables under `/home` for signed
  development builds), `PrivateTmp`, `NoNewPrivileges`, `RestrictAddressFamilies=AF_UNIX`,
  `SystemCallFilter=@system-service`, and a capability bounding set limited
  to what §7 needs.
- **Packaging:** an Arch PKGBUILD first, installing `/usr/bin/inforno`,
  `/usr/bin/autorno`, `/usr/bin/vaultorno`, `/usr/lib/vaultorno/vaultornod`,
  the units, and a default policy with Inforno's release key.

---

## 12. Integration in Inforno

1. Introduce `KeySource` in `inforno_core`, with the methods
   `get/set/remove/names` and `describe()` for the security report. Move the
   current `key_manager` storage functions behind it, as `LocalVault`.
2. Add the `Vaultorno` backend. Pick it at startup if the socket exists and
   `Hello` succeeds. Otherwise fall back to `LocalVault`, and say so in the
   key manager and the security report.
3. One-time migration: entries from the local vault move into the vaultorno
   vault, and the local file is deleted afterwards.
4. Enforce `allowed_hosts` wherever Inforno sends a key (OpenRouter client,
   custom endpoints).
5. Realms: the vaultorno socket and `/var/lib/vaultorno` must never be
   visible inside a Realm. Add a test for that. Even if they were, a Realm
   binary would fail identity.

---

## 13. Milestones

| | What | Done when |
|---|---|---|
| M0 | Spike: verify YubiKey `ssh-keygen -Y sign` signatures in Rust | Example program verifies a real signature, rejects tampered files and the wrong namespace |
| M1 | `vaultorno_client` protocol + `Vault` API; daemon skeleton with the `open` level (user ID only) and plain files | Round trip from an example client; per-user separation tested |
| M2 | Policy file, levels, path and hash identity, environment and ptrace checks | Integration tests with a test policy and test binaries in temp dirs |
| M3 | Signature identity + `cargo xtask build` signing flow | Signed development build of Inforno opens its vault with no exceptions |
| M4 | Sealing with system-level `systemd-creds`, envelope format | Vault files unreadable without the daemon's machine |
| M5 | Inforno `KeySource` + Vaultorno backend + migration + `allowed_hosts` | Inforno runs entirely on vaultorno on Linux |
| M6 | systemd units, hardening, PKGBUILD, `vaultorno status/check/pin` | Clean install on Arch from the package |
| M7 | Windows service, named pipe, DPAPI machine scope | Same tests pass on Windows |
| M8 | macOS Keychain backend in the client | — |

## 14. Testing

- Unit tests with a fake sealer, as in `inforno_core::vault`.
- **Daemon integration tests:** run the daemon in-process on a socket in a
  temp directory, with a test policy and a test vault directory. The
  "binaries" are small test executables, signed with test keys generated in
  the test.
- **Identity checks get their own tests:** wrong signature, wrong namespace,
  tampered binary, `LD_PRELOAD` set, user not listed, expired exception,
  missing or invalid policy (fail closed).
- **Real-hardware tests** (TPM, YubiKey) are `#[ignore]` and run manually,
  like `real_systemd_creds` and `real_dpapi` today.

## 15. Open questions

- **Process-ID reuse on older kernels:** is the `pidfd_open` fallback good
  enough, or should the daemon require Linux 6.5+ (`SO_PEERPIDFD`)?
- **Shared vaults across apps:** should a vault ever be shared between
  apps? Current answer: no. An app is the unit; list both binaries in one app
  instead.
- **Backup and restore:** vault files are machine-bound. Should there be an
  explicit export to a passphrase-encrypted file? API keys are cheap to
  re-issue, so this is probably not needed.
- **Hermetic level:** how does a YubiKey touch for writes work? Through the
  daemon, which then needs FIDO2 access, or by having the app pass a signed
  approval?
- **Windows DLL-injection checks** (§7).
