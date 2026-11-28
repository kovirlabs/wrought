//! Talking to providers: discovery, the JWKS (cached, refreshed at most once
//! a minute when a token names an unknown key), the authorization URL and
//! the code exchange. Uses the workspace `reqwest` (rustls with ring), no
//! redirects followed, short timeouts, bounded response sizes, and HTTPS
//! for every URL except loopback ones.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::jwt::{self, IdClaims, Jwk, JwtError};
use super::{ProviderConfig, check_endpoint};

/// How long discovery documents and key sets are kept.
const CACHE_FOR: Duration = Duration::from_secs(3600);
/// Least time between two key-set fetches caused by unknown keys.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// Largest response read from a provider.
const MAX_BODY: usize = 512 * 1024;

/// What went wrong while talking to a provider. Messages never contain
/// codes, tokens or secrets.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("the provider could not be reached or answered badly: {0}")]
    Provider(String),
    #[error(transparent)]
    Token(#[from] JwtError),
}

fn bad(msg: impl Into<String>) -> OidcError {
    OidcError::Provider(msg.into())
}

/// A client secret: never printed.
#[derive(Clone)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The parts of a discovery document used here.
#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
}

#[derive(Default)]
struct Keys {
    keys: Vec<Jwk>,
    fetched: Option<Instant>,
}

struct Provider {
    cfg: ProviderConfig,
    secret: Option<Secret>,
    meta: Mutex<Option<(Arc<Metadata>, Instant)>>,
    keys: Mutex<Keys>,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("cfg", &self.cfg)
            .field("secret", &self.secret)
            .finish()
    }
}

/// The configured providers and their caches. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Oidc {
    providers: Arc<Vec<Provider>>,
    http: reqwest::Client,
}

/// How the client proves itself at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientAuth {
    Basic,
    Post,
    None,
}

impl Oidc {
    /// Build the client for `[[auth.oidc]]`. Client secrets come from
    /// `ENGINE_OIDC_<NAME>_CLIENT_SECRET`; a confidential client without
    /// one is an error, so a misconfigured site fails at start.
    pub fn new(providers: &[ProviderConfig]) -> anyhow::Result<Oidc> {
        Self::with_env(providers, |k| std::env::var(k).ok())
    }

    fn with_env(
        providers: &[ProviderConfig],
        env: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Oidc> {
        let mut list = Vec::new();
        for p in providers {
            let var = p.secret_env();
            let secret = env(&var).filter(|s| !s.is_empty()).map(Secret);
            if secret.is_none() && !p.public {
                anyhow::bail!(
                    "[[auth.oidc]] `{}`: set its client secret in {var} (or `public = true` for \
                     a client without a secret)",
                    p.name
                );
            }
            if secret.is_some() && p.public {
                anyhow::bail!(
                    "[[auth.oidc]] `{}` is a public client, but {var} is set: remove one of them",
                    p.name
                );
            }
            list.push(Provider {
                cfg: p.clone(),
                secret,
                meta: Mutex::new(None),
                keys: Mutex::new(Keys::default()),
            });
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("hammerwright/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Oidc {
            providers: Arc::new(list),
            http,
        })
    }

    /// Settings of the provider called `name`.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.find(name).map(|p| &p.cfg)
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.iter().map(|p| &p.cfg)
    }

    fn find(&self, name: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.cfg.name == name)
    }

    async fn get(&self, p: &ProviderConfig, url: &str) -> Result<Vec<u8>, OidcError> {
        let parsed = checked_url(p, url)?;
        let resp = self
            .http
            .get(parsed)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| bad(format!("GET {url}: {}", short(&e))))?;
        refuse_redirect(&resp, url)?;
        if !resp.status().is_success() {
            return Err(bad(format!("GET {url}: status {}", resp.status())));
        }
        read_limited(resp).await
    }

    /// The provider's discovery document (cached for an hour). Its
    /// `issuer` must equal the configured issuer exactly, and every
    /// endpoint must be https (or loopback).
    pub async fn metadata(&self, name: &str) -> Result<Arc<Metadata>, OidcError> {
        let p = self
            .find(name)
            .ok_or_else(|| bad(format!("no provider {name}")))?;
        let mut cached = p.meta.lock().await;
        if let Some((m, at)) = cached.as_ref()
            && at.elapsed() < CACHE_FOR
        {
            return Ok(m.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            p.cfg.issuer.trim_end_matches('/')
        );
        let body = self.get(&p.cfg, &url).await?;
        let m: Metadata =
            serde_json::from_slice(&body).map_err(|e| bad(format!("discovery document: {e}")))?;
        if m.issuer != p.cfg.issuer {
            return Err(bad(format!(
                "the discovery document names issuer `{}`, not the configured `{}`",
                m.issuer, p.cfg.issuer
            )));
        }
        for (what, u) in [
            ("authorization_endpoint", &m.authorization_endpoint),
            ("token_endpoint", &m.token_endpoint),
            ("jwks_uri", &m.jwks_uri),
        ] {
            checked_url(&p.cfg, u).map_err(|e| bad(format!("{what}: {e}")))?;
        }
        if let Some(methods) = &m.code_challenge_methods_supported
            && !methods.iter().any(|x| x == "S256")
        {
            return Err(bad("the provider does not support PKCE with S256"));
        }
        client_auth(&p.cfg, p.secret.is_some(), &m)?;
        let m = Arc::new(m);
        *cached = Some((m.clone(), Instant::now()));
        Ok(m)
    }

    /// The URL to send the browser to.
    pub async fn authorization_url(
        &self,
        name: &str,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<String, OidcError> {
        let p = self
            .find(name)
            .ok_or_else(|| bad(format!("no provider {name}")))?;
        let m = self.metadata(name).await?;
        let mut url = checked_url(&p.cfg, &m.authorization_endpoint)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &p.cfg.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &p.cfg.scope())
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &pkce_challenge(verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    /// Exchange an authorization code and check the ID token that comes
    /// back. `nonce` and `verifier` are the ones stored for this sign-in.
    pub async fn exchange(
        &self,
        name: &str,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<IdClaims, OidcError> {
        let p = self
            .find(name)
            .ok_or_else(|| bad(format!("no provider {name}")))?;
        let m = self.metadata(name).await?;
        let auth = client_auth(&p.cfg, p.secret.is_some(), &m)?;
        // Checked again right before the secret is attached: https (or
        // this machine), the issuer's host, no user name or password.
        let token_url = checked_url(&p.cfg, &m.token_endpoint)?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ];
        let secret = p.secret.as_ref().map(|s| s.0.as_str()).unwrap_or("");
        if auth != ClientAuth::Basic {
            form.push(("client_id", &p.cfg.client_id));
        }
        if auth == ClientAuth::Post {
            form.push(("client_secret", secret));
        }
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut req = self
            .http
            .post(token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body);
        if auth == ClientAuth::Basic {
            // RFC 6749 2.3.1: both parts form-encoded first.
            let pair = format!("{}:{}", form_encode(&p.cfg.client_id), form_encode(secret));
            req = req.header("authorization", format!("Basic {}", STANDARD.encode(pair)));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| bad(format!("token endpoint: {}", short(&e))))?;
        refuse_redirect(&resp, "the token endpoint")?;
        let status = resp.status();
        let body = read_limited(resp).await?;
        #[derive(Deserialize)]
        struct TokenResponse {
            #[serde(default)]
            id_token: Option<String>,
            #[serde(default)]
            error: Option<String>,
        }
        let t: TokenResponse = serde_json::from_slice(&body)
            .map_err(|_| bad(format!("token endpoint: status {status}, not JSON")))?;
        if !status.is_success() {
            let e = t.error.unwrap_or_default();
            let e: String = e
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(60)
                .collect();
            return Err(bad(format!("token endpoint: status {status} {e}")));
        }
        let token = t
            .id_token
            .ok_or_else(|| bad("the token response has no ID token"))?;
        self.verify_id_token(p, &m, &token, nonce).await
    }

    async fn verify_id_token(
        &self,
        p: &Provider,
        m: &Metadata,
        token: &str,
        nonce: &str,
    ) -> Result<IdClaims, OidcError> {
        let (header, alg) = jwt::header(token)?;
        let expect = jwt::Expect {
            issuer: &p.cfg.issuer,
            client_id: &p.cfg.client_id,
            nonce,
            now: unix_now(),
        };
        let mut keys = p.keys.lock().await;
        let stale = keys.fetched.is_none_or(|at| at.elapsed() >= CACHE_FOR);
        if stale {
            self.fetch_keys(&p.cfg, m, &mut keys).await?;
        }
        if jwt::select_key(&keys.keys, &header, alg).is_none() {
            // A new key (rotation): fetch again, but not more than once a
            // minute however many tokens name unknown keys.
            if keys.fetched.is_some_and(|at| at.elapsed() >= REFRESH_EVERY) {
                self.fetch_keys(&p.cfg, m, &mut keys).await?;
            }
        }
        let key = jwt::select_key(&keys.keys, &header, alg).ok_or(JwtError::UnknownKey)?;
        Ok(jwt::verify(token, key, &expect)?)
    }

    async fn fetch_keys(
        &self,
        p: &ProviderConfig,
        m: &Metadata,
        keys: &mut Keys,
    ) -> Result<(), OidcError> {
        // Counted as a fetch even if it fails, so a broken endpoint is not
        // hammered.
        keys.fetched = Some(Instant::now());
        let body = self.get(p, &m.jwks_uri).await?;
        keys.keys = jwt::parse_jwks(&body)?;
        Ok(())
    }
}

/// How to authenticate at the token endpoint: public clients send only
/// their id; confidential ones prefer HTTP Basic (the default when the
/// provider does not say), else a form post.
fn client_auth(
    cfg: &ProviderConfig,
    has_secret: bool,
    m: &Metadata,
) -> Result<ClientAuth, OidcError> {
    if !has_secret {
        return Ok(ClientAuth::None);
    }
    let methods = m
        .token_endpoint_auth_methods_supported
        .clone()
        .unwrap_or_else(|| vec!["client_secret_basic".into()]);
    if methods.iter().any(|x| x == "client_secret_basic") {
        Ok(ClientAuth::Basic)
    } else if methods.iter().any(|x| x == "client_secret_post") {
        Ok(ClientAuth::Post)
    } else {
        Err(bad(format!(
            "provider `{}` supports neither client_secret_basic nor client_secret_post",
            cfg.name
        )))
    }
}

/// Check a provider URL before anything is sent to it, and parse it: the
/// text is checked ([`check_endpoint`]: https or loopback, the issuer's
/// host or an `allowed_hosts` entry, no user name or password), then the
/// URL as `reqwest` parsed and will use it is checked again the same way,
/// so the two parsers cannot disagree about where a request goes.
fn checked_url(p: &ProviderConfig, url: &str) -> Result<reqwest::Url, OidcError> {
    check_endpoint(p, url).map_err(bad)?;
    let parsed = reqwest::Url::parse(url).map_err(|e| bad(format!("{url}: {e}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad("a URL with a user name or password is refused"));
    }
    check_endpoint(p, parsed.as_str()).map_err(bad)?;
    Ok(parsed)
}

/// Redirects are never followed (the client has no redirect policy); a
/// provider answering with one is an error, so nothing, least of all the
/// client secret, is sent on to where it points.
fn refuse_redirect(resp: &reqwest::Response, what: &str) -> Result<(), OidcError> {
    if resp.status().is_redirection() {
        return Err(bad(format!(
            "{what} answered with a redirect (status {}), which is not followed",
            resp.status()
        )));
    }
    Ok(())
}

/// PKCE S256: BASE64URL(SHA-256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `application/x-www-form-urlencoded` value encoding.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A reqwest error without its URL's query (codes never travel in URLs we
/// request, but keep messages short anyway).
fn short(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(i) = s.find('?') {
        s.truncate(i);
    }
    s
}

async fn read_limited(mut resp: reqwest::Response) -> Result<Vec<u8>, OidcError> {
    let mut out = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| bad(format!("reading the response: {}", short(&e))))?
    {
        if out.len() + chunk.len() > MAX_BODY {
            return Err(bad("the response is too large"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(public: bool) -> ProviderConfig {
        ProviderConfig {
            name: "dex".into(),
            label: "Dex".into(),
            issuer: "http://127.0.0.1:1/dex".into(),
            client_id: "w".into(),
            scopes: vec!["openid".into()],
            public,
            create_accounts: Some(false),
            allowed_hosts: Vec::new(),
        }
    }

    /// A tiny HTTP server on 127.0.0.1 for one test: answers each request
    /// with `answer(path)` (status line and extra headers, body) and keeps
    /// every request it got (head and body).
    struct Fake {
        port: u16,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Fake {
        fn start(answer: impl Fn(&str, u16) -> (String, String) + Send + 'static) -> Fake {
            use std::io::{Read, Write};
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { return };
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Head, then as much body as Content-Length says.
                    loop {
                        let n = s.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(i) = text.find("\r\n\r\n") {
                            let len = text[..i]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if buf.len() >= i + 4 + len {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let path = text.split(' ').nth(1).unwrap_or("").to_string();
                    log.lock().unwrap().push(text);
                    let (head, body) = answer(&path, port);
                    let _ = write!(
                        s,
                        "{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
            Fake { port, seen }
        }

        fn requests(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn discovery(port: u16, token_endpoint: &str) -> String {
        serde_json::json!({
            "issuer": format!("http://127.0.0.1:{port}/dex"),
            "authorization_endpoint": format!("http://127.0.0.1:{port}/dex/auth"),
            "token_endpoint": token_endpoint,
            "jwks_uri": format!("http://127.0.0.1:{port}/dex/keys"),
        })
        .to_string()
    }

    fn client_for(port: u16) -> Oidc {
        let mut c = cfg(false);
        c.issuer = format!("http://127.0.0.1:{port}/dex");
        Oidc::with_env(&[c], |_| Some("s3cret!".into())).unwrap()
    }

    const OK_JSON: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json";

    #[tokio::test]
    async fn token_endpoint_redirects_are_not_followed() {
        // Where a redirect would lead: must never be contacted.
        let elsewhere = Fake::start(|_, _| (OK_JSON.into(), "{}".into()));
        let target = elsewhere.port;
        let provider = Fake::start(move |path, port| match path {
            "/dex/.well-known/openid-configuration" => (
                OK_JSON.into(),
                discovery(port, &format!("http://127.0.0.1:{port}/dex/token")),
            ),
            _ => (
                format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://localhost:{target}/steal"
                ),
                String::new(),
            ),
        });
        let o = client_for(provider.port);
        let e = o
            .exchange("dex", "code", "http://localhost/cb", "verifier", "nonce")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("redirect"), "{e}");
        assert!(elsewhere.requests().is_empty(), "the redirect was followed");
        let token_requests: Vec<String> = provider
            .requests()
            .into_iter()
            .filter(|r| r.starts_with("POST /dex/token"))
            .collect();
        assert_eq!(token_requests.len(), 1);
        // The discovery document and key requests carry no credential.
        for r in provider.requests() {
            if !r.starts_with("POST") {
                assert!(!r.to_ascii_lowercase().contains("authorization:"), "{r}");
            }
        }
    }

    #[tokio::test]
    async fn endpoints_off_the_issuers_host_are_refused() {
        for bad in [
            // Plain http to a host that is not this machine.
            "http://id.example.org/dex/token".to_string(),
            // Another host (a name for this machine is still another host).
            "https://token.example.org/dex/token".to_string(),
            "http://localhost:{port}/dex/token".to_string(),
            // A user name and password in the URL.
            "http://user:pw@127.0.0.1:{port}/dex/token".to_string(),
            "http://evil.example@127.0.0.1:{port}/dex/token".to_string(),
        ] {
            let bad2 = bad.clone();
            let provider = Fake::start(move |path, port| match path {
                "/dex/.well-known/openid-configuration" => (
                    OK_JSON.into(),
                    discovery(port, &bad2.replace("{port}", &port.to_string())),
                ),
                _ => (OK_JSON.into(), "{}".into()),
            });
            let o = client_for(provider.port);
            assert!(o.metadata("dex").await.is_err(), "{bad}");
            let e = o
                .exchange("dex", "code", "http://localhost/cb", "v", "n")
                .await
                .unwrap_err();
            assert!(matches!(e, OidcError::Provider(_)), "{bad}: {e}");
            assert!(
                provider.requests().iter().all(|r| !r.starts_with("POST")),
                "{bad}: the token endpoint was called"
            );
        }
    }

    #[test]
    fn urls_are_checked_by_both_parsers() {
        let mut c = cfg(false);
        c.issuer = "https://id.example.org/dex".into();
        checked_url(&c, "https://id.example.org/dex/token").unwrap();
        for bad in [
            "http://id.example.org/dex/token",
            "https://other.example.org/token",
            "https://u:p@id.example.org/token",
            "https://id.example.org\\@evil.example/token",
            "https://evil.example%2f@id.example.org/token",
        ] {
            assert!(checked_url(&c, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_come_from_the_environment_and_never_print() {
        let env = |k: &str| (k == "ENGINE_OIDC_DEX_CLIENT_SECRET").then(|| "s3cret!".to_string());
        let o = Oidc::with_env(&[cfg(false)], env).unwrap();
        let shown = format!("{o:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("redacted"));
        let e = Oidc::with_env(&[cfg(false)], |_| None).unwrap_err();
        assert!(
            e.to_string().contains("ENGINE_OIDC_DEX_CLIENT_SECRET"),
            "{e}"
        );
        Oidc::with_env(&[cfg(true)], |_| None).unwrap();
        assert!(Oidc::with_env(&[cfg(true)], env).is_err());
    }

    #[test]
    fn pkce_matches_rfc7636() {
        // RFC 7636 appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(form_encode("a b&c=d/é"), "a+b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn token_endpoint_authentication() {
        let m = |methods: Option<Vec<&str>>| Metadata {
            issuer: String::new(),
            authorization_endpoint: String::new(),
            token_endpoint: String::new(),
            jwks_uri: String::new(),
            token_endpoint_auth_methods_supported: methods
                .map(|v| v.into_iter().map(String::from).collect()),
            code_challenge_methods_supported: None,
        };
        let c = cfg(false);
        assert_eq!(client_auth(&c, true, &m(None)).unwrap(), ClientAuth::Basic);
        assert_eq!(
            client_auth(&c, true, &m(Some(vec!["client_secret_post"]))).unwrap(),
            ClientAuth::Post
        );
        assert!(client_auth(&c, true, &m(Some(vec!["private_key_jwt"]))).is_err());
        assert_eq!(client_auth(&c, false, &m(None)).unwrap(), ClientAuth::None);
    }
}
