//! Single sign-on with OpenID Connect providers (`oidc` feature).
//!
//! Providers are listed in `site.toml`:
//!
//! ```toml
//! [[auth.oidc]]
//! name = "company"                 # [a-z][a-z0-9_]*, in URLs and the secret's variable
//! label = "Company account"        # "Sign in with Company account"
//! issuer = "https://id.example.org"
//! client_id = "wrought"
//! scopes = ["openid", "email", "profile"]   # default; `openid` is always sent
//! public = false                   # true: PKCE only, no client secret
//! create_accounts = false          # default: true when [auth] registration = "open"
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

/// Whether `url` may be used to talk to a provider: `https://`, or plain
/// `http://` to this machine (a provider for local development and tests).
pub fn allowed_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(h) = host_port.strip_prefix('[') {
        h.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
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
        if !allowed_url(&p.issuer) || p.issuer.contains(['?', '#']) || p.issuer.contains(' ') {
            anyhow::bail!(
                "{at}: issuer must be an https:// URL without query or fragment (plain http:// \
                 only for 127.0.0.1, ::1 or localhost)"
            );
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
        client_id = "wrought"
    "#;

    #[test]
    fn urls_must_be_https_except_loopback() {
        for ok in [
            "https://id.example.org",
            "https://id.example.org/realms/x",
            "http://127.0.0.1:5556/dex",
            "http://localhost/x",
            "http://[::1]:8080/",
        ] {
            assert!(allowed_url(ok), "{ok}");
        }
        for bad in [
            "http://id.example.org",
            "http://127.0.0.1.example.org/",
            "http://localhost.example.org/",
            "ftp://x",
            "https:///x",
            "id.example.org",
            "http://[::2]/",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
        assert_eq!(
            origin_of("https://id.example.org:8443/realms/x?y"),
            "https://id.example.org:8443"
        );
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
            ("client_id = \"wrought\"", "client_id = \"\""),
            (
                "client_id = \"wrought\"",
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
