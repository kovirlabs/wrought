//! Wrought's `auth` feature: accounts, sessions and password resets; with
//! `roles`, roles and per-field access.
//!
//! * [`password`]: Argon2id hashing and the password policy;
//! * [`secret`]: random tokens, token hashing, constant-time comparison;
//! * [`totp`]: optional two-factor sign-in (RFC 6238);
//! * [`access`]: what a user may do ([`Access`]), always compiled;
//! * `roles` (feature `roles`): roles stored in the database;
//! * [`notify`]: how password-reset links reach people ([`AccountNotifier`];
//!   v0.3 logs them, the `mail` feature will send them);
//! * [`web`]: cookies, CSRF, the sign-in / sign-out / reset / TOTP pages and
//!   the small HTML layer the admin builds on.
//!
//! Sessions live in the database (`engine_sessions`); the cookie carries a
//! random 256-bit token whose SHA-256 is the row key. Cookies are `Secure`,
//! `HttpOnly`, `SameSite=Lax` and `__Host-` prefixed.
//!
//! `[auth]` in `site.toml` (all optional, see [`AuthConfig`]):
//!
//! ```toml
//! [auth]
//! session_hours = 12          # absolute session lifetime
//! idle_minutes = 120          # sign out after this long without a request
//! behind_proxy = false        # trust X-Forwarded-For (only behind Caddy/nginx)
//! public_url = "https://example.org"   # for links in messages
//! require_totp_for_admins = false
//! cookie_secure = true        # false only for plain-http development off localhost
//! secret_key_file = "/run/secrets/wrought_key"   # or ENGINE_SECRET_KEY
//! registration = "closed"     # "open": anyone may create an account (needs `roles`)
//! default_roles = []          # roles of accounts made by registration
//! verify_email = true         # registered accounts confirm their address before signing in
//!
//! [[auth.oidc]]               # single sign-on providers (`oidc` feature, see [`oidc`])
//! ```

pub mod access;
mod accounts;
pub mod notify;
pub mod oidc;
pub mod password;
#[cfg(feature = "roles")]
pub mod roles;
pub mod secret;
pub mod site;
pub mod sitekey;
pub mod sql;
pub mod totp;
pub mod web;

use std::sync::Arc;

use engine_db::{Backend, Db, Migration};
use serde::Deserialize;

pub use access::{Access, Action, FieldAccess, Grants, TypeGrant};
pub use accounts::{LoginResult, Session, Stage, User, UserChanges};
pub use notify::{AccountNotifier, LogNotifier, Message};

/// The `auth` (and `roles`) features' own schema.
pub fn migrations(backend: Backend) -> Vec<Migration> {
    macro_rules! m {
        ($id:literal, $file:literal) => {
            match backend {
                Backend::Postgres => {
                    Migration::new($id, include_str!(concat!("../migrations/postgres/", $file)))
                }
                Backend::Sqlite => {
                    Migration::new($id, include_str!(concat!("../migrations/sqlite/", $file)))
                }
            }
        };
    }
    #[allow(unused_mut)]
    let mut v = vec![m!("auth/0001_accounts", "0001_accounts.sql")];
    #[cfg(feature = "roles")]
    v.push(m!("roles/0001_roles", "roles_0001_roles.sql"));
    v.push(m!("auth/0002_verification", "0002_verification.sql"));
    #[cfg(feature = "oidc")]
    v.push(m!("oidc/0001_identities", "oidc_0001_identities.sql"));
    v
}

/// `[auth]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Absolute session lifetime.
    #[serde(default = "default_session_hours")]
    pub session_hours: u32,
    /// Idle timeout.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u32,
    /// Use the last `X-Forwarded-For` address as the client's (rate
    /// limits). Only behind a proxy that sets it.
    #[serde(default)]
    pub behind_proxy: bool,
    /// Base URL for links in messages (password reset). Default: `[site]
    /// base_url`, else the address `engine serve` listens on.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Accounts with administrator rights must turn on two-factor sign-in
    /// before they can use the admin.
    #[serde(default)]
    pub require_totp_for_admins: bool,
    /// `Secure` cookies (and the `__Host-` prefix). Browsers and curl treat
    /// `http://localhost` as secure, so leave this on; turn it off only for
    /// plain-http development on another host name.
    #[serde(default = "yes")]
    pub cookie_secure: bool,
    /// Failed sign-ins per account per 15 minutes before refusing more.
    #[serde(default = "default_account_attempts")]
    pub max_failures_per_account: i64,
    /// Failed sign-ins per client address per 15 minutes.
    #[serde(default = "default_ip_attempts")]
    pub max_failures_per_address: i64,
    /// A file holding the site key that encrypts TOTP secrets
    /// (`ENGINE_SECRET_KEY` wins). See [`sitekey`].
    #[serde(default)]
    pub secret_key_file: Option<String>,
    /// `closed` (accounts are made by administrators) or `open` (visitors
    /// register at `/account/register/`).
    #[serde(default)]
    pub registration: Registration,
    /// Roles given to accounts made by registration.
    #[serde(default)]
    pub default_roles: Vec<String>,
    /// Accounts made by registration per client address per hour.
    #[serde(default = "default_registrations")]
    pub max_registrations_per_address: i64,
    /// Registered accounts must follow an emailed link before they can sign
    /// in. Accounts made by administrators or the CLI are trusted.
    #[serde(default = "yes")]
    pub verify_email: bool,
    /// Single sign-on providers (`[[auth.oidc]]`, `oidc` feature).
    #[serde(default)]
    pub oidc: Vec<oidc::ProviderConfig>,
    /// The loaded site key (from the environment or `secret_key_file`).
    #[serde(skip)]
    pub site_key: Option<sitekey::SiteKey>,
}

/// Who may create accounts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Registration {
    #[default]
    Closed,
    Open,
}

fn default_registrations() -> i64 {
    5
}

fn default_session_hours() -> u32 {
    12
}
fn default_idle_minutes() -> u32 {
    120
}
fn yes() -> bool {
    true
}
fn default_account_attempts() -> i64 {
    5
}
fn default_ip_attempts() -> i64 {
    30
}

impl Default for AuthConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl AuthConfig {
    pub fn from_site(section: Option<&toml::Table>) -> anyhow::Result<Self> {
        let cfg: AuthConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[auth]: {e}"))?,
            None => AuthConfig::default(),
        };
        if cfg.session_hours == 0 || cfg.session_hours > 24 * 30 {
            anyhow::bail!("[auth] session_hours must be between 1 and 720");
        }
        if cfg.idle_minutes == 0 {
            anyhow::bail!("[auth] idle_minutes must be at least 1");
        }
        if let Some(u) = &cfg.public_url
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            anyhow::bail!("[auth] public_url must start with https:// or http://");
        }
        let mut cfg = cfg;
        if cfg.registration == Registration::Open && !cfg!(feature = "roles") {
            anyhow::bail!(
                "[auth] registration = \"open\" needs the `roles` feature: without roles every \
                 account is an administrator"
            );
        }
        if !cfg.default_roles.is_empty() && !cfg!(feature = "roles") {
            anyhow::bail!("[auth] default_roles needs the `roles` feature");
        }
        if cfg.default_roles.iter().any(|r| r == "admin") {
            anyhow::bail!("[auth] default_roles must not include `admin`");
        }
        let open = cfg.registration == Registration::Open;
        oidc::check_providers(&mut cfg.oidc, open)?;
        cfg.site_key = sitekey::SiteKey::load(cfg.secret_key_file.as_deref())?;
        if cfg.require_totp_for_admins && cfg.site_key.is_none() {
            anyhow::bail!(
                "[auth] require_totp_for_admins needs a site key to encrypt two-factor secrets: \
                 set {} (32+ random characters) or [auth] secret_key_file",
                sitekey::KEY_ENV
            );
        }
        Ok(cfg)
    }

    /// Name of the session cookie.
    pub fn session_cookie(&self) -> &'static str {
        if self.cookie_secure {
            "__Host-wrought_session"
        } else {
            "wrought_session"
        }
    }

    /// Name of the cookie behind CSRF tokens on signed-out forms.
    pub fn csrf_cookie(&self) -> &'static str {
        self.anon_csrf().cookie_name()
    }

    /// The signed-out CSRF token's cookie settings.
    pub fn anon_csrf(&self) -> engine_core::web::AnonCsrf {
        engine_core::web::AnonCsrf {
            secure: self.cookie_secure,
        }
    }
}

/// Errors from account changes.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Per-field messages, e.g. `("password", "must be at least 12 characters")`.
    #[error("{}", .0.iter().map(|(f, m)| format!("`{f}` {m}")).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<(String, String)>),
    #[error("this account was changed by someone else (version {expected} edited, now {current})")]
    Conflict { expected: i32, current: i32 },
    #[error("no such account")]
    NotFound,
    /// Too many attempts from this client; nothing was done.
    #[error("too many attempts; try again later")]
    Limited,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::Other(e.into())
    }
}

impl AuthError {
    /// As an `anyhow::Error`, keeping the cause of `Other` as it is (so the
    /// chain still says, e.g., that the database is unavailable).
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            AuthError::Other(e) => e,
            e => anyhow::Error::new(e),
        }
    }

    pub fn invalid(field: &str, msg: impl Into<String>) -> Self {
        AuthError::Invalid(vec![(field.to_string(), msg.into())])
    }
}

/// Accounts and sessions of one site. Cheap to clone.
#[derive(Clone)]
pub struct Auth {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    cfg: AuthConfig,
    site_name: String,
    public_url: String,
    notifier: Arc<dyn AccountNotifier>,
}

impl Auth {
    /// `public_url`: where links in messages point (see
    /// [`AuthConfig::public_url`], which wins over this).
    pub fn new(
        db: Db,
        cfg: AuthConfig,
        site_name: &str,
        public_url: &str,
        notifier: Arc<dyn AccountNotifier>,
    ) -> Self {
        let public_url = cfg
            .public_url
            .clone()
            .unwrap_or_else(|| public_url.to_string())
            .trim_end_matches('/')
            .to_string();
        Auth {
            inner: Arc::new(Inner {
                db,
                cfg,
                site_name: site_name.to_string(),
                public_url,
                notifier,
            }),
        }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.inner.cfg
    }

    /// The signed-out CSRF token's cookie settings.
    pub fn anon_csrf(&self) -> engine_core::web::AnonCsrf {
        self.inner.cfg.anon_csrf()
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn site_name(&self) -> &str {
        &self.inner.site_name
    }

    pub fn public_url(&self) -> &str {
        &self.inner.public_url
    }

    pub fn notifier(&self) -> &dyn AccountNotifier {
        self.inner.notifier.as_ref()
    }
}

/// Normalise an email address for storage and lookup.
pub fn normalize_email(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Check an email address's shape (not deliverability).
pub fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    (3..=254).contains(&s.len())
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | ',' | ';')
        })
        && !domain.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_cookie_names() {
        let c = AuthConfig::default();
        assert!(c.cookie_secure);
        assert_eq!(c.session_cookie(), "__Host-wrought_session");
        assert_eq!(c.session_hours, 12);
        let t: toml::Table = toml::from_str("cookie_secure = false\nidle_minutes = 5").unwrap();
        let c = AuthConfig::from_site(Some(&t)).unwrap();
        assert_eq!(c.session_cookie(), "wrought_session");
        let t: toml::Table = toml::from_str("sesion_hours = 1").unwrap();
        assert!(AuthConfig::from_site(Some(&t)).is_err());
    }

    #[test]
    fn emails() {
        assert!(valid_email("ada@example.org"));
        for bad in [
            "",
            "ada",
            "@example.org",
            "ada@org",
            "a da@x.org",
            "a@b@c.org",
            "<a>@x.org",
        ] {
            assert!(!valid_email(bad), "{bad}");
        }
        assert_eq!(normalize_email("  Ada@Example.ORG "), "ada@example.org");
    }
}
