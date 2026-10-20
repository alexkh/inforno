//! Signing keys (design doc §5, §6, §8): SSHSIG verification of statements,
//! the key fingerprints that name vaults, and the two key lists in the state
//! folder, `revoked_keys` and `successions`.
//!
//! Only `ssh-ed25519` and `sk-ssh-ed25519@openssh.com` keys sign statements.
//! For security keys, a signature counts only with the "user present" flag.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use rustix::fs::{
    AtFlags, Gid, Mode, OFlags, Uid, fchown, fstat, fsync, open, openat, renameat, unlinkat,
};
use ssh_key::{Algorithm, HashAlg, PublicKey, SshSig};

/// SSHSIG namespace of statements (§8).
pub const STATEMENT_NAMESPACE: &str = "vaultorno/statement";
/// SSHSIG namespace of succession statements (§8). Distinct from statements',
/// so neither kind of signature can stand in for the other.
pub const SUCCESSION_NAMESPACE: &str = "vaultorno/succession";
/// Refused signing keys, one OpenSSH public key per line (§6).
pub const REVOKED_KEYS: &str = "revoked_keys";
/// Key rotations, one per line: the old key's fingerprint, then the new key
/// (§6, §8).
pub const SUCCESSIONS: &str = "successions";

/// The "user present" bit in a security key's signature flags (§8).
const SK_USER_PRESENT: u8 = 0x01;
/// Length of an Ed25519 signature, after which a security key's flags follow.
const ED25519_SIGNATURE_LEN: usize = 64;

/// The SHA-256 fingerprint of a signing key. Its hex form names the key's
/// vault (§6).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyFingerprint(pub [u8; 32]);

impl KeyFingerprint {
    pub fn of(key: &PublicKey) -> KeyFingerprint {
        KeyFingerprint(
            key.fingerprint(HashAlg::Sha256)
                .sha256()
                .expect("a SHA-256 fingerprint has 32 bytes"),
        )
    }

    /// Parses `SHA256:<base64>`, as `ssh-keygen -l` prints it.
    pub fn parse(text: &str) -> Option<KeyFingerprint> {
        ssh_key::Fingerprint::from_str(text)
            .ok()?
            .sha256()
            .map(KeyFingerprint)
    }

    /// Lowercase hex: the vault name (§6).
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
        }
        s
    }

    /// The first 16 hex digits, where a shorter form helps people (§6).
    pub fn key_id(&self) -> String {
        self.hex()[..16].to_owned()
    }
}

impl fmt::Display for KeyFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&ssh_key::Fingerprint::Sha256(self.0), f)
    }
}

impl fmt::Debug for KeyFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Who signed something that verified.
#[derive(Debug, Clone)]
pub struct SignedBy {
    pub key: PublicKey,
    pub fingerprint: KeyFingerprint,
}

/// Verifies an armored SSHSIG (`-----BEGIN SSH SIGNATURE-----`) over the
/// exact bytes of `message` for `namespace`. The signature carries its own
/// key; the caller decides what that key may do (§8).
pub fn verify_signature(
    namespace: &str,
    message: &[u8],
    armored: &str,
) -> Result<SignedBy, String> {
    let sig = SshSig::from_pem(armored.trim()).map_err(|e| format!("not an SSH signature: {e}"))?;
    let key = PublicKey::from(sig.public_key().clone());
    let algorithm = key.algorithm();
    if !matches!(algorithm, Algorithm::Ed25519 | Algorithm::SkEd25519) {
        return Err(format!(
            "{} keys can't sign statements; use ssh-ed25519 or sk-ssh-ed25519@openssh.com",
            algorithm.as_str()
        ));
    }
    if sig.algorithm() != algorithm {
        return Err("the signature's algorithm doesn't match its key".into());
    }
    if sig.namespace() != namespace {
        return Err(format!(
            "signature namespace is {:?}, not {namespace:?}",
            sig.namespace()
        ));
    }
    key.verify(namespace, message, &sig)
        .map_err(|_| "bad signature".to_owned())?;
    if algorithm == Algorithm::SkEd25519 {
        // The signature covers the authenticator's flags, but the library
        // doesn't look at them: a signature made without a touch is refused
        // here (§8).
        let flags = sig
            .signature_bytes()
            .get(ED25519_SIGNATURE_LEN)
            .copied()
            .unwrap_or(0);
        if flags & SK_USER_PRESENT == 0 {
            return Err("the security key signed without user presence (no touch)".into());
        }
    }
    let fingerprint = KeyFingerprint::of(&key);
    Ok(SignedBy { key, fingerprint })
}

/// Parses one OpenSSH public key line: type, base64 and optional comment.
pub fn parse_key_line(line: &str) -> Result<PublicKey, String> {
    PublicKey::from_openssh(line.trim()).map_err(|e| format!("not an OpenSSH public key: {e}"))
}

/// Lines that carry content: not blank, not starting with `#` (§5). Numbered
/// from 1.
fn content_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
}

// ---------------------------------------------------------------------------
// Key lists

/// `revoked_keys` and `successions`, parsed.
#[derive(Debug, Default)]
pub struct KeyLists {
    revoked: HashSet<KeyFingerprint>,
    /// New key → the key it succeeded.
    predecessor: HashMap<KeyFingerprint, KeyFingerprint>,
}

impl KeyLists {
    /// Parses both files' text. Any line that doesn't parse is an error: the
    /// daemon then refuses everything rather than guess (§5).
    pub fn parse(revoked_keys: &str, successions: &str) -> Result<KeyLists, String> {
        let mut lists = KeyLists::default();
        for (n, line) in content_lines(revoked_keys) {
            let key = parse_key_line(line).map_err(|e| format!("{REVOKED_KEYS} line {n}: {e}"))?;
            lists.revoked.insert(KeyFingerprint::of(&key));
        }
        for (n, line) in content_lines(successions) {
            let bad = |what: &str| format!("{SUCCESSIONS} line {n}: {what}");
            let (from, to) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| bad("expected a fingerprint, then a public key"))?;
            let from = KeyFingerprint::parse(from)
                .ok_or_else(|| bad(&format!("{from:?} isn't a SHA256: fingerprint")))?;
            let to = KeyFingerprint::of(&parse_key_line(to).map_err(|e| bad(&e))?);
            if to == from {
                return Err(bad("a key can't succeed itself"));
            }
            match lists.predecessor.insert(to, from) {
                Some(earlier) if earlier != from => {
                    return Err(bad(&format!("{to} already succeeds {earlier}")));
                }
                _ => {}
            }
        }
        // Every line of successions must lead back to a first key.
        for start in lists.predecessor.keys() {
            lists
                .first_of(start)
                .ok_or_else(|| format!("{SUCCESSIONS} has a cycle through {start}"))?;
        }
        Ok(lists)
    }

    pub fn is_revoked(&self, key: &KeyFingerprint) -> bool {
        self.revoked.contains(key)
    }

    /// The key `key` succeeded, if a succession is recorded for it.
    pub fn predecessor_of(&self, key: &KeyFingerprint) -> Option<KeyFingerprint> {
        self.predecessor.get(key).copied()
    }

    /// Whether `to` succeeding `from` fits the recorded lines (§8): `Ok(true)`
    /// if it can be added, `Ok(false)` if it's recorded already. A key
    /// succeeds at most one other, and no line may close into a cycle.
    pub fn check_new_succession(
        &self,
        from: &KeyFingerprint,
        to: &KeyFingerprint,
    ) -> Result<bool, String> {
        if from == to {
            return Err("a key can't succeed itself".into());
        }
        match self.predecessor.get(to) {
            Some(prev) if prev == from => return Ok(false),
            Some(prev) => return Err(format!("{to} already succeeds {prev}")),
            None => {}
        }
        // Parsed lists have no cycles, so this walk ends.
        let mut cur = *from;
        while let Some(prev) = self.predecessor.get(&cur) {
            if prev == to {
                return Err(format!("{to} comes before {from} already"));
            }
            cur = *prev;
        }
        Ok(true)
    }

    /// Keys whose line of successions passes through `key`: they open the
    /// vault it opens, whatever happens to `key` itself (§8).
    pub fn successors_of(&self, key: &KeyFingerprint) -> Vec<KeyFingerprint> {
        let mut out: Vec<KeyFingerprint> = self
            .predecessor
            .keys()
            .filter(|start| {
                let mut cur = **start;
                while let Some(prev) = self.predecessor.get(&cur) {
                    if prev == key {
                        return true;
                    }
                    cur = *prev;
                }
                false
            })
            .copied()
            .collect();
        out.sort_by_key(|k| k.0);
        out
    }

    /// The vault a key opens: the first key's in its line of successions,
    /// or its own (§8).
    pub fn vault_of(&self, key: &KeyFingerprint) -> KeyFingerprint {
        self.first_of(key).expect("cycles are refused when parsing")
    }

    fn first_of(&self, key: &KeyFingerprint) -> Option<KeyFingerprint> {
        let mut cur = *key;
        for _ in 0..=self.predecessor.len() {
            match self.predecessor.get(&cur) {
                Some(prev) => cur = *prev,
                None => return Some(cur),
            }
        }
        None
    }
}

/// Largest key list read, so a planted special file can't stall a reader.
const MAX_LIST_BYTES: u64 = 1024 * 1024;

/// The state folder, opened once so every file in it is reached relative to
/// the same folder.
fn open_dir(dir: &Path) -> Result<OwnedFd, String> {
    open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| format!("can't open {}: {e}", dir.display()))
}

/// Reads `name` in `dir`: a regular file with one link, never through a
/// symlink, at most [`MAX_LIST_BYTES`]. `None` if it doesn't exist.
/// `vaultorno revoke` runs as root in a folder another user owns, so it
/// mustn't be steered into reading another file (§12).
fn read_list_at(dir: &OwnedFd, name: &str) -> Result<Option<String>, String> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = match openat(dir, name, flags, Mode::empty()) {
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(e) => return Err(format!("can't open {name}: {e}")),
        Ok(fd) => fd,
    };
    let file = File::from(fd);
    let meta = file
        .metadata()
        .map_err(|e| format!("can't stat {name}: {e}"))?;
    if !meta.file_type().is_file() || meta.nlink() != 1 {
        return Err(format!("{name} isn't a regular file with one link"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_LIST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("can't read {name}: {e}"))?;
    if bytes.len() as u64 > MAX_LIST_BYTES {
        return Err(format!("{name} is larger than {MAX_LIST_BYTES} bytes"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{name} isn't UTF-8"))
}

/// The key lists in the state folder. Both files are small, so they are
/// reread at every `Hello`: root may edit them by hand, and any change
/// applies to the next connection (§6).
pub struct KeyListFiles {
    dir: PathBuf,
}

impl KeyListFiles {
    pub fn new(state_dir: impl Into<PathBuf>) -> KeyListFiles {
        KeyListFiles {
            dir: state_dir.into(),
        }
    }

    /// The current lists. A missing file is an empty list; an unreadable
    /// file or a line that doesn't parse is an error, so every request is
    /// refused until it's fixed (§5, §9).
    pub fn load(&self) -> Result<KeyLists, String> {
        let refuse = |e: String| format!("{e}; refusing every request until it's fixed");
        let dir = open_dir(&self.dir).map_err(refuse)?;
        let revoked = read_list_at(&dir, REVOKED_KEYS).map_err(refuse)?;
        let successions = read_list_at(&dir, SUCCESSIONS).map_err(refuse)?;
        KeyLists::parse(
            revoked.as_deref().unwrap_or(""),
            successions.as_deref().unwrap_or(""),
        )
        .map_err(refuse)
    }

    /// Appends `to` succeeding `from` to `successions` (§8), after a comment
    /// line with `note`, unless it's there already. Returns whether the file
    /// changed. The caller serialises writers; root editing the file by hand
    /// at the same moment can still lose one of the two edits.
    pub fn record_succession(
        &self,
        from: &KeyFingerprint,
        to: &PublicKey,
        note: &str,
    ) -> Result<bool, String> {
        let dir = open_dir(&self.dir)?;
        let text = read_list_at(&dir, SUCCESSIONS)?.unwrap_or_default();
        let to_fingerprint = KeyFingerprint::of(to);
        if !KeyLists::parse("", &text)?.check_new_succession(from, &to_fingerprint)? {
            return Ok(false);
        }
        let line = to
            .to_openssh()
            .map_err(|e| format!("can't write the key: {e}"))?;
        let mut out = text;
        if out.trim().is_empty() {
            out = "# successions: the old key's fingerprint, then the new key as in a .pub file\n"
                .to_owned();
        } else if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("# {note}\n{from} {}\n", line.trim()));
        // What's written must read back as exactly this one more line.
        if KeyLists::parse("", &out)?.predecessor_of(&to_fingerprint) != Some(*from) {
            return Err("the new line doesn't read back as written".into());
        }
        replace_file(&dir, SUCCESSIONS, out.as_bytes())
            .map_err(|e| format!("can't write {SUCCESSIONS}: {e}"))?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// `vaultorno revoke`

/// Adds `key` to `revoked_keys` in `state_dir` or, with `undo`, removes it
/// (§12). Comments and other lines are kept. Returns whether the file
/// changed. Refuses to edit a list that doesn't parse: the daemon refuses
/// everything until it's fixed by hand.
pub fn edit_revoked_keys(state_dir: &Path, key: &PublicKey, undo: bool) -> Result<bool, String> {
    let dir = open_dir(state_dir)?;
    let existing = read_list_at(&dir, REVOKED_KEYS)?;
    let text = existing.as_deref().unwrap_or("");
    KeyLists::parse(text, "").map_err(|e| format!("{e}; fix it by hand first"))?;

    let target = KeyFingerprint::of(key);
    let mut found = false;
    let mut out = String::with_capacity(text.len() + 128);
    for line in text.lines() {
        let trimmed = line.trim();
        let is_target = !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && parse_key_line(trimmed).is_ok_and(|k| KeyFingerprint::of(&k) == target);
        found |= is_target;
        if is_target && undo {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if found != undo {
        return Ok(false);
    }
    if !undo {
        if existing.is_none() {
            out.push_str(
                "# revoked_keys: one OpenSSH public key per line, as in sshd's RevokedKeys\n",
            );
        }
        let line = key
            .to_openssh()
            .map_err(|e| format!("can't write the key: {e}"))?;
        out.push_str(line.trim());
        out.push('\n');
    }
    replace_file(&dir, REVOKED_KEYS, out.as_bytes())
        .map_err(|e| format!("can't write {REVOKED_KEYS}: {e}"))?;
    Ok(true)
}

/// Replaces `name` in `dir` atomically with a file owned like the folder,
/// mode `0600` (§6): the daemon can read it even when root wrote it, and
/// nothing about the old file (which another user could have swapped) is
/// copied.
fn replace_file(dir: &OwnedFd, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = format!("{name}.tmp");
    // Never follow a planted link: remove a leftover, then create exclusively.
    match unlinkat(dir, tmp.as_str(), AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(e) => return Err(e.into()),
    }
    let fd = openat(
        dir,
        tmp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let dir_stat = fstat(dir)?;
    let mut f = File::from(fd);
    let meta = f.metadata()?;
    if (meta.uid(), meta.gid()) != (dir_stat.st_uid, dir_stat.st_gid) {
        fchown(
            &f,
            Some(Uid::from_raw(dir_stat.st_uid)),
            Some(Gid::from_raw(dir_stat.st_gid)),
        )?;
    }
    f.set_permissions(Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    renameat(dir, tmp.as_str(), dir, name)?;
    fsync(dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::private::Ed25519Keypair;
    use ssh_key::{LineEnding, PrivateKey, Signature, SigningKey};
    use std::fs;

    /// Made with `ssh-keygen -t ed25519` and `ssh-keygen -Y sign -n
    /// vaultorno/statement` (OpenSSH 9.6, default hash SHA-512) over
    /// `{"hello":1}\n`.
    const REAL_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAGf35LBmeMKO1jMA9oUql/3dGOKq+QS9ISbJkDJzoRu vaultorno_test_key";
    const REAL_FINGERPRINT: &str = "SHA256:er8u9VfNuR0xkLRM0QgzXCYQOcmcBhHaj1lAkDj0+fA";
    const REAL_MESSAGE: &[u8] = b"{\"hello\":1}\n";
    const REAL_SIGNATURE: &str = "\
-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgAZ/fksGZ4wo7WMwD2hSqX/d0Y4
qr5BL0hJsmQMnOhG4AAAATdmF1bHRvcm5vL3N0YXRlbWVudAAAAAAAAAAGc2hhNTEyAAAA
UwAAAAtzc2gtZWQyNTUxOQAAAEAe6UVP+5Con48967+eF839hRhs3ToKv7SQC37KjF1v9L
UHKTwZxswzQY8kR3CW1chH+YdvxuHvS7ieLDA+8WIL
-----END SSH SIGNATURE-----
";

    fn key(seed: u8) -> PrivateKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
    }

    fn sign(key: &PrivateKey, namespace: &str, msg: &[u8]) -> String {
        SshSig::sign(key, namespace, HashAlg::Sha256, msg)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap()
    }

    fn raw_sign<S: SigningKey>(signer: &S, data: &[u8]) -> Signature {
        signer.try_sign(data).unwrap()
    }

    /// What a security key would produce: it signs SHA-256(application) ‖
    /// flags ‖ counter ‖ SHA-256(data) with an Ed25519 key.
    fn sk_sign(seed: u8, flags: u8, msg: &[u8]) -> (PublicKey, String) {
        use ssh_key::sha2::{Digest, Sha256};
        let inner = Ed25519Keypair::from_seed(&[seed; 32]);
        let public = ssh_key::public::SkEd25519::new(inner.public, "ssh:vaultorno_signing");
        let data = SshSig::signed_data(STATEMENT_NAMESPACE, HashAlg::Sha512, msg).unwrap();
        let counter = 7u32.to_be_bytes();
        let mut sk_data = Sha256::digest(public.application()).to_vec();
        sk_data.push(flags);
        sk_data.extend_from_slice(&counter);
        sk_data.extend_from_slice(&Sha256::digest(&data));
        let ed = raw_sign(&PrivateKey::from(inner), &sk_data);
        let mut bytes = ed.as_bytes().to_vec();
        bytes.push(flags);
        bytes.extend_from_slice(&counter);
        let signature = Signature::new(Algorithm::SkEd25519, bytes).unwrap();
        let key_data = ssh_key::public::KeyData::from(public);
        let sig = SshSig::new(
            key_data.clone(),
            STATEMENT_NAMESPACE,
            HashAlg::Sha512,
            signature,
        )
        .unwrap();
        (
            PublicKey::from(key_data),
            sig.to_pem(LineEnding::LF).unwrap(),
        )
    }

    #[test]
    fn verifies_a_real_ssh_keygen_signature() {
        let by = verify_signature(STATEMENT_NAMESPACE, REAL_MESSAGE, REAL_SIGNATURE).unwrap();
        assert_eq!(by.fingerprint.to_string(), REAL_FINGERPRINT);
        assert_eq!(
            by.fingerprint,
            KeyFingerprint::of(&parse_key_line(REAL_KEY).unwrap())
        );
        assert_eq!(
            KeyFingerprint::parse(REAL_FINGERPRINT),
            Some(by.fingerprint)
        );
        assert_eq!(by.fingerprint.hex().len(), 64);
        assert_eq!(by.fingerprint.key_id(), by.fingerprint.hex()[..16]);

        assert_eq!(
            verify_signature(STATEMENT_NAMESPACE, b"{\"hello\":2}\n", REAL_SIGNATURE).unwrap_err(),
            "bad signature"
        );
        assert!(
            verify_signature("file", REAL_MESSAGE, REAL_SIGNATURE)
                .unwrap_err()
                .contains("namespace")
        );
        assert!(
            verify_signature(STATEMENT_NAMESPACE, REAL_MESSAGE, "garbage")
                .unwrap_err()
                .contains("not an SSH signature")
        );
    }

    #[test]
    fn security_key_signatures_need_user_presence() {
        let msg = b"{\"statement\":true}";
        let (public, touched) = sk_sign(9, SK_USER_PRESENT, msg);
        let by = verify_signature(STATEMENT_NAMESPACE, msg, &touched).unwrap();
        assert_eq!(by.fingerprint, KeyFingerprint::of(&public));
        assert!(verify_signature(STATEMENT_NAMESPACE, b"other", &touched).is_err());

        let (_, untouched) = sk_sign(9, 0x04, msg);
        assert!(
            verify_signature(STATEMENT_NAMESPACE, msg, &untouched)
                .unwrap_err()
                .contains("user presence")
        );
    }

    #[test]
    fn plain_ed25519_signatures_verify_with_either_hash() {
        let k = key(1);
        let msg = b"statement bytes";
        let sig = sign(&k, STATEMENT_NAMESPACE, msg);
        let by = verify_signature(STATEMENT_NAMESPACE, msg, &sig).unwrap();
        assert_eq!(by.fingerprint, KeyFingerprint::of(k.public_key()));
        let sha512 = SshSig::sign(&k, STATEMENT_NAMESPACE, HashAlg::Sha512, msg)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        assert!(verify_signature(STATEMENT_NAMESPACE, msg, &sha512).is_ok());
    }

    fn line(k: &PrivateKey, comment: &str) -> String {
        let mut public = k.public_key().clone();
        public.set_comment(comment);
        public.to_openssh().unwrap()
    }

    fn fp(k: &PrivateKey) -> KeyFingerprint {
        KeyFingerprint::of(k.public_key())
    }

    #[test]
    fn key_lists_parse_comments_and_successions() {
        let (a, b, c) = (key(1), key(2), key(3));
        let revoked = format!(
            "# revoked\n\n{}\n   \n",
            line(&a, "inforno_release_2026 stolen 2026-11-02")
        );
        let successions = format!(
            "# successions\n{} {}\n{} {}\n",
            fp(&a),
            line(&b, "release_2027"),
            fp(&b),
            line(&c, "release_2028")
        );
        let lists = KeyLists::parse(&revoked, &successions).unwrap();
        assert!(lists.is_revoked(&fp(&a)));
        assert!(!lists.is_revoked(&fp(&b)));
        assert_eq!(
            lists.vault_of(&fp(&c)),
            fp(&a),
            "a chain leads to the first key"
        );
        assert_eq!(lists.vault_of(&fp(&b)), fp(&a));
        assert_eq!(lists.vault_of(&fp(&a)), fp(&a));
        assert_eq!(lists.vault_of(&fp(&key(4))), fp(&key(4)));
    }

    #[test]
    fn key_lists_refuse_what_they_cant_parse() {
        let (a, b) = (key(1), key(2));
        let err = KeyLists::parse("# ok\nssh-ed25519 notbase64\n", "").unwrap_err();
        assert!(err.starts_with("revoked_keys line 2:"), "{err}");
        assert!(KeyLists::parse("", &line(&b, "no fingerprint")).is_err());
        assert!(KeyLists::parse("", &format!("SHA256:nope {}", line(&b, ""))).is_err());
        assert!(KeyLists::parse("", &format!("{} {}", fp(&b), line(&b, ""))).is_err());
        let conflict = format!(
            "{} {}\n{} {}\n",
            fp(&a),
            line(&b, ""),
            fp(&key(3)),
            line(&b, "")
        );
        assert!(
            KeyLists::parse("", &conflict)
                .unwrap_err()
                .contains("already succeeds")
        );
        let cycle = format!("{} {}\n{} {}\n", fp(&a), line(&b, ""), fp(&b), line(&a, ""));
        assert!(KeyLists::parse("", &cycle).unwrap_err().contains("cycle"));
    }

    #[test]
    fn key_lists_take_any_openssh_key_type() {
        // sshd's RevokedKeys holds keys of every type; none of these can
        // sign a statement, but a list holding them must still parse.
        let ecdsa = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBLb1PRZa3LSkY4Py4yxG602+P58VPZvwjKSVlhyH+Dy4QbFD3o2aN4loBFkPkXryYu3FYpvKjKEG3jeZzwKrhww= ecdsa_test";
        let rsa = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQC9JFaAylP+RgYuq7N8z9z2VktixlVhDMykIAROf4JLDPDgVCkrFtlbl1EJftDPjF5IIlCmiDMKR407qDnasN/E+FiNmMT6FdMUl65uxJsUSvRF5QOtkthkFqo0PrA0wjKHUcbS2JDN9Z1NxXYd83A5pFO4mUHf9WIO+Ls1Fli9Kw== rsa_test";
        let lists = KeyLists::parse(&format!("{ecdsa}\n{rsa}\n"), "").unwrap();
        assert!(lists.is_revoked(&KeyFingerprint::of(&parse_key_line(ecdsa).unwrap())));
        assert!(lists.is_revoked(&KeyFingerprint::of(&parse_key_line(rsa).unwrap())));
    }

    #[test]
    fn key_list_files_refuse_links_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, line(&key(1), "x") + "\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(REVOKED_KEYS)).unwrap();
        let files = KeyListFiles::new(dir.path());
        assert!(files.load().is_err(), "a symlink isn't followed");
        assert!(edit_revoked_keys(dir.path(), key(2).public_key(), false).is_err());

        fs::remove_file(dir.path().join(REVOKED_KEYS)).unwrap();
        fs::hard_link(&target, dir.path().join(REVOKED_KEYS)).unwrap();
        assert!(files.load().unwrap_err().contains("one link"));
        assert!(edit_revoked_keys(dir.path(), key(2).public_key(), false).is_err());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            line(&key(1), "x") + "\n",
            "the linked file is left alone"
        );
    }

    #[test]
    fn key_list_files_reload_on_change_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let files = KeyListFiles::new(dir.path());
        assert!(!files.load().unwrap().is_revoked(&fp(&key(1))));
        fs::write(dir.path().join(REVOKED_KEYS), line(&key(1), "x") + "\n").unwrap();
        assert!(files.load().unwrap().is_revoked(&fp(&key(1))));
        fs::write(dir.path().join(REVOKED_KEYS), "not a key\n").unwrap();
        assert!(files.load().unwrap_err().contains("refusing every request"));
        assert!(files.load().is_err(), "an error isn't cached as good");
        fs::write(dir.path().join(REVOKED_KEYS), "").unwrap();
        assert!(!files.load().unwrap().is_revoked(&fp(&key(1))));
    }

    #[test]
    fn revoke_appends_keeps_comments_and_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (key(1), key(2));
        let path = dir.path().join(REVOKED_KEYS);

        assert!(edit_revoked_keys(dir.path(), a.public_key(), false).unwrap());
        assert!(
            !edit_revoked_keys(dir.path(), a.public_key(), false).unwrap(),
            "already there"
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("# a note root wrote\n");
        fs::write(&path, &text).unwrap();
        assert!(edit_revoked_keys(dir.path(), b.public_key(), false).unwrap());

        let lists = KeyLists::parse(&fs::read_to_string(&path).unwrap(), "").unwrap();
        assert!(lists.is_revoked(&fp(&a)) && lists.is_revoked(&fp(&b)));

        assert!(edit_revoked_keys(dir.path(), a.public_key(), true).unwrap());
        assert!(
            !edit_revoked_keys(dir.path(), a.public_key(), true).unwrap(),
            "already gone"
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# a note root wrote"));
        let lists = KeyLists::parse(&text, "").unwrap();
        assert!(!lists.is_revoked(&fp(&a)) && lists.is_revoked(&fp(&b)));

        fs::write(&path, "garbage\n").unwrap();
        assert!(edit_revoked_keys(dir.path(), a.public_key(), false).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "garbage\n",
            "left as it was"
        );
    }

    #[test]
    fn successions_are_recorded_once_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let files = KeyListFiles::new(dir.path());
        let (a, b, c) = (key(1), key(2), key(3));
        assert!(files.record_succession(&fp(&a), b.public_key(), "first").unwrap());
        assert!(!files.record_succession(&fp(&a), b.public_key(), "again").unwrap());
        assert!(files.record_succession(&fp(&b), c.public_key(), "second").unwrap());

        let lists = files.load().unwrap();
        assert_eq!(lists.vault_of(&fp(&c)), fp(&a));
        let mut expected = vec![fp(&b), fp(&c)];
        expected.sort_by_key(|k| k.0);
        assert_eq!(lists.successors_of(&fp(&a)), expected);
        assert!(lists.successors_of(&fp(&c)).is_empty());

        let err = files
            .record_succession(&fp(&key(4)), b.public_key(), "x")
            .unwrap_err();
        assert!(err.contains("already succeeds"), "{err}");
        let err = files
            .record_succession(&fp(&c), a.public_key(), "x")
            .unwrap_err();
        assert!(err.contains("comes before"), "a cycle: {err}");

        let text = fs::read_to_string(dir.path().join(SUCCESSIONS)).unwrap();
        assert_eq!(text.lines().filter(|l| l.starts_with("SHA256:")).count(), 2);
        assert!(text.contains("# first\n") && !text.contains("# again"));
        assert_eq!(
            fs::metadata(dir.path().join(SUCCESSIONS)).unwrap().mode() & 0o777,
            0o600
        );
    }

    /// A real YubiKey (§8): the development key's
    /// `sk-ssh-ed25519@openssh.com` signature verifies, and one by a key
    /// made with `-O no-touch-required` is refused. Make the files with the
    /// commands below; the second key is for this test only, and signing
    /// with it must happen without a touch:
    ///
    /// ```sh
    /// ssh-keygen -t ed25519-sk -O application=ssh:vaultorno_signing -f ~/.ssh/vaultorno_sign_sk
    /// ssh-keygen -t ed25519-sk -O application=ssh:vaultorno_no_touch -O no-touch-required \
    ///     -f /tmp/vaultorno_no_touch_sk
    /// printf '{"format":1}\n' > /tmp/yk_message.json
    /// ssh-keygen -Y sign -f ~/.ssh/vaultorno_sign_sk -n vaultorno/statement \
    ///     < /tmp/yk_message.json > /tmp/yk_touch.sig        # touch the key
    /// ssh-keygen -Y sign -f /tmp/vaultorno_no_touch_sk -n vaultorno/statement \
    ///     < /tmp/yk_message.json > /tmp/yk_no_touch.sig     # don't touch
    ///
    /// VAULTORNO_YK_MESSAGE=/tmp/yk_message.json VAULTORNO_YK_SIG=/tmp/yk_touch.sig \
    /// VAULTORNO_YK_NO_TOUCH_SIG=/tmp/yk_no_touch.sig \
    /// VAULTORNO_YK_PUB=$HOME/.ssh/vaultorno_sign_sk.pub \
    ///     cargo test --release -p vaultorno --lib -- --ignored real_yubikey
    /// ```
    ///
    /// `VAULTORNO_YK_PUB` is optional; when set, the signature must be by
    /// that key.
    #[test]
    #[ignore]
    fn real_yubikey_signatures() {
        let path = |var: &str| {
            PathBuf::from(std::env::var_os(var).unwrap_or_else(|| panic!("{var} isn't set")))
        };
        let read = |var: &str| fs::read_to_string(path(var)).unwrap();
        let message = fs::read(path("VAULTORNO_YK_MESSAGE")).unwrap();
        let touched = read("VAULTORNO_YK_SIG");

        let by = verify_signature(STATEMENT_NAMESPACE, &message, &touched).unwrap();
        assert_eq!(
            by.key.algorithm(),
            Algorithm::SkEd25519,
            "not a security key's signature"
        );
        if std::env::var_os("VAULTORNO_YK_PUB").is_some() {
            let expected = parse_key_line(&read("VAULTORNO_YK_PUB")).unwrap();
            assert_eq!(by.fingerprint, KeyFingerprint::of(&expected));
        }
        let mut edited = message.clone();
        edited.push(b'\n');
        assert_eq!(
            verify_signature(STATEMENT_NAMESPACE, &edited, &touched).unwrap_err(),
            "bad signature"
        );
        assert!(
            verify_signature(SUCCESSION_NAMESPACE, &message, &touched)
                .unwrap_err()
                .contains("namespace")
        );

        let untouched = read("VAULTORNO_YK_NO_TOUCH_SIG");
        let err = verify_signature(STATEMENT_NAMESPACE, &message, &untouched).unwrap_err();
        assert!(err.contains("user presence"), "{err}");
    }
}
