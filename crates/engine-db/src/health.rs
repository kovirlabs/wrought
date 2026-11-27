//! Surviving a database restart or outage (DECISIONS.md, "Survive a
//! sleeping database").
//!
//! The pool does not check a connection before handing it out (that cost a
//! round trip per query), so after PostgreSQL restarts it still holds
//! connections to the old server. Each [`crate::Db`] has a [`Health`]:
//!
//! * a query that fails because its connection broke ([`disconnect`])
//!   records the break; from then on a pooled connection that sat idle
//!   across the break is checked with one round trip before use and
//!   dropped if dead (connections used since are not checked again);
//! * the failed query is retried on a fresh connection when that is safe:
//!   reads always, writes only when the server said the session ended
//!   before the statement ran ([`Disconnect::NotRun`]); an ambiguous
//!   failure of a write is returned to the caller;
//! * before retrying, one probe connection (shared by every query that saw
//!   the same break) tells whether the server is there at all. If not,
//!   the database is marked down: queries fail at once with an
//!   [`unavailable`] error instead of waiting for the pool's timeout, and a
//!   background task probes until the server answers again, then tells
//!   [`Health::subscribe`] watchers (the form spool's replay).
//!
//! Nothing here runs on the normal path except one atomic load per query
//! (and one per checkout while a break is recent).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long one probe may take to connect.
#[cfg(feature = "postgres")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Between probes while the database is down (first, then at most).
const RETRY_FIRST: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(2);

/// How a query's error relates to its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    /// The session had ended before the statement could run (PostgreSQL's
    /// `57P01` admin shutdown, `57P02` crash shutdown, `57P03` cannot
    /// connect now; a refused connection): safe to run anything again.
    NotRun,
    /// The connection broke while the statement may have been running: safe
    /// to retry only a read.
    Ambiguous,
}

/// Whether `e` means the connection (or the server) went away.
pub fn disconnect(e: &sqlx::Error) -> Option<Disconnect> {
    use std::io::ErrorKind as K;
    match e {
        sqlx::Error::Io(io) if is_unavailable_io(io) => None,
        sqlx::Error::Io(io) => Some(match io.kind() {
            K::ConnectionRefused | K::NotConnected | K::BrokenPipe => Disconnect::NotRun,
            _ => Disconnect::Ambiguous,
        }),
        sqlx::Error::Database(d) => match d.code().as_deref() {
            Some("57P01" | "57P02" | "57P03") => Some(Disconnect::NotRun),
            Some(c) if c.starts_with("08") => Some(Disconnect::Ambiguous),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `sql` only reads (a `SELECT` or `WITH` with no data-modifying
/// part), so running it twice is harmless. Only called after a failure.
pub fn is_read(sql: &str) -> bool {
    let s = sql.trim_start().to_ascii_lowercase();
    (s.starts_with("select") || s.starts_with("with"))
        && !["insert ", "update ", "delete ", "nextval(", "pg_notify("]
            .iter()
            .any(|w| s.contains(w))
}

#[derive(Debug)]
struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the database is unavailable")
    }
}

impl std::error::Error for Unavailable {}

/// The error of a query refused because the database is down.
pub fn unavailable() -> sqlx::Error {
    sqlx::Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        Unavailable,
    ))
}

fn is_unavailable_io(io: &std::io::Error) -> bool {
    io.get_ref().is_some_and(|e| e.is::<Unavailable>())
}

/// Whether `e` is [`unavailable`] or another sign that the database cannot
/// be reached (for answering `503` instead of `500`).
pub fn is_unavailable(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(io) => is_unavailable_io(io),
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        other => disconnect(other).is_some(),
    }
}

/// [`is_unavailable`] anywhere in an error's chain (through template errors
/// too: `engine_core::templates::fn_error` keeps the cause as the source).
pub fn is_unavailable_any(e: &anyhow::Error) -> bool {
    let e: &(dyn std::error::Error + 'static) = e.as_ref();
    is_unavailable_err(e)
}

/// [`is_unavailable`] anywhere in the chain of a `std` error, e.g. a
/// `minijinja::Error` from rendering a template
/// ([`engine_core::templates::any_cause`]).
pub fn is_unavailable_err(e: &(dyn std::error::Error + 'static)) -> bool {
    engine_core::templates::any_cause(e, &mut is_unavailable_link)
}

fn is_unavailable_link(e: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(s) = e.downcast_ref::<sqlx::Error>() {
        return is_unavailable(s);
    }
    // The parts of a `sqlx::Error` seen without it: a wrapper declared
    // `#[error(transparent)]` hands on its inner error's *source*, so the
    // chain can start below the `sqlx::Error`.
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return is_unavailable_io(io);
    }
    #[cfg(feature = "postgres")]
    if let Some(d) = e.downcast_ref::<sqlx::postgres::PgDatabaseError>() {
        let c = d.code();
        return matches!(c, "57P01" | "57P02" | "57P03") || c.starts_with("08");
    }
    e.is::<Unavailable>()
}

/// What [`Health::status`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub up: bool,
    /// How long it has been down.
    pub down_for: Option<Duration>,
    /// Outages seen since the engine started.
    pub outages: u64,
    /// Probe connections made since the engine started.
    pub probes: u64,
    pub last_error: Option<String>,
}

/// A database's reachability, shared by its pool's clones.
#[derive(Debug)]
pub struct Health {
    origin: Instant,
    /// Milliseconds since `origin` of the last broken connection (0: none).
    broken_at: AtomicU64,
    down: AtomicBool,
    down_since: AtomicU64,
    outages: AtomicU64,
    probes: AtomicU64,
    recovering: AtomicBool,
    last_error: Mutex<Option<String>>,
    /// `true` while up.
    up_tx: tokio::sync::watch::Sender<bool>,
    /// Serialises probes; holds when the last one finished (ms).
    probe: tokio::sync::Mutex<u64>,
    #[cfg(feature = "postgres")]
    opts: Option<sqlx::postgres::PgConnectOptions>,
}

#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
impl Health {
    /// For a database that cannot go away on its own (SQLite).
    #[cfg(any(feature = "sqlite", test))]
    pub(crate) fn local() -> Health {
        Health::new(
            #[cfg(feature = "postgres")]
            None,
        )
    }

    pub(crate) fn new(
        #[cfg(feature = "postgres")] opts: Option<sqlx::postgres::PgConnectOptions>,
    ) -> Health {
        Health {
            origin: Instant::now(),
            broken_at: AtomicU64::new(0),
            down: AtomicBool::new(false),
            down_since: AtomicU64::new(0),
            outages: AtomicU64::new(0),
            probes: AtomicU64::new(0),
            recovering: AtomicBool::new(false),
            last_error: Mutex::new(None),
            up_tx: tokio::sync::watch::channel(true).0,
            probe: tokio::sync::Mutex::new(0),
            #[cfg(feature = "postgres")]
            opts,
        }
    }

    fn now_ms(&self) -> u64 {
        // +1 so that 0 keeps meaning "never".
        self.origin.elapsed().as_millis() as u64 + 1
    }

    /// Whether the database is known to be down (one atomic load).
    #[inline]
    pub fn is_down(&self) -> bool {
        self.down.load(Ordering::Relaxed)
    }

    /// Record that a connection broke now.
    fn broke(&self) {
        self.broken_at.store(self.now_ms(), Ordering::Relaxed);
    }

    /// Whether a pooled connection idle for `idle_for` has been idle since
    /// before the last break (and so must be checked before use).
    #[inline]
    pub fn idle_across_break(&self, idle_for: Duration) -> bool {
        let b = self.broken_at.load(Ordering::Relaxed);
        b != 0 && self.now_ms().saturating_sub(idle_for.as_millis() as u64) <= b
    }

    /// After a broken connection seen at `seen` ([`Health::mark`]): is the
    /// server there? One probe answers for every caller that saw the same
    /// break; a failed probe marks the database down.
    pub async fn recover(self: &std::sync::Arc<Self>, seen: u64) -> bool {
        let mut last = self.probe.lock().await;
        if *last >= seen {
            return !self.is_down();
        }
        let ok = self.probe_once().await;
        *last = self.now_ms();
        drop(last);
        if !ok {
            self.watch_recovery();
        }
        ok
    }

    /// Record a break and return its time, for [`Health::recover`].
    pub fn mark(&self) -> u64 {
        let t = self.now_ms();
        self.broken_at.store(t, Ordering::Relaxed);
        t
    }

    async fn probe_once(&self) -> bool {
        #[cfg(feature = "postgres")]
        if let Some(opts) = &self.opts {
            use sqlx::Connection as _;
            self.probes.fetch_add(1, Ordering::Relaxed);
            let r = tokio::time::timeout(
                PROBE_TIMEOUT,
                sqlx::postgres::PgConnection::connect_with(opts),
            )
            .await;
            return match r {
                Ok(Ok(c)) => {
                    let _ = c.close().await;
                    self.set_up();
                    true
                }
                Ok(Err(e)) => {
                    self.set_down(&e.to_string());
                    false
                }
                Err(_) => {
                    self.set_down("connecting timed out");
                    false
                }
            };
        }
        true
    }

    fn set_up(&self) {
        if self.down.swap(false, Ordering::AcqRel) {
            // Connections still pooled from before are checked before use.
            self.broke();
            let since = self.down_since.load(Ordering::Relaxed);
            let secs = self.now_ms().saturating_sub(since) as f64 / 1000.0;
            eprintln!("database: reachable again (down {secs:.1} s)");
            self.up_tx.send_replace(true);
        }
    }

    fn set_down(&self, why: &str) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
        if !self.down.swap(true, Ordering::AcqRel) {
            self.down_since.store(self.now_ms(), Ordering::Relaxed);
            self.outages.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "database: unreachable ({why}); published pages are still served, pages that \
                 need it answer 503 until it is back"
            );
            self.up_tx.send_replace(false);
        }
    }

    /// Probe in the background until the database answers (started when it
    /// is marked down; one task at a time).
    pub(crate) fn watch_recovery(self: &std::sync::Arc<Self>) {
        if !self.is_down() || self.recovering.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.recovering.store(false, Ordering::Release);
            return;
        };
        let me = self.clone();
        rt.spawn(async move {
            let mut wait = RETRY_FIRST;
            while me.is_down() {
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RETRY_MAX);
                let mut last = me.probe.lock().await;
                if me.is_down() {
                    me.probe_once().await;
                    *last = me.now_ms();
                }
            }
            me.recovering.store(false, Ordering::Release);
            // Went down again between the last probe and here.
            me.watch_recovery();
        });
    }

    /// Probe now, unless a probe finished less than `min` ago (then its
    /// answer stands): the health endpoint's `?check`, which anyone may
    /// call, cannot make more than one connection per `min`.
    pub async fn check_recent(self: &std::sync::Arc<Self>, min: Duration) -> bool {
        let mut last = self.probe.lock().await;
        if *last != 0 && self.now_ms().saturating_sub(*last) < min.as_millis() as u64 {
            return !self.is_down();
        }
        let ok = self.probe_once().await;
        *last = self.now_ms();
        drop(last);
        if !ok {
            self.watch_recovery();
        }
        ok
    }

    /// `true` while up; changes when the database goes down or comes back.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.up_tx.subscribe()
    }

    pub fn status(&self) -> Status {
        let down = self.is_down();
        Status {
            up: !down,
            down_for: down.then(|| {
                Duration::from_millis(
                    self.now_ms()
                        .saturating_sub(self.down_since.load(Ordering::Relaxed)),
                )
            }),
            outages: self.outages.load(Ordering::Relaxed),
            probes: self.probes.load(Ordering::Relaxed),
            last_error: self
                .last_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes() {
        assert!(is_read("SELECT 1"));
        assert!(is_read("  with x AS (SELECT 1) SELECT * FROM x"));
        assert!(!is_read("INSERT INTO t VALUES (1)"));
        assert!(!is_read(
            "WITH d AS (DELETE FROM t RETURNING id) SELECT * FROM d"
        ));
        assert!(!is_read("UPDATE t SET a = 1"));
        assert!(!is_read("SELECT nextval('s')"));
    }

    #[test]
    fn unavailable_is_recognised() {
        let e = unavailable();
        assert!(is_unavailable(&e));
        assert_eq!(disconnect(&e), None);
        let any = anyhow::Error::new(unavailable()).context("rendering");
        assert!(is_unavailable_any(&any));
        let refused = sqlx::Error::Io(std::io::ErrorKind::ConnectionRefused.into());
        assert_eq!(disconnect(&refused), Some(Disconnect::NotRun));
        let eof = sqlx::Error::Io(std::io::ErrorKind::UnexpectedEof.into());
        assert_eq!(disconnect(&eof), Some(Disconnect::Ambiguous));
        assert_eq!(disconnect(&sqlx::Error::RowNotFound), None);
    }

    #[test]
    fn idle_across_break() {
        let h = Health::local();
        assert!(!h.idle_across_break(Duration::from_secs(100)));
        std::thread::sleep(Duration::from_millis(5));
        h.broke();
        // Idle since before the break: check it.
        assert!(h.idle_across_break(Duration::from_secs(100)));
        std::thread::sleep(Duration::from_millis(5));
        // Released after the break: no check.
        assert!(!h.idle_across_break(Duration::from_millis(1)));
    }
}
