//! Random tokens, hashing and constant-time comparison.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// `n` random bytes from the operating system.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    OsRng.fill_bytes(&mut b);
    b
}

/// A random 256-bit token, URL-safe base64 (43 characters).
pub fn token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(32))
}

/// Whether `s` looks like a token made by [`token`].
pub fn is_token(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// SHA-256 of a token: what the database stores instead of the token.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Compare two secrets without leaking where they differ.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding (TOTP secrets).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decode base32 (case-insensitive, spaces and padding ignored).
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| !matches!(c, b' ' | b'=' | b'-')) {
        let v = B32.iter().position(|x| *x == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
        buf &= (1 << bits) - 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_recognised() {
        let (a, b) = (token(), token());
        assert_ne!(a, b);
        assert!(is_token(&a) && is_token(&b));
        assert!(!is_token("short") && !is_token(&format!("{}!", &a[..42])));
        assert_eq!(hash_token(&a).len(), 32);
        assert!(same(&a, &a.clone()) && !same(&a, &b) && !same("a", "ab"));
    }

    #[test]
    fn base32_round_trip() {
        // RFC 4648 test vectors.
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"f"), "MY");
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(base32_decode(&base32_encode(data)).unwrap(), data);
        }
        assert_eq!(base32_decode("mzxw 6ytb oi").unwrap(), b"foobar");
        assert!(base32_decode("not base32!").is_none());
    }
}
