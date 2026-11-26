//! Wrought's data layer: one connection pool type ([`Db`]) and a migration
//! runner shared by every database-backed feature.
//!
//! Backends are Cargo features. `postgres` is implemented; `sqlite` is
//! planned (roadmap v0.5). Callers hold a [`Db`] and never name a backend's
//! pool type directly, except inside a `match` on [`Db`] in the module that
//! implements a backend-specific query. Adding SQLite means adding a variant
//! and the matching arms, not changing callers.
//!
//! The database URL comes from `ENGINE_DATABASE_URL` or, failing that,
//! `[database] url` in `site.toml` (see [`DbConfig`]).

#[cfg(not(feature = "postgres"))]
compile_error!(
    "engine-db needs a database backend: enable its `postgres` feature (the `db-postgres` \
     feature of engine-cli, or the `cms` preset). `sqlite` is planned for v0.5."
);

pub mod config;
pub mod migrate;
pub mod testing;

use anyhow::{Context, Result};

pub use config::DbConfig;
pub use migrate::Migration;

/// A database engine, known even when it is not compiled in (for messages
/// and for generating DDL without a connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Postgres,
    Sqlite,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Postgres => "postgres",
            Backend::Sqlite => "sqlite",
        }
    }

    /// Whether this binary can talk to the backend.
    pub fn compiled(self) -> bool {
        match self {
            Backend::Postgres => cfg!(feature = "postgres"),
            Backend::Sqlite => false,
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A connection pool to the site's database.
///
/// Deliberately not `#[non_exhaustive]`: when a backend variant is added,
/// every `match` on `Db` stops compiling until it handles the new backend.
#[derive(Clone, Debug)]
pub enum Db {
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
}

impl Db {
    /// Connect using a site's database settings.
    pub async fn connect(cfg: &DbConfig) -> Result<Db> {
        match cfg.backend()? {
            #[cfg(feature = "postgres")]
            Backend::Postgres => {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(cfg.max_connections)
                    .acquire_timeout(std::time::Duration::from_secs(10))
                    .connect(&cfg.url)
                    .await
                    .with_context(|| format!("connecting to {}", cfg.redacted_url()))?;
                Ok(Db::Postgres(pool))
            }
            other => anyhow::bail!(
                "this engine was built without the `db-{other}` feature, so it cannot use {}",
                cfg.redacted_url()
            ),
        }
    }

    pub fn backend(&self) -> Backend {
        match self {
            #[cfg(feature = "postgres")]
            Db::Postgres(_) => Backend::Postgres,
        }
    }

    /// The Postgres pool, if this is a Postgres database.
    #[cfg(feature = "postgres")]
    #[allow(unreachable_patterns)]
    pub fn postgres(&self) -> Option<&sqlx::PgPool> {
        match self {
            Db::Postgres(p) => Some(p),
            _ => None,
        }
    }

    pub async fn close(&self) {
        match self {
            #[cfg(feature = "postgres")]
            Db::Postgres(p) => p.close().await,
        }
    }
}
