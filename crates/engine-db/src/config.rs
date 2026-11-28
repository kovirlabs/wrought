//! `[database]` in `site.toml`.
//!
//! ```toml
//! [database]
//! url = "postgres://engine@localhost/example"
//! max_connections = 5   # optional: sized from the machine when unset
//! ```
//!
//! `ENGINE_DATABASE_URL` overrides `url`, so passwords can stay out of the
//! site folder; `ENGINE_DATABASE_MAX_CONNECTIONS` overrides
//! `max_connections`. Unset, the pool has
//! `engine_core::limits::db_connections` connections (CPUs + 4, 4 to 32).
//!
//! PostgreSQL is the only database. A `sqlite:` URL (accepted up to 0.7) is
//! refused with a message saying what to do instead.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Environment variable that overrides `[database] url`.
pub const URL_ENV: &str = "ENGINE_DATABASE_URL";

/// Environment variable that overrides `max_connections`.
pub const MAX_ENV: &str = engine_core::limits::DB_MAX_ENV;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Clone)]
pub struct DbConfig {
    pub url: String,
    pub max_connections: u32,
}

impl std::fmt::Debug for DbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConfig")
            .field("url", &self.redacted_url())
            .field("max_connections", &self.max_connections)
            .finish()
    }
}

impl DbConfig {
    pub fn new(url: impl Into<String>) -> Self {
        DbConfig {
            url: url.into(),
            max_connections: engine_core::limits::db_connections(engine_core::resources::get()),
        }
    }

    /// Read `[database]` plus the environment. `Ok(None)` when neither names
    /// a database.
    pub fn from_site(section: Option<&toml::Table>) -> Result<Option<Self>> {
        let mut cfg = Self::from_parts(section, std::env::var(URL_ENV).ok())?;
        if let (Some(c), Ok(v)) = (cfg.as_mut(), std::env::var(MAX_ENV)) {
            c.max_connections = v
                .trim()
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| anyhow::anyhow!("{MAX_ENV} must be a whole number above 0"))?;
        }
        Ok(cfg)
    }

    fn from_parts(section: Option<&toml::Table>, env_url: Option<String>) -> Result<Option<Self>> {
        let s: Section = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[database]: {e}"))?,
            None => Section::default(),
        };
        let url = env_url.filter(|u| !u.trim().is_empty()).or(s.url);
        let Some(url) = url else {
            return Ok(None);
        };
        let max = s
            .max_connections
            .unwrap_or_else(|| engine_core::limits::db_connections(engine_core::resources::get()));
        if max == 0 {
            bail!("[database] max_connections must be at least 1");
        }
        let cfg = DbConfig {
            url,
            max_connections: max,
        };
        cfg.check_url()?;
        Ok(Some(cfg))
    }

    /// Error unless the URL names a PostgreSQL database.
    pub fn check_url(&self) -> Result<()> {
        let scheme = self.url.split_once("://").map(|(s, _)| s).unwrap_or("");
        match scheme {
            "postgres" | "postgresql" => Ok(()),
            _ if self.url.starts_with("sqlite:") => bail!(
                "database url {} names a SQLite database, and this engine no longer supports \
                 SQLite (since 0.8 PostgreSQL is the only database). Create a PostgreSQL \
                 database for the site (`engine new --preset cms` writes a compose.yml that \
                 runs one in Docker; or use a PostgreSQL server), set ENGINE_DATABASE_URL (or \
                 [database] url in site.toml) to its postgres:// URL, then run `engine \
                 migrate`. The site's migrations/sqlite/ folder is no longer used.",
                self.redacted_url()
            ),
            _ => bail!(
                "database url {} must start with postgres:// (PostgreSQL is the only database)",
                self.redacted_url()
            ),
        }
    }

    /// The sqlx connection settings of the URL.
    pub fn connect_options(&self) -> Result<sqlx::postgres::PgConnectOptions> {
        use std::str::FromStr;
        self.check_url()?;
        sqlx::postgres::PgConnectOptions::from_str(&self.url)
            .with_context(|| format!("database url {}", self.redacted_url()))
    }

    /// The URL with any password replaced by `***`, for messages.
    pub fn redacted_url(&self) -> String {
        redact(&self.url)
    }
}

/// Replace the password in `scheme://user:password@host/...` with `***`.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, tail) = match rest.find(['/', '?']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let user = userinfo.split(':').next().unwrap_or("");
    if userinfo.contains(':') {
        format!("{scheme}://{user}:***@{host}{tail}")
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn env_overrides_file() {
        let t = table("url = 'postgres://a@h/db'\nmax_connections = 3\n");
        let c = DbConfig::from_parts(Some(&t), None).unwrap().unwrap();
        assert_eq!(c.url, "postgres://a@h/db");
        assert_eq!(c.max_connections, 3);
        let c = DbConfig::from_parts(Some(&t), Some("postgresql://b@h/x".into()))
            .unwrap()
            .unwrap();
        assert_eq!(c.url, "postgresql://b@h/x");
        assert!(DbConfig::from_parts(None, None).unwrap().is_none());
        assert!(
            DbConfig::from_parts(Some(&table("typo = 1")), None)
                .unwrap_err()
                .to_string()
                .contains("typo")
        );
        assert!(DbConfig::from_parts(Some(&table("url = 'mysql://x/y'")), None).is_err());
    }

    #[test]
    fn sqlite_urls_are_refused_with_what_to_do() {
        for url in ["sqlite://data/site.db", "sqlite:///abs/x.db", "sqlite:x.db"] {
            let err = DbConfig::from_parts(Some(&table(&format!("url = '{url}'"))), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains("no longer supports SQLite"), "{err}");
            assert!(err.contains("ENGINE_DATABASE_URL"), "{err}");
            assert!(err.contains("engine migrate"), "{err}");
            // The environment variable is checked the same way.
            assert!(DbConfig::from_parts(None, Some(url.into())).is_err());
        }
        assert!(DbConfig::new("sqlite://x.db").connect_options().is_err());
    }

    #[test]
    fn passwords_are_redacted() {
        assert_eq!(
            redact("postgres://u:secret@localhost:5432/db?sslmode=require"),
            "postgres://u:***@localhost:5432/db?sslmode=require"
        );
        assert_eq!(redact("postgres://u@h/db"), "postgres://u@h/db");
        assert_eq!(redact("postgres://h/db"), "postgres://h/db");
        let c = DbConfig::new("postgres://u:p%40ss@h/db");
        assert!(!format!("{c:?}").contains("p%40ss"));
        let err = DbConfig::new("sqlite://u:pw@x").check_url().unwrap_err();
        assert!(!err.to_string().contains(":pw@"), "{err}");
    }
}
