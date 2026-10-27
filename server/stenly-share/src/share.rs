//! Contract helpers (Stenly share contract v1): ids, tokens, envelope checks,
//! timestamps. The server never decrypts; it only checks the envelope frame.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Envelope version byte.
pub const VERSION: u8 = 0x01;
/// `[version][12-byte nonce][ciphertext || 16-byte tag]`: at least 29 bytes.
pub const MIN_ENVELOPE: usize = 1 + 12 + 16;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim()).ok()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random number generator");
    b
}

/// 16 random bytes, base64url without padding (22 characters).
pub fn new_id() -> String {
    b64(&random::<16>())
}

/// 32 random bytes, base64url without padding (43 characters).
pub fn new_owner_token() -> String {
    b64(&random::<32>())
}

/// Hex SHA-256: the only form an owner token is stored in.
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time byte comparison (length leaks, contents don't).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A share id as minted by [`new_id`]: 22 base64url characters.
pub fn valid_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Checks the envelope frame: version byte and minimum length. The server
/// can't (and doesn't try to) verify the ciphertext itself.
pub fn check_envelope(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() < MIN_ENVELOPE {
        return Err("ciphertext is too short to be a v1 envelope");
    }
    if bytes[0] != VERSION {
        return Err("unsupported envelope version");
    }
    Ok(())
}

/// RFC 3986 percent-encoding of everything but unreserved characters.
pub fn urlencode(s: &str) -> String {
    use std::fmt::Write;
    s.bytes().fold(String::with_capacity(s.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// Seconds per day.
pub const DAY: i64 = 86_400;

pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// RFC 3339 UTC, whole seconds: `2026-10-24T09:30:00Z`.
pub fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix).unwrap_or(OffsetDateTime::UNIX_EPOCH).format(&Rfc3339).unwrap_or_default()
}

/// Optional plaintext title for link previews: trimmed, no control
/// characters, at most 200 characters. Empty becomes `None`.
pub fn clean_title(title: Option<&str>) -> Option<String> {
    let t: String = title?.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(200).collect();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tokens_have_contract_lengths() {
        let id = new_id();
        assert_eq!(id.len(), 22);
        assert!(valid_id(&id));
        assert_eq!(new_owner_token().len(), 43);
        assert!(!valid_id("short"));
        assert!(!valid_id("aaaaaaaaaaaaaaaaaaaa/="));
    }

    #[test]
    fn token_hash_is_sha256_hex() {
        assert_eq!(token_hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn rfc3339_is_utc_seconds() {
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clean_title(Some("  Hi\u{0007} there ")), Some("Hi there".into()));
        assert_eq!(clean_title(Some("   ")), None);
        assert_eq!(clean_title(Some(&"x".repeat(500))).unwrap().len(), 200);
    }

    /// The contract test vector. The server never decrypts, so this test
    /// uses AES-GCM (dev-dependency) to prove the vector and our envelope
    /// framing agree, and that the server accepts it.
    #[test]
    fn contract_test_vector() {
        use aes_gcm::{
            Aes256Gcm, KeyInit,
            aead::{Aead, Payload},
        };
        let key = b64_decode("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8").unwrap();
        assert_eq!(key, (0u8..32).collect::<Vec<_>>());
        let nonce = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab];
        let plaintext = br##"{"v":1,"title":"Test standup","markdown":"# Test standup\n\n- Ship it"}"##;
        let expected = "AaChoqOkpaanqKmqq506Cg9_-i6dFgzzv2JY-vwkySpkssQ2DfJqU_Zdh1dsswQsm8BVPR9lvifoXR_wjWdoMikMtG8OHTBXMIlQ1tjdzKUGRIeZOWidVgcd55Jbwe3HNthABw";

        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let ct = cipher.encrypt((&nonce).into(), Payload { msg: plaintext, aad: b"stenly-share-v1" }).unwrap();
        let mut envelope = vec![VERSION];
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ct);
        assert_eq!(b64(&envelope), expected);

        let decoded = b64_decode(expected).unwrap();
        assert!(check_envelope(&decoded).is_ok());
        let pt = cipher.decrypt((&decoded[1..13]).into(), Payload { msg: &decoded[13..], aad: b"stenly-share-v1" }).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn envelope_frame_is_checked() {
        assert!(check_envelope(&[1; 28]).is_err());
        assert!(check_envelope(&[2; 40]).is_err());
        assert!(check_envelope(&[1; 29]).is_ok());
    }
}
