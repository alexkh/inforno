//! The client layer (design doc §6): combining the two halves into the app
//! key, and sealing lockers into envelopes the daemon can't read.
//!
//! Envelope layout:
//!
//! | Bytes | Field |
//! |---|---|
//! | 4 | magic `VLT1` |
//! | 16 | `key_id` |
//! | 24 | nonce, random per write |
//! | rest | XChaCha20-Poly1305 ciphertext of `length (u32 BE) ‖ data ‖ zero padding`, then the 16-byte tag |
//!
//! The whole envelope is a power of two between 1 KiB and 256 KiB.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::Error;
use crate::files::random_bytes;
use crate::protocol::{
    ENVELOPE_MAGIC, ENVELOPE_OVERHEAD, HALF_LEN, MAX_DATA, MIN_STORED, check_envelope_shape,
};

/// Length of the key check carried by every envelope.
pub const KEY_ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 4 + KEY_ID_LEN + NONCE_LEN;

const SPLIT_SALT: &[u8] = b"vaultorno_split/v1";
const KEY_ID_INFO: &[u8] = b"vaultorno_split/v1/key_id";
const LOCKER_INFO: &str = "vaultorno_client/v1";

/// The envelope size for `data_len` bytes of app data.
pub fn stored_size(data_len: usize) -> usize {
    (data_len + ENVELOPE_OVERHEAD)
        .next_power_of_two()
        .max(MIN_STORED)
}

/// `<uid> ‖ 0 ‖ <vault>`: who and which vault, as bound into keys and
/// associated data. Neither part can contain a NUL byte.
pub fn identity_bytes(uid: u32, vault: &str) -> Vec<u8> {
    let mut v = uid.to_string().into_bytes();
    v.push(0);
    v.extend_from_slice(vault.as_bytes());
    v
}

/// A vault's app key. Exists only inside the app's process.
pub struct AppKey {
    key: Zeroizing<[u8; 32]>,
    key_id: [u8; KEY_ID_LEN],
    identity: Vec<u8>,
    vault: String,
}

impl AppKey {
    /// HKDF-SHA256 over `daemon's half ‖ user's half`, salt
    /// `vaultorno_split/v1`, info `<uid> ‖ 0 ‖ <vault>`. Not XOR: with XOR, a
    /// compromised daemon could shift the key by a value it chooses.
    pub fn derive(
        daemon_half: &[u8; HALF_LEN],
        user_half: &[u8; HALF_LEN],
        uid: u32,
        vault: &str,
    ) -> AppKey {
        let mut ikm = Zeroizing::new([0u8; 2 * HALF_LEN]);
        ikm[..HALF_LEN].copy_from_slice(daemon_half);
        ikm[HALF_LEN..].copy_from_slice(user_half);
        let identity = identity_bytes(uid, vault);
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(Some(SPLIT_SALT), &ikm[..])
            .expand(&identity, &mut key[..])
            .expect("32 bytes is a valid HKDF output length");
        let mut key_id = [0u8; KEY_ID_LEN];
        Hkdf::<Sha256>::new(None, &key[..])
            .expand(KEY_ID_INFO, &mut key_id)
            .expect("16 bytes is a valid HKDF output length");
        AppKey {
            key,
            key_id,
            identity,
            vault: vault.to_owned(),
        }
    }

    pub fn key_id(&self) -> &[u8; KEY_ID_LEN] {
        &self.key_id
    }

    fn cipher(&self, locker: &str) -> XChaCha20Poly1305 {
        let mut k = Zeroizing::new([0u8; 32]);
        let info = format!("{LOCKER_INFO}/{}/{locker}", self.vault);
        Hkdf::<Sha256>::new(None, &self.key[..])
            .expand(info.as_bytes(), &mut k[..])
            .expect("32 bytes is a valid HKDF output length");
        XChaCha20Poly1305::new(Key::from_slice(&k[..]))
    }

    /// Magic, key id, user, vault and locker name: an envelope moved to
    /// another locker, vault or user fails to decrypt.
    fn associated_data(&self, locker: &str) -> Vec<u8> {
        let mut ad = Vec::with_capacity(4 + KEY_ID_LEN + self.identity.len() + 1 + locker.len());
        ad.extend_from_slice(&ENVELOPE_MAGIC);
        ad.extend_from_slice(&self.key_id);
        ad.extend_from_slice(&self.identity);
        ad.push(0);
        ad.extend_from_slice(locker.as_bytes());
        ad
    }

    /// Seals `data` for `locker` into an envelope.
    pub fn seal(&self, locker: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
        if data.len() > MAX_DATA {
            return Err(Error::TooLarge(format!(
                "{} bytes; a locker holds at most {MAX_DATA}",
                data.len()
            )));
        }
        let total = stored_size(data.len());
        let mut plain = Zeroizing::new(vec![0u8; total - HEADER_LEN - TAG_LEN]);
        plain[..4].copy_from_slice(&(data.len() as u32).to_be_bytes());
        plain[4..4 + data.len()].copy_from_slice(data);
        let nonce = random_bytes::<NONCE_LEN>()?;
        let sealed = self
            .cipher(locker)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &self.associated_data(locker),
                },
            )
            .map_err(|_| Error::Corrupt)?;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&ENVELOPE_MAGIC);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }

    /// Opens an envelope sealed for `locker`.
    pub fn open(&self, locker: &str, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        if check_envelope_shape(envelope).is_err() {
            return Err(Error::Corrupt);
        }
        if envelope[4..4 + KEY_ID_LEN] != self.key_id {
            return Err(Error::KeyMismatch);
        }
        let nonce = &envelope[4 + KEY_ID_LEN..HEADER_LEN];
        let plain = Zeroizing::new(
            self.cipher(locker)
                .decrypt(
                    XNonce::from_slice(nonce),
                    Payload {
                        msg: &envelope[HEADER_LEN..],
                        aad: &self.associated_data(locker),
                    },
                )
                .map_err(|_| Error::Corrupt)?,
        );
        let len = u32::from_be_bytes(plain[..4].try_into().expect("4 bytes")) as usize;
        if len > plain.len() - 4 || plain[4 + len..].iter().any(|&b| b != 0) {
            return Err(Error::Corrupt);
        }
        Ok(Zeroizing::new(plain[4..4 + len].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MAX_STORED;

    fn key(uid: u32, vault: &str) -> AppKey {
        AppKey::derive(&[1; 32], &[2; 32], uid, vault)
    }

    #[test]
    fn sizes() {
        assert_eq!(stored_size(0), 1024);
        assert_eq!(stored_size(960), 1024);
        assert_eq!(stored_size(961), 2048);
        assert_eq!(stored_size(MAX_DATA), MAX_STORED);
    }

    #[test]
    fn round_trip_at_every_size_class() {
        let k = key(1000, "test_app");
        for len in [0, 1, 960, 961, 5000, MAX_DATA] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let env = k.seal("api_keys", &data).unwrap();
            assert_eq!(env.len(), stored_size(len));
            assert_eq!(&env[..4], b"VLT1");
            assert_eq!(*k.open("api_keys", &env).unwrap(), data);
        }
        assert!(matches!(
            k.seal("api_keys", &vec![0; MAX_DATA + 1]),
            Err(Error::TooLarge(_))
        ));
    }

    #[test]
    fn bound_to_locker_vault_and_user() {
        let k = key(1000, "test_app");
        let env = k.seal("api_keys", b"secret").unwrap();
        assert!(matches!(k.open("other", &env), Err(Error::Corrupt)));
        // Another vault or user gives another key, so another key id.
        assert!(matches!(
            key(1000, "test_other").open("api_keys", &env),
            Err(Error::KeyMismatch)
        ));
        assert!(matches!(
            key(1001, "test_app").open("api_keys", &env),
            Err(Error::KeyMismatch)
        ));
        // Either half changing gives another key.
        let other_half = AppKey::derive(&[9; 32], &[2; 32], 1000, "test_app");
        assert!(matches!(
            other_half.open("api_keys", &env),
            Err(Error::KeyMismatch)
        ));
    }

    #[test]
    fn tampering_fails() {
        let k = key(1000, "test_app");
        let env = k.seal("api_keys", b"secret").unwrap();
        for at in [44, 100, env.len() - 1] {
            let mut bad = env.clone();
            bad[at] ^= 1;
            assert!(
                matches!(k.open("api_keys", &bad), Err(Error::Corrupt)),
                "byte {at}"
            );
        }
        let mut bad_id = env.clone();
        bad_id[4] ^= 1;
        assert!(matches!(
            k.open("api_keys", &bad_id),
            Err(Error::KeyMismatch)
        ));
    }

    #[test]
    fn same_data_seals_differently() {
        let k = key(1000, "test_app");
        assert_ne!(k.seal("a", b"x").unwrap(), k.seal("a", b"x").unwrap());
    }
}
