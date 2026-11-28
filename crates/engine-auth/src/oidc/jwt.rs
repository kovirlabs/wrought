//! ID token checks: a JWS in compact serialisation, signed with RSA
//! (RS256/384/512, PS256/384/512) or ECDSA (ES256, ES384), verified with
//! `ring`. `none` and the HMAC algorithms are refused: an ID token signed
//! with the client secret would let anyone who knows it mint tokens, and an
//! unsigned one proves nothing. The header's `alg` must match the key
//! (`kty`, `crv`, and the key's own `alg` when it states one).
//!
//! Claims (OpenID Connect Core 3.1.3.7): `iss` equals the issuer exactly,
//! `aud` contains the client id (and with several audiences `azp` must be
//! the client id), `exp`, `iat` and `nbf` within [`LEEWAY_SECONDS`],
//! `nonce` equals the one sent (constant-time), `sub` present.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature;
use serde::Deserialize;
use serde_json::Value as Json;

/// Clock difference tolerated between this server and the provider.
pub const LEEWAY_SECONDS: i64 = 60;
/// Longest token accepted.
const MAX_TOKEN: usize = 16 * 1024;

/// Why a token was refused. The messages name the check, never the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    #[error("the ID token is malformed ({0})")]
    Malformed(&'static str),
    #[error("the ID token uses algorithm `{0}`, which is not accepted")]
    Algorithm(String),
    #[error("the ID token names header parameters this server does not understand")]
    Critical,
    #[error("no key of the provider matches the ID token")]
    UnknownKey,
    #[error("the provider's key does not fit the ID token's algorithm")]
    KeyMismatch,
    #[error("the ID token's signature is not valid")]
    Signature,
    #[error("the ID token comes from another issuer")]
    Issuer,
    #[error("the ID token was issued for another client")]
    Audience,
    #[error("the ID token has expired")]
    Expired,
    #[error("the ID token is not valid yet")]
    NotYetValid,
    #[error("the ID token was issued in the future")]
    IssuedInFuture,
    #[error("the ID token's nonce does not match this sign-in")]
    Nonce,
    #[error("the ID token has no subject")]
    Subject,
}

/// Signature algorithms accepted for ID tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    RS256,
    RS384,
    RS512,
    PS256,
    PS384,
    PS512,
    ES256,
    ES384,
}

impl Alg {
    pub fn parse(s: &str) -> Option<Alg> {
        Some(match s {
            "RS256" => Alg::RS256,
            "RS384" => Alg::RS384,
            "RS512" => Alg::RS512,
            "PS256" => Alg::PS256,
            "PS384" => Alg::PS384,
            "PS512" => Alg::PS512,
            "ES256" => Alg::ES256,
            "ES384" => Alg::ES384,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Alg::RS256 => "RS256",
            Alg::RS384 => "RS384",
            Alg::RS512 => "RS512",
            Alg::PS256 => "PS256",
            Alg::PS384 => "PS384",
            Alg::PS512 => "PS512",
            Alg::ES256 => "ES256",
            Alg::ES384 => "ES384",
        }
    }

    fn kty(self) -> &'static str {
        match self {
            Alg::ES256 | Alg::ES384 => "EC",
            _ => "RSA",
        }
    }

    /// Curve and coordinate length of the EC algorithms.
    fn curve(self) -> Option<(&'static str, usize)> {
        match self {
            Alg::ES256 => Some(("P-256", 32)),
            Alg::ES384 => Some(("P-384", 48)),
            _ => None,
        }
    }
}

/// A public key from the provider's JWKS (only the members used here).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

/// Parse a JWKS document. Keys this server cannot use (other types,
/// encryption keys, malformed entries) are skipped.
pub fn parse_jwks(body: &[u8]) -> Result<Vec<Jwk>, JwtError> {
    #[derive(Deserialize)]
    struct Set {
        keys: Vec<Json>,
    }
    let set: Set = serde_json::from_slice(body).map_err(|_| JwtError::Malformed("JWKS"))?;
    Ok(set
        .keys
        .into_iter()
        .filter_map(|k| serde_json::from_value::<Jwk>(k).ok())
        .filter(|k| matches!(k.kty.as_str(), "RSA" | "EC"))
        .filter(|k| k.use_.as_deref().is_none_or(|u| u == "sig"))
        .collect())
}

/// The JOSE header (members used here; others are ignored unless listed in
/// `crit`).
#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub alg: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    crit: Option<Json>,
}

/// What a valid ID token said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdClaims {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    /// `email_verified` was `true` (or the string `"true"`).
    pub email_verified: bool,
    pub name: Option<String>,
}

/// What the token must say.
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    /// Seconds since the Unix epoch.
    pub now: i64,
}

fn b64(s: &str) -> Result<Vec<u8>, JwtError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| JwtError::Malformed("base64url"))
}

struct Parts<'a> {
    signing_input: &'a str,
    payload: &'a str,
    signature: Vec<u8>,
}

fn split(token: &str) -> Result<Parts<'_>, JwtError> {
    if token.len() > MAX_TOKEN {
        return Err(JwtError::Malformed("too long"));
    }
    let mut it = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err(JwtError::Malformed("not three parts"));
    };
    if h.is_empty() || p.is_empty() || s.is_empty() {
        return Err(JwtError::Malformed("empty part"));
    }
    Ok(Parts {
        signing_input: &token[..h.len() + 1 + p.len()],
        payload: p,
        signature: b64(s)?,
    })
}

/// Read and check the header: its algorithm must be one of [`Alg`], and it
/// must not demand extensions (`crit`).
pub fn header(token: &str) -> Result<(Header, Alg), JwtError> {
    let h = token.split('.').next().unwrap_or("");
    let h: Header = serde_json::from_slice(&b64(h)?).map_err(|_| JwtError::Malformed("header"))?;
    let alg = Alg::parse(&h.alg).ok_or_else(|| JwtError::Algorithm(h.alg.clone()))?;
    if h.crit.is_some() {
        return Err(JwtError::Critical);
    }
    Ok((h, alg))
}

/// Whether `key` may verify a signature made with `alg`.
fn key_fits(key: &Jwk, alg: Alg) -> bool {
    key.kty == alg.kty()
        && key.use_.as_deref().is_none_or(|u| u == "sig")
        && key.alg.as_deref().is_none_or(|a| a == alg.name())
        && alg
            .curve()
            .is_none_or(|(crv, _)| key.crv.as_deref() == Some(crv))
}

/// The key that signed a token with this header: the one with its `kid`,
/// or without a `kid`, the only key that fits the algorithm.
pub fn select_key<'a>(keys: &'a [Jwk], header: &Header, alg: Alg) -> Option<&'a Jwk> {
    let mut fitting = keys.iter().filter(|k| key_fits(k, alg));
    match &header.kid {
        Some(kid) => fitting.find(|k| k.kid.as_deref() == Some(kid.as_str())),
        None => {
            let first = fitting.next()?;
            fitting.next().is_none().then_some(first)
        }
    }
}

/// Check the signature of `token` with `key` (whose type must fit `alg`).
pub fn check_signature(token: &str, alg: Alg, key: &Jwk) -> Result<(), JwtError> {
    let parts = split(token)?;
    if !key_fits(key, alg) {
        return Err(JwtError::KeyMismatch);
    }
    let msg = parts.signing_input.as_bytes();
    let sig = &parts.signature;
    let ok = match alg {
        Alg::ES256 | Alg::ES384 => {
            let (_, len) = alg.curve().expect("EC algorithm");
            let x = b64(key.x.as_deref().ok_or(JwtError::KeyMismatch)?)?;
            let y = b64(key.y.as_deref().ok_or(JwtError::KeyMismatch)?)?;
            if x.len() != len || y.len() != len {
                return Err(JwtError::KeyMismatch);
            }
            let mut point = Vec::with_capacity(1 + 2 * len);
            point.push(4);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            let params = if alg == Alg::ES256 {
                &signature::ECDSA_P256_SHA256_FIXED
            } else {
                &signature::ECDSA_P384_SHA384_FIXED
            };
            signature::UnparsedPublicKey::new(params, point)
                .verify(msg, sig)
                .is_ok()
        }
        _ => {
            let n = b64(key.n.as_deref().ok_or(JwtError::KeyMismatch)?)?;
            let e = b64(key.e.as_deref().ok_or(JwtError::KeyMismatch)?)?;
            let params: &signature::RsaParameters = match alg {
                Alg::RS256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                Alg::RS384 => &signature::RSA_PKCS1_2048_8192_SHA384,
                Alg::RS512 => &signature::RSA_PKCS1_2048_8192_SHA512,
                Alg::PS256 => &signature::RSA_PSS_2048_8192_SHA256,
                Alg::PS384 => &signature::RSA_PSS_2048_8192_SHA384,
                _ => &signature::RSA_PSS_2048_8192_SHA512,
            };
            signature::RsaPublicKeyComponents { n: &n, e: &e }
                .verify(params, msg, sig)
                .is_ok()
        }
    };
    if ok { Ok(()) } else { Err(JwtError::Signature) }
}

/// A NumericDate (seconds; fractions allowed and truncated).
fn date(v: Option<&Json>) -> Option<i64> {
    match v? {
        Json::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Check the claims of a token whose signature was verified.
fn check_claims(payload: &str, expect: &Expect) -> Result<IdClaims, JwtError> {
    let claims: serde_json::Map<String, Json> =
        serde_json::from_slice(&b64(payload)?).map_err(|_| JwtError::Malformed("claims"))?;
    let s = |k: &str| claims.get(k).and_then(Json::as_str);
    if s("iss") != Some(expect.issuer) {
        return Err(JwtError::Issuer);
    }
    let auds: Vec<&str> = match claims.get("aud") {
        Some(Json::String(a)) => vec![a.as_str()],
        Some(Json::Array(v)) => v.iter().filter_map(Json::as_str).collect(),
        _ => return Err(JwtError::Audience),
    };
    if !auds.contains(&expect.client_id) {
        return Err(JwtError::Audience);
    }
    // Several audiences: the token must say it was issued to us.
    match s("azp") {
        Some(azp) if azp != expect.client_id => return Err(JwtError::Audience),
        None if auds.len() > 1 => return Err(JwtError::Audience),
        _ => {}
    }
    let now = expect.now;
    let exp = date(claims.get("exp")).ok_or(JwtError::Malformed("exp"))?;
    if exp + LEEWAY_SECONDS <= now {
        return Err(JwtError::Expired);
    }
    let iat = date(claims.get("iat")).ok_or(JwtError::Malformed("iat"))?;
    if iat > now + LEEWAY_SECONDS {
        return Err(JwtError::IssuedInFuture);
    }
    if let Some(nbf) = claims.get("nbf") {
        let nbf = date(Some(nbf)).ok_or(JwtError::Malformed("nbf"))?;
        if nbf > now + LEEWAY_SECONDS {
            return Err(JwtError::NotYetValid);
        }
    }
    match s("nonce") {
        Some(n) if crate::secret::same(n, expect.nonce) => {}
        _ => return Err(JwtError::Nonce),
    }
    let subject = s("sub").unwrap_or("");
    if subject.is_empty() || subject.len() > 255 || subject.chars().any(char::is_control) {
        return Err(JwtError::Subject);
    }
    let email_verified = match claims.get("email_verified") {
        Some(Json::Bool(b)) => *b,
        Some(Json::String(v)) => v == "true",
        _ => false,
    };
    let text = |k: &str| {
        s(k).map(str::trim)
            .filter(|v| !v.is_empty() && !v.chars().any(char::is_control))
            .map(str::to_string)
    };
    Ok(IdClaims {
        issuer: expect.issuer.to_string(),
        subject: subject.to_string(),
        email: text("email"),
        email_verified,
        name: text("name").map(|n| n.chars().take(100).collect()),
    })
}

/// Verify an ID token with `key` (chosen with [`select_key`]) and check its
/// claims.
pub fn verify(token: &str, key: &Jwk, expect: &Expect) -> Result<IdClaims, JwtError> {
    let (_, alg) = header(token)?;
    check_signature(token, alg, key)?;
    check_claims(split(token)?.payload, expect)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair};
    use serde_json::json;

    pub(crate) fn enc(v: &Json) -> String {
        URL_SAFE_NO_PAD.encode(v.to_string())
    }

    /// An ECDSA P-256 key made for the test, with its JWK.
    pub(crate) struct EcKey {
        pair: EcdsaKeyPair,
        pub jwk: Jwk,
    }

    impl EcKey {
        pub(crate) fn new(kid: &str) -> EcKey {
            let rng = SystemRandom::new();
            let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
            let pair = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
            let p = pair.public_key().as_ref();
            assert_eq!(p.len(), 65);
            let jwk = Jwk {
                kty: "EC".into(),
                kid: Some(kid.into()),
                crv: Some("P-256".into()),
                x: Some(URL_SAFE_NO_PAD.encode(&p[1..33])),
                y: Some(URL_SAFE_NO_PAD.encode(&p[33..])),
                use_: Some("sig".into()),
                ..Default::default()
            };
            EcKey { pair, jwk }
        }

        pub(crate) fn sign(&self, header: &Json, claims: &Json) -> String {
            let input = format!("{}.{}", enc(header), enc(claims));
            let sig = self
                .pair
                .sign(&SystemRandom::new(), input.as_bytes())
                .unwrap();
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
        }
    }

    // RFC 7515 appendix A.2: a published example RSA key (not a secret)
    // and a JWS it signed.
    const RFC_N: &str = "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp-Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-pCgNMsD1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xsmtdVMTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ";
    const RFC_D: &str = "Eq5xpGnNCivDflJsRQBXHx1hdR1k6Ulwe2JZD50LpXyWPEAeP88vLNO97IjlA7_GQ5sLKMgvfTeXZx9SE-7YwVol2NXOoAJe46sui395IW_GO-pWJ1O0BkTGoVEn2bKVRUCgu-GjBVaYLU6f3l9kJfFNS3E0QbVdxzubSu3Mkqzjkn439X0M_V51gfpRLI9JYanrC4D4qAdGcopV_0ZHHzQlBjudU2QvXt4ehNYTCBr6XCLQUShb1juUO1ZdiYoFaFQT5Tw8bGUl_x_jTj3ccPDVZFD9pIuhLhBOneufuBiB4cS98l2SR_RQyGWSeWjnczT0QU91p1DhOVRuOopznQ";
    const RFC_P: &str = "4BzEEOtIpmVdVEZNCqS7baC4crd0pqnRH_5IB3jw3bcxGn6QLvnEtfdUdiYrqBdss1l58BQ3KhooKeQTa9AB0Hw_Py5PJdTJNPY8cQn7ouZ2KKDcmnPGBY5t7yLc1QlQ5xHdwW1VhvKn-nXqhJTBgIPgtldC-KDV5z-y2XDwGUc";
    const RFC_Q: &str = "uQPEfgmVtjL0Uyyx88GZFF1fOunH3-7cepKmtH4pxhtCoHqpWmT8YAmZxaewHgHAjLYsp1ZSe7zFYHj7C6ul7TjeLQeZD_YwD66t62wDmpe_HlB-TnBA-njbglfIsRLtXlnDzQkv5dTltRJ11BKBBypeeF6689rjcJIDEz9RWdc";
    const RFC_DP: &str = "BwKfV3Akq5_MFZDFZCnW-wzl-CCo83WoZvnLQwCTeDv8uzluRSnm71I3QCLdhrqE2e9YkxvuxdBfpT_PI7Yz-FOKnu1R6HsJeDCjn12Sk3vmAktV2zb34MCdy7cpdTh_YVr7tss2u6vneTwrA86rZtu5Mbr1C1XsmvkxHQAdYo0";
    const RFC_DQ: &str = "h_96-mK1R_7glhsum81dZxjTnYynPbZpHziZjeeHcXYsXaaMwkOlODsWa7I9xXDoRwbKgB719rrmI2oKr6N3Do9U0ajaHF-NKJnwgjMd2w9cjz3_-kyNlxAr2v4IKhGNpmM5iIgOS1VZnOZ68m6_pbLBSp3nssTdlqvd0tIiTHU";
    const RFC_QI: &str = "IYd7DHOhrWvxkwPQsRM2tOgrjbcrfvtQJipd-DlcxyVuuM9sQLdgjVk2oy26F0EmpScGLq2MowX7fhd_QJQ3ydy5cY7YIBi87w93IKLEdfnbJtoOPLUW0ITrJReOgo1cq9SbsxYawBgfp_gh6A5603k2-ZQwVK0JKSHuLFkuQ3U";
    const RFC_JWS: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw";

    pub(crate) fn rfc_rsa_jwk() -> Jwk {
        Jwk {
            kty: "RSA".into(),
            kid: Some("rfc".into()),
            n: Some(RFC_N.into()),
            e: Some("AQAB".into()),
            ..Default::default()
        }
    }

    /// Sign with the RFC 7515 example key (RS256).
    pub(crate) fn rsa_sign(header: &Json, claims: &Json) -> String {
        let d = |s: &str| URL_SAFE_NO_PAD.decode(s).unwrap();
        let comps = ring::rsa::KeyPairComponents {
            public_key: ring::rsa::PublicKeyComponents {
                n: d(RFC_N),
                e: d("AQAB"),
            },
            d: d(RFC_D),
            p: d(RFC_P),
            q: d(RFC_Q),
            dP: d(RFC_DP),
            dQ: d(RFC_DQ),
            qInv: d(RFC_QI),
        };
        let pair = signature::RsaKeyPair::from_components(&comps).unwrap();
        let input = format!("{}.{}", enc(header), enc(claims));
        let mut sig = vec![0; pair.public().modulus_len()];
        pair.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    const NOW: i64 = 1_800_000_000;
    const ISS: &str = "https://id.example.org";

    fn claims() -> Json {
        json!({
            "iss": ISS, "sub": "user-1", "aud": "hammerwright", "exp": NOW + 300,
            "iat": NOW - 5, "nonce": "n-123", "email": "Ada@Example.org",
            "email_verified": true, "name": "Ada",
        })
    }

    fn expect() -> Expect<'static> {
        Expect {
            issuer: ISS,
            client_id: "hammerwright",
            nonce: "n-123",
            now: NOW,
        }
    }

    fn check(key: &EcKey, header: Json, c: Json) -> Result<IdClaims, JwtError> {
        let t = key.sign(&header, &c);
        verify(&t, &key.jwk, &expect())
    }

    fn with(mut c: Json, k: &str, v: Json) -> Json {
        if v.is_null() {
            c.as_object_mut().unwrap().remove(k);
        } else {
            c[k] = v;
        }
        c
    }

    #[test]
    fn rfc7515_rs256_example_verifies_and_tampering_fails() {
        let key = rfc_rsa_jwk();
        check_signature(RFC_JWS, Alg::RS256, &key).unwrap();
        // Change one character of the payload.
        let bad = RFC_JWS.replacen(".eyJpc3MiOiJqb2", ".eyJpc3MiOiJqc2", 1);
        assert_eq!(
            check_signature(&bad, Alg::RS256, &key),
            Err(JwtError::Signature)
        );
        // The same signature under another algorithm.
        assert_eq!(
            check_signature(RFC_JWS, Alg::PS256, &key),
            Err(JwtError::Signature)
        );
        assert_eq!(
            check_signature(RFC_JWS, Alg::ES256, &key),
            Err(JwtError::KeyMismatch)
        );
        // Tokens the tests sign with the RFC's key verify as RS256.
        let t = rsa_sign(&json!({"alg": "RS256"}), &json!({"iss": "joe"}));
        check_signature(&t, Alg::RS256, &key).unwrap();
    }

    #[test]
    fn rs256_id_token_is_accepted() {
        let t = rsa_sign(&json!({"alg": "RS256", "kid": "rfc"}), &claims());
        let c = verify(&t, &rfc_rsa_jwk(), &expect()).unwrap();
        assert_eq!(c.subject, "user-1");
        assert_eq!(c.email.as_deref(), Some("Ada@Example.org"));
        assert!(c.email_verified);
        // A key that says it is for another algorithm is not used.
        let mut k = rfc_rsa_jwk();
        k.alg = Some("RS512".into());
        assert_eq!(verify(&t, &k, &expect()), Err(JwtError::KeyMismatch));
    }

    #[test]
    fn es256_id_token_is_accepted() {
        let key = EcKey::new("k1");
        let c = check(&key, json!({"alg": "ES256", "kid": "k1"}), claims()).unwrap();
        assert_eq!(c.issuer, ISS);
        assert_eq!(c.name.as_deref(), Some("Ada"));
        // email_verified as a string, as some providers send it.
        let c = check(
            &key,
            json!({"alg": "ES256"}),
            with(claims(), "email_verified", json!("true")),
        )
        .unwrap();
        assert!(c.email_verified);
        let c = check(
            &key,
            json!({"alg": "ES256"}),
            with(claims(), "email_verified", Json::Null),
        )
        .unwrap();
        assert!(!c.email_verified);
    }

    #[test]
    fn bad_signatures_and_algorithms_are_refused() {
        let key = EcKey::new("k1");
        let other = EcKey::new("k1");
        let t = key.sign(&json!({"alg": "ES256"}), &claims());
        // Signed by another key.
        assert_eq!(verify(&t, &other.jwk, &expect()), Err(JwtError::Signature));
        // One bit of the signature flipped.
        let (head, sig) = t.rsplit_once('.').unwrap();
        let mut raw = URL_SAFE_NO_PAD.decode(sig).unwrap();
        raw[10] ^= 1;
        let flipped = format!("{head}.{}", URL_SAFE_NO_PAD.encode(raw));
        assert_eq!(
            verify(&flipped, &key.jwk, &expect()),
            Err(JwtError::Signature)
        );
        // Claims changed after signing.
        let (h, rest) = t.split_once('.').unwrap();
        let (_, s) = rest.split_once('.').unwrap();
        let forged = format!("{h}.{}.{s}", enc(&with(claims(), "sub", json!("admin"))));
        assert_eq!(
            verify(&forged, &key.jwk, &expect()),
            Err(JwtError::Signature)
        );
        // `none`, HMAC and unknown algorithms.
        for alg in ["none", "HS256", "HS512", "ES512", "RS1", ""] {
            let t = format!("{}.{}.c2ln", enc(&json!({"alg": alg})), enc(&claims()));
            assert_eq!(
                verify(&t, &key.jwk, &expect()),
                Err(JwtError::Algorithm(alg.into())),
                "{alg}"
            );
        }
        let unsigned = format!("{}.{}.", enc(&json!({"alg": "none"})), enc(&claims()));
        assert!(verify(&unsigned, &key.jwk, &expect()).is_err());
        // The header says RS256 but the key is an EC key: no confusion.
        let t = key.sign(&json!({"alg": "RS256"}), &claims());
        assert_eq!(verify(&t, &key.jwk, &expect()), Err(JwtError::KeyMismatch));
        // ES384 header with a P-256 key.
        let t = key.sign(&json!({"alg": "ES384"}), &claims());
        assert_eq!(verify(&t, &key.jwk, &expect()), Err(JwtError::KeyMismatch));
        // Extensions the server does not know.
        assert_eq!(
            check(&key, json!({"alg": "ES256", "crit": ["exp"]}), claims()),
            Err(JwtError::Critical)
        );
        // Not a JWS at all.
        for junk in [
            "",
            "a.b",
            "a.b.c.d",
            "!!.e30.e30",
            &"a".repeat(MAX_TOKEN + 1),
        ] {
            assert!(verify(junk, &key.jwk, &expect()).is_err(), "{junk}");
        }
    }

    #[test]
    fn claims_are_checked() {
        let key = EcKey::new("k1");
        let h = || json!({"alg": "ES256"});
        let cases = [
            (
                with(claims(), "iss", json!("https://id.example.org/")),
                JwtError::Issuer,
            ),
            (
                with(claims(), "iss", json!("https://evil.example")),
                JwtError::Issuer,
            ),
            (with(claims(), "iss", Json::Null), JwtError::Issuer),
            (
                with(claims(), "aud", json!("someone-else")),
                JwtError::Audience,
            ),
            (with(claims(), "aud", json!(["a", "b"])), JwtError::Audience),
            (with(claims(), "aud", Json::Null), JwtError::Audience),
            // Several audiences without azp, and a foreign azp.
            (
                with(claims(), "aud", json!(["hammerwright", "other"])),
                JwtError::Audience,
            ),
            (with(claims(), "azp", json!("other")), JwtError::Audience),
            (
                with(claims(), "exp", json!(NOW - LEEWAY_SECONDS - 1)),
                JwtError::Expired,
            ),
            (
                with(claims(), "exp", Json::Null),
                JwtError::Malformed("exp"),
            ),
            (
                with(claims(), "exp", json!("soon")),
                JwtError::Malformed("exp"),
            ),
            (
                with(claims(), "iat", json!(NOW + LEEWAY_SECONDS + 5)),
                JwtError::IssuedInFuture,
            ),
            (
                with(claims(), "iat", Json::Null),
                JwtError::Malformed("iat"),
            ),
            (
                with(claims(), "nbf", json!(NOW + LEEWAY_SECONDS + 5)),
                JwtError::NotYetValid,
            ),
            (with(claims(), "nonce", json!("n-124")), JwtError::Nonce),
            (with(claims(), "nonce", Json::Null), JwtError::Nonce),
            (with(claims(), "sub", json!("")), JwtError::Subject),
            (with(claims(), "sub", Json::Null), JwtError::Subject),
        ];
        for (c, want) in cases {
            assert_eq!(check(&key, h(), c.clone()), Err(want), "{c}");
        }
        // Within the leeway, and several audiences with the right azp.
        let ok = with(claims(), "exp", json!(NOW - LEEWAY_SECONDS + 1));
        let ok = with(ok, "nbf", json!(NOW + 30));
        let ok = with(ok, "aud", json!(["hammerwright", "other"]));
        let ok = with(ok, "azp", json!("hammerwright"));
        check(&key, h(), ok).unwrap();
    }

    #[test]
    fn keys_are_selected_by_kid_and_algorithm() {
        let a = EcKey::new("a");
        let b = EcKey::new("b");
        let mut rsa = rfc_rsa_jwk();
        rsa.kid = Some("a".into());
        let keys = vec![rsa.clone(), a.jwk.clone(), b.jwk.clone()];
        let hdr = |kid: Option<&str>| Header {
            alg: "ES256".into(),
            kid: kid.map(str::to_string),
            crit: None,
        };
        assert_eq!(select_key(&keys, &hdr(Some("b")), Alg::ES256), Some(&b.jwk));
        // kid "a" names both an RSA and an EC key: the algorithm decides.
        assert_eq!(select_key(&keys, &hdr(Some("a")), Alg::ES256), Some(&a.jwk));
        assert_eq!(select_key(&keys, &hdr(Some("a")), Alg::RS256), Some(&rsa));
        assert_eq!(select_key(&keys, &hdr(Some("c")), Alg::ES256), None);
        // No kid: only when exactly one key fits.
        assert_eq!(select_key(&keys, &hdr(None), Alg::ES256), None);
        assert_eq!(select_key(&keys, &hdr(None), Alg::RS256), Some(&rsa));
        // Encryption keys are never used for signatures.
        let mut enc_key = b.jwk.clone();
        enc_key.use_ = Some("enc".into());
        assert_eq!(select_key(&[enc_key], &hdr(Some("b")), Alg::ES256), None);
        let set = json!({"keys": [
            {"kty": "RSA", "kid": "r", "n": RFC_N, "e": "AQAB", "use": "sig"},
            {"kty": "oct", "k": "c2VjcmV0"},
            {"kty": "RSA", "kid": "e", "n": RFC_N, "e": "AQAB", "use": "enc"},
            {"no": "kty"},
        ]});
        let parsed = parse_jwks(set.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kid.as_deref(), Some("r"));
        assert!(parse_jwks(b"[]").is_err());
    }
}
