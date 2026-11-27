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
pub mod health;
pub mod migrate;
pub mod sql;
pub mod testing;
mod time;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

pub use config::DbConfig;
pub use health::Health;
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
    /// Reachability (restarts, outages): see [`health`].
    health: Arc<Health>,
    /// Tells pools apart (clones share it), for [`Db::pinned`].
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub(crate) id: u64,
}

impl Db {
    fn wrap(pool: Pool, path: Option<PathBuf>, health: Arc<Health>) -> Db {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Db {
            pool,
            changes: Arc::new(tokio::sync::watch::channel(0).0),
            path,
            health,
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Run `f`, a synchronous piece of work on this thread that blocks on
    /// queries (a page render), with all of its Postgres queries through
    /// this `Db` on one pooled connection: taken at its first query and
    /// returned when `f` ends, instead of one checkout per query.
    ///
    /// sqlx checks a connection with a round trip each time it goes back to
    /// the pool, so a page of five queries cost five extra round trips (ten
    /// with the check on checkout); pinned, it costs one. Queries `f` runs
    /// concurrently get a connection of their own, as before. Transactions
    /// always take their own connection. No effect on SQLite.
    pub fn pinned<R>(&self, f: impl FnOnce() -> R) -> R {
        #[cfg(feature = "postgres")]
        if self.backend() == Backend::Postgres {
            return pin::run(self.id, f);
        }
        f()
    }

    /// Connect using a site's database settings.
    pub async fn connect(cfg: &DbConfig) -> Result<Db> {
        match cfg.backend()? {
            #[cfg(feature = "postgres")]
            Backend::Postgres => {
                use std::str::FromStr;
                // Prepared statements are kept per connection (sqlx's
                // statement cache). The engine has a few hundred distinct
                // statements (the catalogue plus per-type SQL), more than
                // sqlx's default of 100, so a busy connection would keep
                // re-preparing them. One connection is opened at start so
                // the first request does not pay for it. `?host=/run/postgresql`
                // in the URL connects over a Unix socket.
                let opts = sqlx::postgres::PgConnectOptions::from_str(&cfg.url)
                    .with_context(|| format!("database url {}", cfg.redacted_url()))?
                    .statement_cache_capacity(512);
                // sqlx checks a connection with a round trip when it goes
                // back to the pool; checking it again on every checkout too
                // (its default) doubled that. Only a connection idle long
                // enough for the server or a firewall to have dropped it, or
                // idle since before a connection was seen to break (a
                // database restart), is checked before use.
                let health = Arc::new(Health::new(Some(opts.clone())));
                let h = health.clone();
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(cfg.max_connections)
                    .min_connections(1)
                    .acquire_timeout(std::time::Duration::from_secs(10))
                    .test_before_acquire(false)
                    .before_acquire(move |conn, meta| {
                        let check =
                            meta.idle_for >= IDLE_CHECK || h.idle_across_break(meta.idle_for);
                        Box::pin(async move {
                            if check {
                                use sqlx::Connection as _;
                                conn.ping().await?;
                            }
                            Ok(true)
                        })
                    })
                    .connect_with(opts)
                    .await
                    .with_context(|| format!("connecting to {}", cfg.redacted_url()))?;
                Ok(Db::wrap(Pool::Pg(pool), None, health))
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
                // Making a new database file (and switching it to WAL) waits
                // for four disk syncs; on a disk whose syncs sometimes take
                // seconds that outlasted the pool's 15 s wait for a
                // connection, which is meant for contention between
                // connections, not for the disk ("pool timed out while
                // waiting for an open connection" on a fresh site). So a new
                // file is made first, on a connection of its own that waits
                // for the disk as long as it takes; the pool only ever opens
                // an existing WAL file, which syncs nothing.
                if std::fs::metadata(&path).map_or(true, |m| m.len() == 0) {
                    use sqlx::Connection as _;
                    sqlx::sqlite::SqliteConnection::connect_with(&opts)
                        .await
                        .with_context(|| format!("creating {}", path.display()))?
                        .close()
                        .await
                        .with_context(|| format!("creating {}", path.display()))?;
                }
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(cfg.max_connections)
                    .acquire_timeout(std::time::Duration::from_secs(15))
                    .connect_with(opts)
                    .await
                    .with_context(|| format!("opening {}", path.display()))?;
                Ok(Db::wrap(
                    Pool::Lite(pool),
                    Some(path),
                    Arc::new(Health::local()),
                ))
            }
            #[allow(unreachable_patterns)]
            other => anyhow::bail!(
                "this engine was built without the `db-{other}` feature, so it cannot use {}",
                cfg.redacted_url()
            ),
        }
    }

    /// Whether the database can be reached ([`health`]).
    pub fn health(&self) -> &Arc<Health> {
        &self.health
    }

    /// The most connections the pool opens.
    pub fn max_connections(&self) -> u32 {
        match &self.pool {
            #[cfg(feature = "postgres")]
            Pool::Pg(p) => p.options().get_max_connections(),
            #[cfg(feature = "sqlite")]
            Pool::Lite(p) => p.options().get_max_connections(),
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

    /// A `LISTEN` connection on a pool of its own (Postgres).
    ///
    /// `PgListener::connect_with(&pool)` keeps one connection of the shared
    /// pool for as long as it listens. The publisher and the job workers
    /// each listen for the life of `engine serve`, so two of the pool's
    /// connections (five by default) were never available to requests,
    /// renders and jobs; under load everything else queued for the rest and
    /// a publish could wait for seconds. The listener's own one-connection
    /// pool (same settings, reconnected by sqlx when it drops) leaves the
    /// shared pool whole.
    #[cfg(feature = "postgres")]
    pub async fn pg_listener(&self) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
        let Some(shared) = self.postgres() else {
            return Err(sqlx::Error::Configuration(
                "LISTEN needs a Postgres database".into(),
            ));
        };
        let own = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_lazy_with((*shared.connect_options()).clone());
        sqlx::postgres::PgListener::connect_with(&own).await
    }

    /// The database file, for SQLite.
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Start a transaction.
    pub async fn begin(&self) -> Result<Tx, sqlx::Error> {
        Tx::begin(self).await
    }

    /// Start a transaction whose commit does not wait for the write-ahead
    /// log to reach the disk (Postgres `SET LOCAL synchronous_commit = off`;
    /// a plain transaction on SQLite). Only for bookkeeping that can be
    /// redone: a server crash may lose its last fraction of a second, never
    /// corrupt anything. Job heartbeats, claims and results use it; content
    /// saves, revisions, accounts and orders never do.
    pub async fn begin_relaxed(&self) -> Result<Tx, sqlx::Error> {
        let mut tx = Tx::begin(self).await?;
        if self.backend() == Backend::Postgres {
            crate::query("SET LOCAL synchronous_commit = off")
                .execute(&mut tx)
                .await?;
        }
        Ok(tx)
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

/// A pooled Postgres connection idle at least this long is checked with a
/// round trip before it is handed out.
#[cfg(feature = "postgres")]
const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// The connection [`Db::pinned`] holds for the work on this thread.
#[cfg(feature = "postgres")]
pub(crate) mod pin {
    use std::cell::RefCell;

    type PgConn = sqlx::pool::PoolConnection<sqlx::Postgres>;

    thread_local! {
        /// The pinned `Db`'s id and, between queries, its connection.
        static PIN: RefCell<Option<(u64, Option<PgConn>)>> = const { RefCell::new(None) };
    }

    /// Inside [`crate::Db::pinned`] for `db`: the held connection, if any
    /// (the caller gives it back with [`put`]). `None` when not pinned.
    pub(crate) fn take(db: u64) -> Option<Option<PgConn>> {
        PIN.with(|p| match &mut *p.borrow_mut() {
            Some((id, held)) if *id == db => Some(held.take()),
            _ => None,
        })
    }

    /// Give the connection back for the next query (or, if another query
    /// running at the same time already did, to the pool).
    pub(crate) fn put(db: u64, conn: PgConn) {
        let spare = PIN.with(|p| match &mut *p.borrow_mut() {
            Some((id, held)) if *id == db && held.is_none() => {
                *held = Some(conn);
                None
            }
            _ => Some(conn),
        });
        drop(spare);
    }

    /// Restores the previous state even if the work panics.
    struct Guard(Option<Option<(u64, Option<PgConn>)>>);

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                let mine = PIN.with(|p| p.replace(prev));
                // Back to the pool (outside the borrow).
                drop(mine);
            }
        }
    }

    pub(crate) fn run<R>(db: u64, f: impl FnOnce() -> R) -> R {
        let prev = PIN.with(|p| p.replace(Some((db, None))));
        let _guard = Guard(Some(prev));
        f()
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
