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
use super::{ProviderConfig, allowed_url};

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
            .user_agent(concat!("wrought/", env!("CARGO_PKG_VERSION")))
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

    async fn get(&self, url: &str) -> Result<Vec<u8>, OidcError> {
        if !allowed_url(url) {
            return Err(bad(format!("{url} is not https://")));
        }
        let resp = self
            .http
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| bad(format!("GET {url}: {}", short(&e))))?;
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
        let body = self.get(&url).await?;
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
            if !allowed_url(u) || reqwest::Url::parse(u).is_err() {
                return Err(bad(format!("{what} {u} is not an https:// URL")));
            }
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
        let mut url = reqwest::Url::parse(&m.authorization_endpoint)
            .map_err(|e| bad(format!("authorization_endpoint: {e}")))?;
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
            .post(&m.token_endpoint)
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
            self.fetch_keys(m, &mut keys).await?;
        }
        if jwt::select_key(&keys.keys, &header, alg).is_none() {
            // A new key (rotation): fetch again, but not more than once a
            // minute however many tokens name unknown keys.
            if keys.fetched.is_some_and(|at| at.elapsed() >= REFRESH_EVERY) {
                self.fetch_keys(m, &mut keys).await?;
            }
        }
        let key = jwt::select_key(&keys.keys, &header, alg).ok_or(JwtError::UnknownKey)?;
        Ok(jwt::verify(token, key, &expect)?)
    }

    async fn fetch_keys(&self, m: &Metadata, keys: &mut Keys) -> Result<(), OidcError> {
        // Counted as a fetch even if it fails, so a broken endpoint is not
        // hammered.
        keys.fetched = Some(Instant::now());
        let body = self.get(&m.jwks_uri).await?;
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
