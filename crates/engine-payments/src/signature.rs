//! Webhook signatures in the scheme Stripe uses (and the fake provider
//! copies, so both go through this one check): a header
//! `t=<unix seconds>,v1=<hex HMAC-SHA256>[,v1=...]` where the HMAC is taken
//! over `"<t>.<raw body>"` with the endpoint's secret.
//!
//! A delivery is accepted when the timestamp is within `tolerance` seconds
//! of now (both directions: an old capture cannot be replayed later, a
//! clock far ahead is not trusted either) and any `v1` matches. Comparison
//! is constant-time. Other schemes (`v0=`, unknown keys) are ignored, as
//! Stripe's own libraries do.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Why a signature was refused. Messages name the problem without echoing
/// the header (it may be attacker-made).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("the signature header is missing")]
    Missing,
    #[error("the signature header is malformed")]
    Malformed,
    #[error("the signature has no timestamp")]
    NoTimestamp,
    #[error("the signature has no v1 entry")]
    NoSignature,
    #[error("the timestamp is outside the tolerance ({age} s from now)")]
    Stale { age: i64 },
    #[error("no v1 signature matches")]
    Mismatch,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mac(secret: &str, timestamp: i64, body: &[u8]) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("any key length");
    m.update(timestamp.to_string().as_bytes());
    m.update(b".");
    m.update(body);
    m
}

/// The hex HMAC-SHA256 of `"<timestamp>.<body>"`.
pub fn compute(secret: &str, timestamp: i64, body: &[u8]) -> String {
    hex(&mac(secret, timestamp, body).finalize().into_bytes())
}

/// A header value signing `body` at `timestamp` (the fake provider and
/// tests).
pub fn header(secret: &str, timestamp: i64, body: &[u8]) -> String {
    format!("t={timestamp},v1={}", compute(secret, timestamp, body))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Check `header` against `body`. `now` and `tolerance` in seconds.
pub fn verify(
    header: Option<&str>,
    body: &[u8],
    secret: &str,
    now: i64,
    tolerance: i64,
) -> Result<i64, SignatureError> {
    let header = header.ok_or(SignatureError::Missing)?;
    if header.len() > 4096 {
        return Err(SignatureError::Malformed);
    }
    let mut timestamp = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        let (k, v) = part
            .trim()
            .split_once('=')
            .ok_or(SignatureError::Malformed)?;
        match k {
            "t" => {
                if timestamp.is_some() {
                    return Err(SignatureError::Malformed);
                }
                timestamp = Some(v.parse::<i64>().map_err(|_| SignatureError::Malformed)?);
            }
            "v1" => sigs.push(v),
            _ => {}
        }
    }
    let t = timestamp.ok_or(SignatureError::NoTimestamp)?;
    if sigs.is_empty() {
        return Err(SignatureError::NoSignature);
    }
    let age = now.saturating_sub(t);
    if age.abs() > tolerance {
        return Err(SignatureError::Stale { age });
    }
    let expected = mac(secret, t, body);
    // Check every candidate (no early exit) so timing says nothing about
    // which one matched; each check is constant-time.
    let mut ok = false;
    for s in sigs {
        if let Some(bytes) = unhex(s) {
            ok |= expected.clone().verify_slice(&bytes).is_ok();
        }
    }
    if ok {
        Ok(t)
    } else {
        Err(SignatureError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_secret";
    const BODY: &[u8] = br#"{"id":"evt_1","type":"checkout.session.completed"}"#;

    #[test]
    fn known_vector() {
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog").
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(b"key").unwrap();
        m.update(b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            hex(&m.finalize().into_bytes()),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn a_good_signature_verifies() {
        let h = header(SECRET, 1_700_000_000, BODY);
        assert_eq!(
            verify(Some(&h), BODY, SECRET, 1_700_000_010, 300),
            Ok(1_700_000_000)
        );
    }

    #[test]
    fn any_of_several_v1_entries_may_match() {
        let good = compute(SECRET, 100, BODY);
        let other = compute("another secret", 100, BODY);
        let h = format!("t=100,v1={other},v0=abc,v1={good},extra=1");
        assert!(verify(Some(&h), BODY, SECRET, 100, 300).is_ok());
        let h = format!("t=100,v1={other},v1=zz");
        assert_eq!(
            verify(Some(&h), BODY, SECRET, 100, 300),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn tampering_is_refused() {
        let h = header(SECRET, 100, BODY);
        let mut body = BODY.to_vec();
        body[10] ^= 1;
        assert_eq!(
            verify(Some(&h), &body, SECRET, 100, 300),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(Some(&h), BODY, "wrong secret", 100, 300),
            Err(SignatureError::Mismatch)
        );
        // The timestamp is signed too: moving it breaks the signature.
        let sig = compute(SECRET, 100, BODY);
        let moved = format!("t=101,v1={sig}");
        assert_eq!(
            verify(Some(&moved), BODY, SECRET, 101, 300),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn old_and_future_timestamps_are_refused() {
        let h = header(SECRET, 1000, BODY);
        assert!(verify(Some(&h), BODY, SECRET, 1300, 300).is_ok());
        assert_eq!(
            verify(Some(&h), BODY, SECRET, 1301, 300),
            Err(SignatureError::Stale { age: 301 })
        );
        assert_eq!(
            verify(Some(&h), BODY, SECRET, 600, 300),
            Err(SignatureError::Stale { age: -400 })
        );
    }

    #[test]
    fn malformed_headers_are_refused() {
        let sig = compute(SECRET, 100, BODY);
        for (h, e) in [
            ("", SignatureError::Malformed),
            ("garbage", SignatureError::Malformed),
            ("t=abc,v1=00", SignatureError::Malformed),
            ("t=1,t=2,v1=00", SignatureError::Malformed),
            ("v1=00", SignatureError::NoTimestamp),
            ("t=100", SignatureError::NoSignature),
            ("t=100,v0=00", SignatureError::NoSignature),
        ] {
            assert_eq!(verify(Some(h), BODY, SECRET, 100, 300), Err(e), "{h}");
        }
        assert_eq!(
            verify(None, BODY, SECRET, 100, 300),
            Err(SignatureError::Missing)
        );
        let odd = format!("t=100,v1={}", &sig[1..]);
        assert_eq!(
            verify(Some(&odd), BODY, SECRET, 100, 300),
            Err(SignatureError::Mismatch)
        );
    }
}
