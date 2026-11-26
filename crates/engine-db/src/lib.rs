//! Wrought's data layer: one database handle ([`Db`]) for PostgreSQL and
//! SQLite, backend-neutral queries ([`sql`]), a migration runner and test
//! databases.
//!
//! Backends are Cargo features (`postgres`, `sqlite`; both may be on, and
//! the database URL picks one at run time). Feature crates never name a
//! backend's pool type: they declare their statements with
//! [`statements!`] and run them on a [`Db`] or a [`Tx`].
//!
//! The database URL comes from `ENGINE_DATABASE_URL` or, failing that,
//! `[database] url` in `site.toml` (see [`DbConfig`]).

#[cfg(not(any(feature = "postgres", feature = "sqlite")))]
compile_error!(
    "engine-db needs a database backend: enable its `postgres` or `sqlite` feature (the \
     `db-postgres` or `db-sqlite` feature of engine-cli, or a preset)."
);

pub mod config;
pub mod migrate;
pub mod sql;
pub mod testing;
mod time;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

pub use config::DbConfig;
pub use migrate::Migration;
pub use sql::{Arg, Col, Conn, Exec, FromRow, Query, Row, Sql, Tx, query};
pub use time::Timestamp;

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
            Backend::Sqlite => cfg!(feature = "sqlite"),
        }
    }

    /// Both backends, in a fixed order.
    pub const ALL: [Backend; 2] = [Backend::Postgres, Backend::Sqlite];
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Pool {
    #[cfg(feature = "postgres")]
    Pg(sqlx::PgPool),
    #[cfg(feature = "sqlite")]
    Lite(sqlx::SqlitePool),
}

/// A connection pool to the site's database, plus the in-process change
/// signal that stands in for Postgres `NOTIFY` on SQLite.
#[derive(Clone, Debug)]
pub struct Db {
    pub(crate) pool: Pool,
    changes: Arc<tokio::sync::watch::Sender<u64>>,
    path: Option<PathBuf>,
}

impl Db {
    fn wrap(pool: Pool, path: Option<PathBuf>) -> Db {
        Db {
            pool,
            changes: Arc::new(tokio::sync::watch::channel(0).0),
            path,
        }
    }

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
                Ok(Db::wrap(Pool::Pg(pool), None))
            }
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => {
                use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
                use std::str::FromStr;
                let path = config::sqlite_path(&cfg.url)
                    .with_context(|| format!("database url {}", cfg.redacted_url()))?;
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("creating {}", dir.display()))?;
                }
                let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Normal)
                    .foreign_keys(true)
                    .busy_timeout(std::time::Duration::from_secs(15));
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(cfg.max_connections)
                    .acquire_timeout(std::time::Duration::from_secs(15))
                    .connect_with(opts)
                    .await
                    .with_context(|| format!("opening {}", path.display()))?;
                Ok(Db::wrap(Pool::Lite(pool), Some(path)))
            }
            #[allow(unreachable_patterns)]
            other => anyhow::bail!(
                "this engine was built without the `db-{other}` feature, so it cannot use {}",
                cfg.redacted_url()
            ),
        }
    }

    pub fn backend(&self) -> Backend {
        match &self.pool {
            #[cfg(feature = "postgres")]
            Pool::Pg(_) => Backend::Postgres,
            #[cfg(feature = "sqlite")]
            Pool::Lite(_) => Backend::Sqlite,
        }
    }

    /// The Postgres pool, if this is a Postgres database (for the
    /// Postgres-only features: `LISTEN`, job queue).
    #[cfg(feature = "postgres")]
    pub fn postgres(&self) -> Option<&sqlx::PgPool> {
        match &self.pool {
            Pool::Pg(p) => Some(p),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// The database file, for SQLite.
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Start a transaction.
    pub async fn begin(&self) -> Result<Tx, sqlx::Error> {
        Tx::begin(self).await
    }

    /// Hold one connection.
    pub async fn acquire(&self) -> Result<Conn, sqlx::Error> {
        Conn::acquire(self).await
    }

    /// Tell this process's listeners that content changed (the in-process
    /// half of SQLite's change notification; harmless on Postgres, where
    /// `NOTIFY` does the job).
    pub fn notify_changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Wake-ups sent by [`Db::notify_changed`].
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub async fn close(&self) {
        match &self.pool {
            #[cfg(feature = "postgres")]
            Pool::Pg(p) => p.close().await,
            #[cfg(feature = "sqlite")]
            Pool::Lite(p) => p.close().await,
        }
    }
}

/// Error for features the spec keeps on Postgres (`jobs`, `mail`,
/// `payments`).
pub fn postgres_only(feature: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the `{feature}` feature needs PostgreSQL (it relies on `FOR UPDATE SKIP LOCKED` and \
         `LISTEN/NOTIFY`); this site's database is SQLite. Use a postgres:// database URL, or \
         build the engine without `{feature}`"
    )
}
