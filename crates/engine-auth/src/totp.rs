//! Time-based one-time passwords (RFC 6238: HMAC-SHA1, 30 s steps, 6
//! digits), the format every authenticator app reads.

use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::secret;

pub const STEP_SECONDS: u64 = 30;
/// Steps accepted either side of now (clock drift).
pub const WINDOW: i64 = 1;

/// A new random secret (160 bits), base32.
pub fn new_secret() -> String {
    secret::base32_encode(&secret::random_bytes(20))
}

/// The code for one time step.
pub fn code_at(secret_b32: &str, step: u64) -> Option<String> {
    let key = secret::base32_decode(secret_b32)?;
    if key.len() < 10 {
        return None;
    }
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    Some(format!("{:06}", bin % 1_000_000))
}

pub fn current_step() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / STEP_SECONDS)
        .unwrap_or(0)
}

/// The time step `code` matches within the window around `now_step`, if
/// any and if it is later than `last_step` (no replays).
pub fn verify(secret_b32: &str, code: &str, now_step: u64, last_step: Option<i64>) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut found = None;
    for d in -WINDOW..=WINDOW {
        let Some(step) = now_step.checked_add_signed(d) else {
            continue;
        };
        if let Some(expected) = code_at(secret_b32, step)
            && secret::same(&expected, &code)
        {
            found = Some(step);
        }
    }
    found.filter(|s| last_step.is_none_or(|last| (*s as i64) > last))
}

/// The `otpauth://` URI authenticator apps import.
pub fn uri(secret_b32: &str, issuer: &str, account: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'@') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        enc(issuer)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238 appendix B, SHA-1 key "12345678901234567890".
        let secret = secret::base32_encode(b"12345678901234567890");
        for (t, code) in [
            (59u64, "287082"),
            (1_111_111_109, "081804"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(code_at(&secret, t / 30).unwrap(), code, "t={t}");
        }
    }

    #[test]
    fn window_and_replay() {
        let s = new_secret();
        let now = 1_000_000;
        let code = code_at(&s, now).unwrap();
        assert_eq!(verify(&s, &code, now, None), Some(now));
        assert_eq!(verify(&s, &code, now + 1, None), Some(now));
        assert_eq!(verify(&s, &code, now + 2, None), None);
        assert_eq!(verify(&s, &code, now, Some(now as i64)), None, "replay");
        assert_eq!(verify(&s, "12345", now, None), None);
        assert_eq!(verify(&s, "abcdef", now, None), None);
        assert!(uri(&s, "My Site", "a@b.c").starts_with("otpauth://totp/My%20Site:a@b.c?secret="));
    }
}
