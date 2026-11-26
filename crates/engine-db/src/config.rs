//! `[database]` in `site.toml`.
//!
//! ```toml
//! [database]
//! url = "postgres://engine@localhost/example"
//! max_connections = 5
//! ```
//!
//! `ENGINE_DATABASE_URL` overrides `url`, so passwords can stay out of the
//! site folder.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::Backend;

/// Environment variable that overrides `[database] url`.
pub const URL_ENV: &str = "ENGINE_DATABASE_URL";

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
            max_connections: 5,
        }
    }

    /// Read `[database]` plus the environment. `Ok(None)` when neither names
    /// a database.
    pub fn from_site(section: Option<&toml::Table>) -> Result<Option<Self>> {
        Self::from_parts(section, std::env::var(URL_ENV).ok())
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
        let max = s.max_connections.unwrap_or(5);
        if max == 0 {
            bail!("[database] max_connections must be at least 1");
        }
        let cfg = DbConfig {
            url,
            max_connections: max,
        };
        cfg.backend()?;
        Ok(Some(cfg))
    }

    /// The backend named by the URL scheme.
    pub fn backend(&self) -> Result<Backend> {
        let scheme = self.url.split_once("://").map(|(s, _)| s).unwrap_or("");
        match scheme {
            "postgres" | "postgresql" => Ok(Backend::Postgres),
            "sqlite" => Ok(Backend::Sqlite),
            _ => bail!(
                "database url {} must start with postgres:// (or sqlite:// from v0.5)",
                self.redacted_url()
            ),
        }
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
        let c = DbConfig::from_parts(Some(&t), Some("postgres://b@h/x".into()))
            .unwrap()
            .unwrap();
        assert_eq!(c.url, "postgres://b@h/x");
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
    fn passwords_are_redacted() {
        assert_eq!(
            redact("postgres://u:secret@localhost:5432/db?sslmode=require"),
            "postgres://u:***@localhost:5432/db?sslmode=require"
        );
        assert_eq!(redact("postgres://u@h/db"), "postgres://u@h/db");
        assert_eq!(redact("postgres://h/db"), "postgres://h/db");
        let c = DbConfig::new("postgres://u:p%40ss@h/db");
        assert!(!format!("{c:?}").contains("p%40ss"));
    }
}
