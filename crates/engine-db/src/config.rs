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
//! SQLite: `url = "sqlite://data/site.db"` (relative to the site folder) or
//! `sqlite:///var/lib/wrought/site.db` (absolute). The file is created on
//! first use.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::Backend;

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
    /// a database. Relative SQLite paths are relative to `site`.
    pub fn from_site(section: Option<&toml::Table>, site: &Path) -> Result<Option<Self>> {
        let mut cfg = Self::from_parts(section, std::env::var(URL_ENV).ok())?;
        if let Some(c) = cfg.as_mut() {
            c.resolve_relative(site);
        }
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
        cfg.backend()?;
        Ok(Some(cfg))
    }

    /// Make a relative SQLite path absolute, relative to `base`.
    pub fn resolve_relative(&mut self, base: &Path) {
        if self.backend().ok() != Some(Backend::Sqlite) {
            return;
        }
        if let Ok(p) = sqlite_path(&self.url)
            && p.is_relative()
        {
            let abs = std::path::absolute(base.join(&p)).unwrap_or_else(|_| base.join(&p));
            self.url = format!("sqlite://{}", abs.display());
        }
    }

    /// The backend named by the URL scheme.
    pub fn backend(&self) -> Result<Backend> {
        let scheme = self.url.split_once("://").map(|(s, _)| s).unwrap_or("");
        match scheme {
            "postgres" | "postgresql" => Ok(Backend::Postgres),
            "sqlite" => Ok(Backend::Sqlite),
            _ if self.url.starts_with("sqlite:") => Ok(Backend::Sqlite),
            _ => bail!(
                "database url {} must start with postgres:// or sqlite://",
                self.redacted_url()
            ),
        }
    }

    /// The URL with any password replaced by `***`, for messages.
    pub fn redacted_url(&self) -> String {
        redact(&self.url)
    }
}

/// The file of a `sqlite://path`, `sqlite:path` or `sqlite:///abs` URL
/// (query parameters are not supported; the engine sets its own pragmas).
pub fn sqlite_path(url: &str) -> Result<PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    if rest.contains('?') {
        bail!("SQLite URLs take no `?` options; the engine sets WAL, foreign keys and timeouts");
    }
    if rest.is_empty() || rest == ":memory:" {
        bail!(
            "give SQLite a file, e.g. sqlite://data/site.db (in-memory databases are not supported)"
        );
    }
    Ok(PathBuf::from(rest))
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
    fn sqlite_paths_resolve_against_the_site() {
        let mut c = DbConfig::new("sqlite://data/site.db");
        assert_eq!(c.backend().unwrap(), Backend::Sqlite);
        c.resolve_relative(Path::new("/srv/site"));
        assert_eq!(c.url, "sqlite:///srv/site/data/site.db");
        assert_eq!(
            sqlite_path(&c.url).unwrap(),
            PathBuf::from("/srv/site/data/site.db")
        );
        let mut c = DbConfig::new("sqlite:///abs/x.db");
        c.resolve_relative(Path::new("/srv/site"));
        assert_eq!(c.url, "sqlite:///abs/x.db");
        assert!(sqlite_path("sqlite::memory:").is_err());
        assert!(sqlite_path("sqlite://x.db?mode=ro").is_err());
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
