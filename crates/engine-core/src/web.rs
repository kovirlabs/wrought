//! HTTP helpers shared by the features that take form posts from visitors
//! (`auth`, `forms`, `admin`), compiled with the `request` render mode:
//! cookies, origin checks, submitted forms, client addresses, random
//! tokens, and the signed-out CSRF token (double-submit cookie).
//!
//! Signed-in forms use the session's own token (engine-auth); forms shown
//! to visitors without a session use [`AnonCsrf`]: a random token in a
//! `SameSite=Strict`, `HttpOnly` cookie that the form must repeat in
//! `_csrf`. A cross-site page can neither read the cookie nor make the
//! browser send it, so it cannot produce a matching pair.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use serde::Deserialize;

/// Random tokens, hashing and constant-time comparison.
pub mod secret {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;

    /// `n` random bytes from the operating system.
    pub fn random_bytes(n: usize) -> Vec<u8> {
        let mut b = vec![0u8; n];
        getrandom::getrandom(&mut b).expect("the operating system's random source");
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

    /// SHA-256 of a token: what a database stores instead of the token.
    pub fn hash_token(token: &str) -> Vec<u8> {
        Sha256::digest(token.as_bytes()).to_vec()
    }

    /// Compare two secrets without leaking where they differ.
    pub fn same(a: &str, b: &str) -> bool {
        a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
    }
}

/// The value of cookie `name`.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
}

/// A `Set-Cookie` value. `max_age` 0 deletes the cookie.
pub fn set_cookie(
    name: &str,
    value: &str,
    max_age: u64,
    secure: bool,
    same_site: &str,
) -> HeaderValue {
    let mut s =
        format!("{name}={value}; Path=/; HttpOnly; SameSite={same_site}; Max-Age={max_age}");
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).expect("cookie values are tokens")
}

/// Whether a POST's `Origin` (when sent) is this host. Browsers send it on
/// every cross-origin POST, so this stops cross-site form posts even before
/// the CSRF token is checked.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match origin.split_once("://") {
        Some((_, rest)) => !host.is_empty() && rest.eq_ignore_ascii_case(host),
        None => false,
    }
}

/// A submitted form, as ordered name/value pairs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct FormData(pub Vec<(String, String)>);

impl FormData {
    /// The last value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k == name)
    }
}

/// Who is asking: peer address, forwarded-for header, user agent.
#[derive(Debug, Clone, Default)]
pub struct Client {
    pub peer: Option<IpAddr>,
    pub forwarded_for: Option<String>,
    pub user_agent: String,
}

impl Client {
    pub fn from_headers(headers: &HeaderMap, peer: Option<IpAddr>) -> Self {
        let text = |n: header::HeaderName| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        Client {
            peer,
            forwarded_for: text(header::HeaderName::from_static("x-forwarded-for")),
            user_agent: text(header::USER_AGENT).unwrap_or_default(),
        }
    }

    /// The client's address for rate limits: the peer, or behind a proxy
    /// (`behind_proxy`) the last `X-Forwarded-For` entry, which is the one
    /// the proxy itself added.
    pub fn ip(&self, behind_proxy: bool) -> Option<String> {
        if behind_proxy
            && let Some(last) = self
                .forwarded_for
                .as_deref()
                .and_then(|f| f.rsplit(',').next())
                .map(str::trim)
                .and_then(|s| s.parse::<IpAddr>().ok())
        {
            return Some(last.to_string());
        }
        self.peer.map(|p| p.to_string())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(Client::from_headers(&parts.headers, peer))
    }
}

/// The signed-out CSRF token (double-submit cookie).
#[derive(Debug, Clone, Copy)]
pub struct AnonCsrf {
    /// `Secure` cookie with the `__Host-` prefix (see `[auth] cookie_secure`).
    pub secure: bool,
}

impl AnonCsrf {
    pub fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-hammerwright_csrf"
        } else {
            "hammerwright_csrf"
        }
    }

    /// The visitor's token, or a new one plus the cookie to set.
    pub fn token(&self, headers: &HeaderMap) -> (String, Option<HeaderValue>) {
        match cookie(headers, self.cookie_name()).filter(|t| secret::is_token(t)) {
            Some(t) => (t, None),
            None => {
                let t = secret::token();
                let c = set_cookie(self.cookie_name(), &t, 4 * 3600, self.secure, "Strict");
                (t, Some(c))
            }
        }
    }

    /// Whether a signed-out form's `_csrf` matches the cookie (and the post
    /// is not cross-origin).
    pub fn check(&self, headers: &HeaderMap, form: &FormData) -> bool {
        let Some(c) = cookie(headers, self.cookie_name()) else {
            return false;
        };
        same_origin(headers)
            && secret::is_token(&c)
            && form.get("_csrf").is_some_and(|f| secret::same(f, &c))
    }
}

/// Percent-encode a query value.
pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Decode `a=1&b=x+y%21` into pairs.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let dec = |s: &str| percent_decode(&s.replace('+', " "));
            (!k.is_empty()).then(|| (dec(k), dec(v)))
        })
        .collect()
}

/// Percent-decode, leaving malformed escapes as they are.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A same-site path to return to (`next`, `_page`): only plain paths on
/// this site.
pub fn safe_path(next: Option<&str>) -> Option<String> {
    let n = next?;
    (n.starts_with('/')
        && !n.starts_with("//")
        && !n.contains('\\')
        && n.len() <= 2000
        && !n.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then(|| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let a = secret::token();
        assert!(secret::is_token(&a));
        assert_ne!(a, secret::token());
        assert!(secret::same(&a, &a.clone()));
        assert!(!secret::same(&a, "x"));
    }

    #[test]
    fn anon_csrf_round_trip() {
        let c = AnonCsrf { secure: true };
        let (t, set) = c.token(&HeaderMap::new());
        let set = set.unwrap();
        let s = set.to_str().unwrap();
        assert!(s.starts_with("__Host-hammerwright_csrf=") && s.contains("SameSite=Strict"));
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("__Host-hammerwright_csrf={t}")).unwrap(),
        );
        assert_eq!(c.token(&h), (t.clone(), None));
        let form = FormData(vec![("_csrf".into(), t.clone())]);
        assert!(c.check(&h, &form));
        assert!(!c.check(&h, &FormData(vec![("_csrf".into(), "x".into())])));
        assert!(!c.check(&HeaderMap::new(), &form));
        h.insert(header::HOST, HeaderValue::from_static("a.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!c.check(&h, &form));
    }

    #[test]
    fn query_and_paths() {
        let q = parse_query("page=2&q=a+b%21&flag&=x");
        assert_eq!(
            q,
            [
                ("page".into(), "2".into()),
                ("q".into(), "a b!".into()),
                ("flag".into(), String::new())
            ]
        );
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%é%C3%A9"), "%éé");
        assert_eq!(safe_path(Some("/a/?x=1")).as_deref(), Some("/a/?x=1"));
        for bad in ["//evil", "https://x", "/\\x", "/a b", "x"] {
            assert!(safe_path(Some(bad)).is_none(), "{bad}");
        }
        let c = Client {
            peer: Some("10.0.0.1".parse().unwrap()),
            forwarded_for: Some("1.2.3.4, 5.6.7.8".into()),
            user_agent: String::new(),
        };
        assert_eq!(c.ip(false).as_deref(), Some("10.0.0.1"));
        assert_eq!(c.ip(true).as_deref(), Some("5.6.7.8"));
    }
}
