//! Hammerwright's data layer: the PostgreSQL database handle ([`Db`]), queries
//! ([`sql`]), a migration runner and test databases.
//!
//! PostgreSQL is the only database (DECISIONS.md, v0.8: SQLite removed), so
//! there is one SQL dialect and one code path. Feature crates declare their
//! statements with [`statements!`] and run them on a [`Db`], a [`Tx`] or a
//! [`Conn`]; they never take connections from the pool themselves, so
//! every query gets the outage handling of [`health`].
//!
//! The database URL comes from `ENGINE_DATABASE_URL` or, failing that,
//! `[database] url` in `site.toml` (see [`DbConfig`]).

pub mod config;
pub mod health;
pub mod migrate;
pub mod sleep;
pub mod sql;
pub mod testing;
mod time;

use std::sync::Arc;

use anyhow::{Context, Result};

pub use config::DbConfig;
pub use health::Health;
pub use migrate::Migration;
pub use sql::{Arg, Col, Conn, Exec, FromRow, Query, Row, Sql, Tx, query};
pub use time::Timestamp;

/// A connection pool to the site's PostgreSQL database.
#[derive(Clone, Debug)]
pub struct Db {
    pub(crate) pool: sqlx::PgPool,
    /// Reachability (restarts, outages): see [`health`].
    health: Arc<Health>,
    /// Tells pools apart (clones share it), for [`Db::pinned`].
    pub(crate) id: u64,
}

/// Whether the database server takes a connection now: one attempt, at
/// most 2 s, no pool. `Ok(false)`: unreachable for now (refused, timed out,
/// no such host, starting up or shutting down), worth trying again;
/// `Err`: it answered and refused (password, no such database), which
/// waiting will not change.
pub async fn reachable(cfg: &DbConfig) -> Result<bool> {
    use sqlx::Connection as _;
    let opts = cfg.connect_options()?;
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::postgres::PgConnection::connect_with(&opts),
    )
    .await
    {
        Ok(Ok(c)) => {
            let _ = c.close().await;
            Ok(true)
        }
        Ok(Err(sqlx::Error::Io(_))) | Err(_) => Ok(false),
        Ok(Err(e)) if health::disconnect(&e).is_some() => Ok(false),
        Ok(Err(e)) => {
            Err(anyhow::Error::from(e).context(format!("connecting to {}", cfg.redacted_url())))
        }
    }
}

impl Db {
    /// Run `f`, a synchronous piece of work on this thread that blocks on
    /// queries (a page render), with all of its queries through this `Db`
    /// on one pooled connection: taken at its first query and returned
    /// when `f` ends, instead of one checkout per query.
    ///
    /// sqlx checks a connection with a round trip each time it goes back to
    /// the pool, so a page of five queries cost five extra round trips (ten
    /// with the check on checkout); pinned, it costs one. Queries `f` runs
    /// concurrently get a connection of their own, as before. Transactions
    /// always take their own connection.
    pub fn pinned<R>(&self, f: impl FnOnce() -> R) -> R {
        pin::run(self.id, f)
    }

    /// Connect using a site's database settings.
    pub async fn connect(cfg: &DbConfig) -> Result<Db> {
        // Prepared statements are kept per connection (sqlx's statement
        // cache). The engine has a few hundred distinct statements (the
        // catalogue plus per-type SQL), more than sqlx's default of 100, so
        // a busy connection would keep re-preparing them. One connection is
        // opened at start so the first request does not pay for it.
        // `?host=/run/postgresql` in the URL connects over a Unix socket.
        let opts = cfg
            .connect_options()?
            .statement_cache_capacity(512)
            // This process's sessions, told apart from others' (a backup, a
            // migration) before a sleeping database is stopped (`sleep`).
            .application_name(&format!("hammerwright-engine-{}", std::process::id()));
        // sqlx checks a connection with a round trip when it goes back to
        // the pool; checking it again on every checkout too (its default)
        // doubled that. Only a connection idle long enough for the server or
        // a firewall to have dropped it, or idle since before a connection
        // was seen to break (a database restart), is checked before use.
        let health = Arc::new(Health::new(Some(opts.clone())));
        let h = health.clone();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .test_before_acquire(false)
            .before_acquire(move |conn, meta| {
                let check = meta.idle_for >= IDLE_CHECK || h.idle_across_break(meta.idle_for);
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
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Ok(Db {
            pool,
            health,
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        })
    }

    /// Whether the database can be reached ([`health`]).
    pub fn health(&self) -> &Arc<Health> {
        &self.health
    }

    /// The most connections the pool opens.
    pub fn max_connections(&self) -> u32 {
        self.pool.options().get_max_connections()
    }

    /// The sqlx pool, for tests and tools that inspect it. Engine code runs
    /// its queries through [`Query`], which handles outages.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// A `LISTEN` connection on a pool of its own.
    ///
    /// `PgListener::connect_with(&pool)` keeps one connection of the shared
    /// pool for as long as it listens. The publisher and the job workers
    /// each listen for the life of `engine serve`, so two of the pool's
    /// connections (five by default) were never available to requests,
    /// renders and jobs; under load everything else queued for the rest and
    /// a publish could wait for seconds. The listener's own one-connection
    /// pool (same settings, reconnected by sqlx when it drops) leaves the
    /// shared pool whole.
    pub async fn pg_listener(&self) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
        let own = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_lazy_with((*self.pool.connect_options()).clone());
        sqlx::postgres::PgListener::connect_with(&own).await
    }

    /// Start a transaction.
    pub async fn begin(&self) -> Result<Tx, sqlx::Error> {
        Tx::begin(self).await
    }

    /// Start a transaction whose commit does not wait for the write-ahead
    /// log to reach the disk (`SET LOCAL synchronous_commit = off`). Only
    /// for bookkeeping that can be redone: a server crash may lose its last
    /// fraction of a second, never corrupt anything. Job heartbeats, claims
    /// and results use it; content saves, revisions, accounts and orders
    /// never do.
    pub async fn begin_relaxed(&self) -> Result<Tx, sqlx::Error> {
        let mut tx = Tx::begin(self).await?;
        crate::query("SET LOCAL synchronous_commit = off")
            .execute(&mut tx)
            .await?;
        Ok(tx)
    }

    /// Hold one connection.
    pub async fn acquire(&self) -> Result<Conn, sqlx::Error> {
        Conn::acquire(self).await
    }

    pub async fn close(&self) {
        self.pool.close().await
    }
}

/// A pooled connection idle at least this long is checked with a round
/// trip before it is handed out.
const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// The connection [`Db::pinned`] holds for the work on this thread.
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
