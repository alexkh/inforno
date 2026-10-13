//! YubiKey (FIDO2 hmac-secret) test for the planned Inforno key vault.
//!
//! Checks that a FIDO2 key can derive a stable 32-byte secret, with or without
//! its PIN, and that the secret decrypts what it encrypted earlier.
//!
//!   cargo run --release --example yubitest -- info
//!   cargo run --release --example yubitest -- enroll [--pin] [--file PATH]
//!   cargo run --release --example yubitest -- unlock [--other-mode] [--file PATH]
//!
//! `enroll` creates a credential on the key and stores its id, a random salt
//! and a test ciphertext in `yubitest.json` (nothing in it is secret).
//! `unlock` derives the secret again and decrypts the test ciphertext.
//! `--other-mode` derives in the other PIN mode (with PIN if enrolled
//! without, and vice versa), which must give a different secret, so the
//! decryption is expected to fail.
//!
//! Only one FIDO2 key may be plugged in while this runs.

use anyhow::{anyhow, bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ctap_hid_fido2::fidokey::get_info::{InfoOption, InfoParam};
use ctap_hid_fido2::fidokey::{
    AssertionExtension, CredentialExtension, GetAssertionArgsBuilder, MakeCredentialArgsBuilder,
};
use ctap_hid_fido2::{Cfg, FidoKeyHid, FidoKeyHidFactory};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Relying-party id the credential is bound to. Any string works for a
/// non-web credential; it only has to stay the same.
const RP_ID: &str = "inforno";
const TEST_MESSAGE: &[u8] = b"Inforno vault test: decrypted with the YubiKey-derived key";
const KEY_INFO: &[u8] = b"inforno yubitest v1";

#[derive(serde::Serialize, serde::Deserialize)]
struct Enrolment {
    credential_id: String, // hex
    salt: String,          // hex, 32 bytes
    with_pin: bool,
    nonce: String,      // hex, 24 bytes
    ciphertext: String, // hex
    fingerprint: String,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let file = args
        .iter()
        .position(|a| a == "--file")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("yubitest.json"));

    match args.first().map(String::as_str) {
        Some("info") => info(),
        Some("enroll") => enroll(&file, flag("--pin")),
        Some("unlock") => unlock(&file, flag("--other-mode")),
        _ => {
            eprintln!("usage: yubitest info | enroll [--pin] [--file PATH] | unlock [--other-mode] [--file PATH]");
            Ok(())
        }
    }
}

fn open_device() -> Result<FidoKeyHid> {
    let mut cfg = Cfg::init();
    cfg.keep_alive_msg = "  … touch the YubiKey".to_string();
    FidoKeyHidFactory::create(&cfg).context(
        "could not open the FIDO2 key (is exactly one plugged in, and may this user open /dev/hidraw*?)",
    )
}

/// `Some(true)`/`Some(false)` when the key reports the option, `None` when absent.
fn option(device: &FidoKeyHid, opt: InfoOption) -> Result<Option<bool>> {
    device.enable_info_option(&opt)
}

fn info() -> Result<()> {
    let device = open_device()?;
    let info = device.get_info()?;
    println!("versions:          {:?}", info.versions);
    println!("extensions:        {:?}", info.extensions);
    println!("firmware:          {:#x}", info.firmware_version);
    println!("hmac-secret:       {}", device.enable_info_param(&InfoParam::ExtensionsHmacSecret)?);
    println!("PIN set:           {:?}", option(&device, InfoOption::ClientPin)?);
    println!("alwaysUv:          {:?}  (Some(true) means touch-only enrolment is impossible)", option(&device, InfoOption::AlwaysUv)?);
    println!("makeCredUvNotRqd:  {:?}  (Some(true): enrolling without PIN needs no PIN)", option(&device, InfoOption::MakeCredUvNotRqd)?);
    println!("pinUvAuthProtocols:{:?}", info.pin_uv_auth_protocols);
    Ok(())
}

fn enroll(file: &PathBuf, with_pin: bool) -> Result<()> {
    let device = open_device()?;
    if !device.enable_info_param(&InfoParam::ExtensionsHmacSecret)? {
        bail!("this key does not support the hmac-secret extension");
    }
    let pin_set = option(&device, InfoOption::ClientPin)? == Some(true);
    if with_pin && !pin_set {
        bail!("--pin requested, but the key has no FIDO2 PIN yet (set one with: ykman fido access change-pin)");
    }
    let pin = if with_pin { Some(read_pin()?) } else { None };

    // 1. Create a non-resident credential with hmac-secret enabled.
    println!("Creating the credential…");
    let challenge = random::<32>();
    let extensions = [CredentialExtension::HmacSecret(Some(true))];
    let make = |pin: Option<&str>| {
        let builder = MakeCredentialArgsBuilder::new(RP_ID, &challenge).extensions(&extensions);
        let builder = match pin {
            Some(p) => builder.pin(p),
            None => builder.without_pin_and_uv(),
        };
        device.make_credential_with_args(&builder.build())
    };
    let attestation = match make(pin.as_deref()) {
        Ok(a) => a,
        // A key with a PIN may insist on it for creating credentials, even
        // when the credential is then used without PIN.
        Err(e) if pin.is_none() && pin_set => {
            println!("The key wants its PIN to create the credential ({e}).");
            println!("It is used only now; unlocking will still need just a touch.");
            let creation_pin = read_pin()?;
            make(Some(&creation_pin))?
        }
        Err(e) => return Err(e.context("creating the credential failed")),
    };
    let hmac_enabled = attestation
        .extensions
        .iter()
        .any(|e| matches!(e, CredentialExtension::HmacSecret(Some(true))));
    if !hmac_enabled {
        bail!("the key created the credential without hmac-secret");
    }
    let credential_id = attestation.credential_descriptor.id.clone();

    // 2. Derive the secret once and encrypt a test message with it.
    let salt = random::<32>();
    println!("Deriving the secret…");
    let secret = derive(&device, &credential_id, &salt, pin.as_deref())?;
    let nonce = random::<24>();
    let ciphertext = cipher(&secret)
        .encrypt(XNonce::from_slice(&nonce), TEST_MESSAGE)
        .map_err(|_| anyhow!("encryption failed"))?;

    let enrolment = Enrolment {
        credential_id: hex::encode(&credential_id),
        salt: hex::encode(salt),
        with_pin,
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
        fingerprint: fingerprint(&secret),
    };
    std::fs::write(file, serde_json::to_string_pretty(&enrolment)?)?;
    println!("Enrolled ({}), secret fingerprint {}", mode(with_pin), enrolment.fingerprint);
    println!("Saved to {}. Now run `unlock` (several times, also after unplugging the key).", file.display());
    Ok(())
}

fn unlock(file: &PathBuf, other_mode: bool) -> Result<()> {
    let enrolment: Enrolment = serde_json::from_str(
        &std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?,
    )?;
    let credential_id = hex::decode(&enrolment.credential_id)?;
    let salt: [u8; 32] = hex::decode(&enrolment.salt)?
        .try_into()
        .map_err(|_| anyhow!("salt must be 32 bytes"))?;

    let with_pin = enrolment.with_pin != other_mode;
    let device = open_device()?;
    let pin = if with_pin { Some(read_pin()?) } else { None };

    println!("Deriving the secret ({})…", mode(with_pin));
    let secret = derive(&device, &credential_id, &salt, pin.as_deref())?;
    let fp = fingerprint(&secret);
    println!("secret fingerprint {fp} (enrolled: {})", enrolment.fingerprint);

    let nonce = hex::decode(&enrolment.nonce)?;
    let ciphertext = hex::decode(&enrolment.ciphertext)?;
    match cipher(&secret).decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref()) {
        Ok(plain) => println!("OK: {}", String::from_utf8_lossy(&plain)),
        Err(_) if other_mode => println!("As expected, the other PIN mode gives a different secret: decryption failed."),
        Err(_) => bail!("decryption FAILED: the derived secret differs from the enrolled one"),
    }
    Ok(())
}

/// Asks the key for HMAC-SHA-256(credential secret, salt). Needs a touch; with
/// `pin`, also the PIN, which makes the key use a separate per-credential
/// secret (so the result differs from the touch-only one).
fn derive(device: &FidoKeyHid, credential_id: &[u8], salt: &[u8; 32], pin: Option<&str>) -> Result<[u8; 32]> {
    let challenge = random::<32>();
    let extensions = [AssertionExtension::HmacSecret(Some(*salt))];
    let builder = GetAssertionArgsBuilder::new(RP_ID, &challenge)
        .credential_id(credential_id)
        .extensions(&extensions);
    let builder = match pin {
        Some(p) => builder.pin(p),
        None => builder.without_pin_and_uv(),
    };
    let assertions = device
        .get_assertion_with_args(&builder.build())
        .context("the key refused the assertion (wrong key, wrong PIN, or credential missing)")?;
    let assertion = assertions.first().ok_or_else(|| anyhow!("no assertion returned"))?;
    println!(
        "  user present: {}, user verified: {}",
        assertion.flags.user_present_result, assertion.flags.user_verified_result
    );
    assertion
        .extensions
        .iter()
        .find_map(|e| match e {
            AssertionExtension::HmacSecret(Some(out)) => Some(*out),
            _ => None,
        })
        .ok_or_else(|| anyhow!("the key returned no hmac-secret output"))
}

/// The encryption key is derived from the YubiKey's output rather than used
/// directly, so the raw output is never used for anything else.
fn cipher(secret: &[u8; 32]) -> XChaCha20Poly1305 {
    let mut key = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(None, secret)
        .expand(KEY_INFO, &mut key)
        .expect("32 bytes is a valid HKDF output length");
    XChaCha20Poly1305::new(&key.into())
}

/// Short, non-reversible id of a secret, to compare runs without printing it.
fn fingerprint(secret: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(secret)[..8])
}

fn random<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("the OS random generator failed");
    buf
}

fn read_pin() -> Result<String> {
    Ok(rpassword::prompt_password("FIDO2 PIN: ")?)
}

fn mode(with_pin: bool) -> &'static str {
    if with_pin { "PIN + touch" } else { "touch only" }
}
