//! Single sign-on with OpenID Connect providers (`oidc` feature).
//!
//! Providers are listed in `site.toml`:
//!
//! ```toml
//! [[auth.oidc]]
//! name = "company"                 # [a-z][a-z0-9_]*, in URLs and the secret's variable
//! label = "Company account"        # "Sign in with Company account"
//! issuer = "https://id.example.org"
//! client_id = "hammerwright"
//! scopes = ["openid", "email", "profile"]   # default; `openid` is always sent
//! public = false                   # true: PKCE only, no client secret
//! create_accounts = false          # default: true when [auth] registration = "open"
//! allowed_hosts = []               # endpoints on other hosts than the issuer's
//! ```
//!
//! The client secret comes only from the environment,
//! `ENGINE_OIDC_<NAME>_CLIENT_SECRET` (e.g. `ENGINE_OIDC_COMPANY_CLIENT_SECRET`).
//!
//! This part (the settings and [`Identity`]) is always compiled, so
//! `[auth]` can refuse providers in a build without the feature and other
//! crates can list identities without knowing about the feature. The
//! protocol ([`jwt`], the HTTP client, the routes in [`web`]) is compiled
//! with `oidc` only.
//!
//! Security rules (see DECISIONS and the docs page):
//! * identities are keyed by (issuer, subject), never by email address;
//! * an identity nobody has linked never signs into an existing account,
//!   even when the provider reports the same verified email address: the
//!   owner signs in with their password and links the provider on their
//!   profile page;
//! * new accounts are made only when the provider allows it
//!   (`create_accounts`), with `[auth] default_roles`, and only for a
//!   verified address no account uses;
//! * blocked accounts cannot sign in; two-factor sign-in still asks for the
//!   code; an account keeps at least one way to sign in.

use serde::{Deserialize, Serialize};

#[cfg(feature = "oidc")]
mod accounts;
#[cfg(feature = "oidc")]
mod client;
#[cfg(feature = "oidc")]
pub mod jwt;
#[cfg(feature = "oidc")]
pub mod sql;
#[cfg(feature = "oidc")]
pub mod web;

#[cfg(feature = "oidc")]
pub use accounts::{LinkOutcome, Pending, Purpose, SignInOutcome, Started, UnlinkOutcome};
#[cfg(feature = "oidc")]
pub use client::{Oidc, OidcError};
#[cfg(feature = "oidc")]
pub use web::router;

/// One `[[auth.oidc]]` provider.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Short name: URLs (`/account/oidc/<name>/...`) and the secret's
    /// environment variable.
    pub name: String,
    /// Shown on buttons: "Sign in with <label>".
    pub label: String,
    /// The issuer identifier, exactly as the provider states it in its
    /// discovery document.
    pub issuer: String,
    pub client_id: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// A public client: PKCE only, no client secret.
    #[serde(default)]
    pub public: bool,
    /// Make an account for a provider identity nobody has linked yet.
    /// Default: whether `[auth] registration = "open"`.
    #[serde(default)]
    pub create_accounts: Option<bool>,
    /// Hosts other than the issuer's that the provider's endpoints
    /// (authorization, token, keys) may be on. Empty: only the issuer's.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

fn default_scopes() -> Vec<String> {
    vec!["openid".into(), "email".into(), "profile".into()]
}

impl ProviderConfig {
    /// The environment variable holding this provider's client secret.
    pub fn secret_env(&self) -> String {
        format!(
            "ENGINE_OIDC_{}_CLIENT_SECRET",
            self.name.to_ascii_uppercase()
        )
    }

    /// Whether unknown identities get new accounts (resolved by
    /// [`check_providers`]).
    pub fn creates_accounts(&self) -> bool {
        self.create_accounts.unwrap_or(false)
    }

    /// The scope parameter (`openid` first, once).
    pub fn scope(&self) -> String {
        let mut v = vec!["openid"];
        v.extend(
            self.scopes
                .iter()
                .map(String::as_str)
                .filter(|s| *s != "openid"),
        );
        v.join(" ")
    }

    /// `scheme://host[:port]` of the issuer.
    pub fn issuer_origin(&self) -> String {
        origin_of(&self.issuer)
    }
}

/// `scheme://host[:port]` of an absolute URL (the URL itself when it has no
/// path).
pub fn origin_of(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            format!("{scheme}://{host}")
        }
        None => url.to_string(),
    }
}

/// Whether `host` (lower case, IPv6 without brackets) is this machine:
/// `localhost`, `::1` or 127.0.0.0/8.
pub fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The scheme and host (lower case) of a URL a provider may be reached at:
/// `https://`, or plain `http://` only to this machine, with no user name
/// or password in it. The error says what is wrong.
pub fn endpoint_host(url: &str) -> Result<(String, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url} is not an absolute URL"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(format!("{url} is not an https:// URL"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("a URL with a user name or password is refused".into());
    }
    if authority.contains('\\')
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("{url} has a malformed host"));
    }
    let (host, port) = if let Some(h) = authority.strip_prefix('[') {
        let (h, after) = h
            .split_once(']')
            .ok_or_else(|| format!("{url} has a malformed host"))?;
        if !after.is_empty() && !after.starts_with(':') {
            return Err(format!("{url} has a malformed host"));
        }
        (h, after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let bad_port = port.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()));
    if host.is_empty() || bad_port {
        return Err(format!("{url} has a malformed host"));
    }
    let host = host.to_ascii_lowercase();
    if scheme == "http" && !is_loopback(&host) {
        return Err(format!(
            "{url} is plain http: only https:// (or http:// to this machine) is allowed"
        ));
    }
    Ok((scheme, host))
}

/// Whether `url` may be used to talk to a provider (see [`endpoint_host`]).
pub fn allowed_url(url: &str) -> bool {
    endpoint_host(url).is_ok()
}

/// Check an endpoint of `p` (from its discovery document): allowed by
/// [`endpoint_host`], and on the issuer's host or one of `allowed_hosts`.
pub fn check_endpoint(p: &ProviderConfig, url: &str) -> Result<(), String> {
    let (_, host) = endpoint_host(url)?;
    let (_, issuer_host) = endpoint_host(&p.issuer)?;
    if host == issuer_host
        || p.allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host))
    {
        Ok(())
    } else {
        Err(format!(
            "{url} is not on the issuer's host {issuer_host} (list other hosts in allowed_hosts)"
        ))
    }
}

/// Check `[[auth.oidc]]` and resolve each provider's `create_accounts`
/// default. `registration_open`: `[auth] registration = "open"`.
pub fn check_providers(
    providers: &mut [ProviderConfig],
    registration_open: bool,
) -> anyhow::Result<()> {
    if !providers.is_empty() && !cfg!(feature = "oidc") {
        anyhow::bail!(
            "[[auth.oidc]] needs the `oidc` feature, which is not in this build (rebuild with \
             `--features oidc`)"
        );
    }
    let mut names = std::collections::BTreeSet::new();
    let mut issuers = std::collections::BTreeSet::new();
    for p in providers.iter_mut() {
        let at = format!("[[auth.oidc]] `{}`", p.name);
        let n = &p.name;
        if n.is_empty()
            || n.len() > 32
            || !n.starts_with(|c: char| c.is_ascii_lowercase())
            || !n
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            anyhow::bail!(
                "{at}: name must be 1-32 characters of a-z, 0-9 and _, starting with a letter"
            );
        }
        if !names.insert(n.clone()) {
            anyhow::bail!("{at}: two providers have this name");
        }
        let label = p.label.trim();
        if label.is_empty() || label.chars().count() > 60 || label.chars().any(char::is_control) {
            anyhow::bail!("{at}: label must be 1-60 characters");
        }
        if let Err(e) = endpoint_host(&p.issuer) {
            anyhow::bail!(
                "{at}: issuer must be an https:// URL (plain http:// only for this machine: \
                 localhost, 127.0.0.0/8, ::1): {e}"
            );
        }
        if p.issuer.contains(['?', '#', ' ']) {
            anyhow::bail!("{at}: issuer must not have a query or fragment");
        }
        for h in &p.allowed_hosts {
            if h.is_empty()
                || !h
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            {
                anyhow::bail!("{at}: allowed_hosts: `{h}` is not a host name");
            }
        }
        if !issuers.insert(p.issuer.trim_end_matches('/').to_string()) {
            anyhow::bail!("{at}: two providers have the same issuer");
        }
        if p.client_id.is_empty()
            || p.client_id.len() > 255
            || p.client_id.chars().any(|c| c.is_control())
        {
            anyhow::bail!("{at}: client_id must be 1-255 characters");
        }
        for s in &p.scopes {
            if s.is_empty()
                || !s
                    .bytes()
                    .all(|b| b == 0x21 || (0x23..=0x5B).contains(&b) || (0x5D..=0x7E).contains(&b))
            {
                anyhow::bail!("{at}: `{s}` is not a valid scope");
            }
        }
        let create = p.create_accounts.unwrap_or(registration_open);
        if create && !cfg!(feature = "roles") {
            anyhow::bail!(
                "{at}: create_accounts needs the `roles` feature: without roles every account is \
                 an administrator"
            );
        }
        p.create_accounts = Some(create);
    }
    Ok(())
}

/// A provider identity linked to an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Identity {
    /// The provider's `name` when it was linked.
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    /// The address the provider reported when last used (information only:
    /// it is never used to find an account).
    pub email: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl crate::Auth {
    /// The provider identities linked to an account (none without the
    /// `oidc` feature).
    pub async fn identities(&self, user_id: i64) -> anyhow::Result<Vec<Identity>> {
        #[cfg(feature = "oidc")]
        {
            self.oidc_identities(user_id).await
        }
        #[cfg(not(feature = "oidc"))]
        {
            let _ = user_id;
            Ok(Vec::new())
        }
    }

    /// The configured providers (`[[auth.oidc]]`).
    pub fn oidc_providers(&self) -> &[ProviderConfig] {
        &self.config().oidc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(toml_text: &str) -> Vec<ProviderConfig> {
        #[derive(Deserialize)]
        struct W {
            oidc: Vec<ProviderConfig>,
        }
        toml::from_str::<W>(toml_text).unwrap().oidc
    }

    const ONE: &str = r#"
        [[oidc]]
        name = "dex"
        label = "Dex"
        issuer = "http://127.0.0.1:5556/dex"
        client_id = "hammerwright"
    "#;

    #[test]
    fn urls_must_be_https_except_loopback() {
        for ok in [
            "https://id.example.org",
            "https://id.example.org/realms/x",
            "http://127.0.0.1:5556/dex",
            "http://localhost/x",
            "http://[::1]:8080/",
            "http://127.0.0.2/dex",
        ] {
            assert!(allowed_url(ok), "{ok}");
        }
        for bad in [
            "http://id.example.org",
            "http://127.0.0.1.example.org/",
            "http://localhost.example.org/",
            "http://128.0.0.1/",
            "ftp://x",
            "https:///x",
            "id.example.org",
            "http://[::2]/",
            "https://id.example.org:x/",
            "https://[::1]x/",
            // User names or passwords in URLs.
            "https://user:pw@id.example.org/",
            "https://id.example.org@evil.example/",
            "http://evil@127.0.0.1/",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
        assert!(
            endpoint_host("https://u@id.example.org/")
                .unwrap_err()
                .contains("user name")
        );
        assert_eq!(
            origin_of("https://id.example.org:8443/realms/x?y"),
            "https://id.example.org:8443"
        );
    }

    #[test]
    fn endpoints_must_be_on_the_issuers_host() {
        let mut v = p(&ONE.replace(
            "http://127.0.0.1:5556/dex",
            "https://id.example.org/realms/x",
        ));
        let mut pr = v.remove(0);
        check_endpoint(&pr, "https://id.example.org/token").unwrap();
        check_endpoint(&pr, "https://ID.example.org:8443/keys").unwrap();
        for bad in [
            // Another host.
            "https://keys.example.org/keys",
            "https://id.example.org.evil.example/token",
            // Plain http to a host that is not this machine.
            "http://id.example.org/token",
            // User names or passwords.
            "https://u:p@id.example.org/token",
            "https://evil.example@id.example.org/token",
        ] {
            assert!(check_endpoint(&pr, bad).is_err(), "{bad}");
        }
        pr.allowed_hosts = vec!["keys.example.org".into()];
        check_endpoint(&pr, "https://keys.example.org/keys").unwrap();
        assert!(check_endpoint(&pr, "http://keys.example.org/keys").is_err());
    }

    #[cfg(feature = "oidc")]
    #[test]
    fn providers_are_checked() {
        let mut v = p(ONE);
        check_providers(&mut v, false).unwrap();
        assert_eq!(v[0].create_accounts, Some(false));
        assert_eq!(v[0].scope(), "openid email profile");
        assert_eq!(v[0].secret_env(), "ENGINE_OIDC_DEX_CLIENT_SECRET");
        if cfg!(feature = "roles") {
            let mut v = p(ONE);
            check_providers(&mut v, true).unwrap();
            assert_eq!(v[0].create_accounts, Some(true));
        }
        for (from, to) in [
            ("name = \"dex\"", "name = \"Dex\""),
            ("name = \"dex\"", "name = \"a-b\""),
            (
                "issuer = \"http://127.0.0.1:5556/dex\"",
                "issuer = \"http://id.example.org\"",
            ),
            ("label = \"Dex\"", "label = \"\""),
            (
                "issuer = \"http://127.0.0.1:5556/dex\"",
                "issuer = \"https://u:p@id.example.org\"",
            ),
            (
                "label = \"Dex\"",
                "label = \"Dex\"\nallowed_hosts = [\"https://x\"]",
            ),
            ("client_id = \"hammerwright\"", "client_id = \"\""),
            (
                "client_id = \"hammerwright\"",
                "client_id = \"w\"\nscopes = [\"a b\"]",
            ),
        ] {
            let mut v = p(&ONE.replace(from, to));
            assert!(check_providers(&mut v, false).is_err(), "{to}");
        }
        let mut two = p(&format!("{ONE}{ONE}"));
        assert!(check_providers(&mut two, false).is_err());
        let mut v = p(&format!(
            "{ONE}{}",
            ONE.replace("name = \"dex\"", "name = \"dex2\"")
        ));
        assert!(check_providers(&mut v, false).is_err(), "same issuer twice");
    }
}
