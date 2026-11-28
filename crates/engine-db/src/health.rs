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
    /// Put to sleep by the hosting admin's watchdog ([`crate::sleep`]):
    /// down on purpose, woken by the next demand.
    pub asleep: bool,
}

/// The sleeping database's counters ([`crate::sleep`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SleepStatus {
    /// Sleeping is on for this database.
    pub on: bool,
    pub asleep: bool,
    /// Times put to sleep, woken, and wakes that failed.
    pub sleeps: u64,
    pub wakes: u64,
    pub wake_failures: u64,
    /// The last wake: asleep to reachable, in milliseconds.
    pub last_wake_ms: Option<u64>,
    pub last_sleep_error: Option<String>,
}

/// Keeps the database awake while held ([`Health::hold_awake`]).
pub struct AwakeGuard(std::sync::Arc<Health>);

impl Drop for AwakeGuard {
    fn drop(&mut self) {
        self.0.holds.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The wake command and how long a wake may take.
#[derive(Clone)]
struct Waker(crate::sleep::Hook, Duration);

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Waker({:?})", self.1)
    }
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
    // A sleeping database (`crate::sleep`); all idle unless it is on.
    waker: std::sync::OnceLock<Waker>,
    counting: AtomicBool,
    uses: AtomicU64,
    holds: std::sync::atomic::AtomicUsize,
    asleep: AtomicBool,
    /// Being put to sleep (demands meanwhile wake it right after).
    settling: AtomicBool,
    wanted: AtomicBool,
    waking: AtomicBool,
    wake_failed_at: AtomicU64,
    sleeps: AtomicU64,
    wakes: AtomicU64,
    wake_failures: AtomicU64,
    last_wake_ms: AtomicU64,
    last_sleep_error: Mutex<Option<String>>,
    wake_done: tokio::sync::Notify,
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
            waker: std::sync::OnceLock::new(),
            counting: AtomicBool::new(false),
            uses: AtomicU64::new(0),
            holds: std::sync::atomic::AtomicUsize::new(0),
            asleep: AtomicBool::new(false),
            settling: AtomicBool::new(false),
            wanted: AtomicBool::new(false),
            waking: AtomicBool::new(false),
            wake_failed_at: AtomicU64::new(0),
            sleeps: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
            wake_failures: AtomicU64::new(0),
            last_wake_ms: AtomicU64::new(0),
            last_sleep_error: Mutex::new(None),
            wake_done: tokio::sync::Notify::new(),
        }
    }

    /// Turn sleeping on ([`crate::sleep::enable`]): from now on a demand
    /// while asleep runs `wake`, which fails after `give_up`.
    pub fn enable_sleep(&self, wake: crate::sleep::Hook, give_up: Duration) {
        let _ = self.waker.set(Waker(wake, give_up));
        self.counting.store(true, Ordering::Release);
    }

    /// Whether checkouts are counted (sleeping is on; one atomic load).
    #[inline]
    pub fn counting(&self) -> bool {
        self.counting.load(Ordering::Relaxed)
    }

    /// A checkout that is not background work.
    #[inline]
    pub(crate) fn note_use(&self) {
        self.uses.fetch_add(1, Ordering::Relaxed);
    }

    /// Checkouts counted so far (sleeping on).
    pub fn uses(&self) -> u64 {
        self.uses.load(Ordering::Relaxed)
    }

    /// Whether it was put to sleep (and not woken yet).
    #[inline]
    pub fn is_asleep(&self) -> bool {
        self.asleep.load(Ordering::Acquire)
    }

    /// Keep the database awake until the guard is dropped (a publish).
    pub fn hold_awake(self: &std::sync::Arc<Self>) -> AwakeGuard {
        self.holds.fetch_add(1, Ordering::AcqRel);
        AwakeGuard(self.clone())
    }

    pub fn holds(&self) -> usize {
        self.holds.load(Ordering::Acquire)
    }

    /// Resolves when the database is up (at once if it is).
    pub async fn wait_up(&self) {
        let _ = self.subscribe().wait_for(|u| *u).await;
    }

    /// Start putting it to sleep: from now on it counts as down, and a
    /// demand waits and wakes it once the sleep is done. `false` if it is
    /// down or asleep already.
    pub(crate) fn begin_sleep(&self) -> bool {
        if self.is_down() || self.asleep.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.settling.store(true, Ordering::Release);
        self.down_since.store(self.now_ms(), Ordering::Relaxed);
        self.down.store(true, Ordering::Release);
        self.up_tx.send_replace(false);
        true
    }

    /// The sleep is done: `asleep` (stopped) or not (still up: undo).
    pub(crate) fn end_sleep(self: &std::sync::Arc<Self>, asleep: bool) {
        if asleep {
            self.sleeps.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                Some("asleep (put to sleep when idle)".into());
        } else {
            self.asleep.store(false, Ordering::Release);
            self.broke();
            self.down.store(false, Ordering::Release);
            self.up_tx.send_replace(true);
        }
        self.settling.store(false, Ordering::Release);
        if self.wanted.swap(false, Ordering::AcqRel) {
            self.wake();
        }
    }

    pub(crate) fn note_sleep_error(&self, e: &str) {
        *self
            .last_sleep_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
    }

    /// Wake it if it is asleep: the host's wake command, then a probe
    /// every 50 ms until it answers. One wake at a time, whoever asks; after
    /// a failed one, not again for [`crate::sleep::WAKE_RETRY`].
    pub fn wake(self: &std::sync::Arc<Self>) {
        if !self.is_asleep() {
            return;
        }
        if self.settling.load(Ordering::Acquire) {
            self.wanted.store(true, Ordering::Release);
            return;
        }
        let failed = self.wake_failed_at.load(Ordering::Relaxed);
        if failed != 0
            && self.now_ms().saturating_sub(failed) < crate::sleep::WAKE_RETRY.as_millis() as u64
        {
            return;
        }
        let Some(Waker(hook, give_up)) = self.waker.get().cloned() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.waking.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        rt.spawn(async move {
            let t0 = Instant::now();
            eprintln!("database: asleep and needed: waking it");
            let r = tokio::time::timeout(give_up, async {
                hook().await?;
                while !me.connects().await {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                anyhow::Ok(())
            })
            .await;
            let failure = match r {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("wake failed: {e:#}")),
                Err(_) => Some(format!(
                    "wake failed: not reachable {} s after the wake command",
                    give_up.as_secs()
                )),
            };
            match failure {
                None => {
                    let ms = t0.elapsed().as_millis() as u64;
                    me.last_wake_ms.store(ms.max(1), Ordering::Relaxed);
                    me.wakes.fetch_add(1, Ordering::Relaxed);
                    me.wake_failed_at.store(0, Ordering::Relaxed);
                    me.asleep.store(false, Ordering::Release);
                    // Connections pooled from before the sleep are checked.
                    me.broke();
                    me.down.store(false, Ordering::Release);
                    me.up_tx.send_replace(true);
                    eprintln!("database: awake after {ms} ms");
                }
                Some(why) => {
                    me.wake_failures.fetch_add(1, Ordering::Relaxed);
                    me.wake_failed_at.store(me.now_ms(), Ordering::Relaxed);
                    eprintln!(
                        "database: {why}; published pages are still served, pages that need it \
                         answer 503, form posts are spooled; tried again on the next demand"
                    );
                    *me.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                }
            }
            me.waking.store(false, Ordering::Release);
            me.wake_done.notify_waiters();
        });
    }

    /// A demand: wake it if it is asleep and wait up to `max` for it.
    /// `true` once it is up; `false` at once after a recent failed wake.
    pub async fn wait_awake(self: &std::sync::Arc<Self>, max: Duration) -> bool {
        let done = self.wake_done.notified();
        tokio::pin!(done);
        done.as_mut().enable();
        let mut up = self.subscribe();
        self.wake();
        if !self.is_down() {
            return true;
        }
        if !self.waking.load(Ordering::Acquire) && !self.settling.load(Ordering::Acquire) {
            return false;
        }
        let _ = tokio::time::timeout(max, async {
            tokio::select! {
                _ = up.wait_for(|u| *u) => {}
                _ = &mut done => {}
            }
        })
        .await;
        !self.is_down()
    }

    pub fn sleep_status(&self) -> SleepStatus {
        let ms = self.last_wake_ms.load(Ordering::Relaxed);
        SleepStatus {
            on: self.counting(),
            asleep: self.is_asleep(),
            sleeps: self.sleeps.load(Ordering::Relaxed),
            wakes: self.wakes.load(Ordering::Relaxed),
            wake_failures: self.wake_failures.load(Ordering::Relaxed),
            last_wake_ms: (ms != 0).then_some(ms),
            last_sleep_error: self
                .last_sleep_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// One connection, closed at once: does the server answer?
    pub(crate) async fn connects(&self) -> bool {
        #[cfg(feature = "postgres")]
        if let Some(opts) = &self.opts {
            use sqlx::Connection as _;
            self.probes.fetch_add(1, Ordering::Relaxed);
            return match tokio::time::timeout(
                PROBE_TIMEOUT,
                sqlx::postgres::PgConnection::connect_with(opts),
            )
            .await
            {
                Ok(Ok(c)) => {
                    let _ = c.close().await;
                    true
                }
                _ => false,
            };
        }
        true
    }

    /// Sessions on the server other than this process's (by application
    /// name): a backup, a migration, another engine process, another site;
    /// and this process's own that are busy.
    #[cfg(feature = "postgres")]
    pub(crate) async fn other_sessions(&self) -> anyhow::Result<i64> {
        use sqlx::Connection as _;
        let Some(opts) = &self.opts else {
            return Ok(0);
        };
        let mut c = tokio::time::timeout(
            PROBE_TIMEOUT,
            sqlx::postgres::PgConnection::connect_with(opts),
        )
        .await
        .map_err(|_| anyhow::anyhow!("connecting timed out"))??;
        // Others' sessions, plus this process's that are not idle (a query
        // or a transaction under way; idle ones, LISTEN included, are ended
        // by the shutdown).
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend' \
             AND pid <> pg_backend_pid() \
             AND (coalesce(application_name, '') <> $1 OR state IS DISTINCT FROM 'idle')",
        )
        .bind(opts.get_application_name().unwrap_or(""))
        .fetch_one(&mut c)
        .await?;
        let _ = c.close().await;
        Ok(n)
    }

    #[cfg(not(feature = "postgres"))]
    pub(crate) async fn other_sessions(&self) -> anyhow::Result<i64> {
        Ok(0)
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
        // Asleep: nothing to probe for; the next demand wakes it.
        if !self.is_down() || self.is_asleep() || self.recovering.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.recovering.store(false, Ordering::Release);
            return;
        };
        let me = self.clone();
        rt.spawn(async move {
            let mut wait = RETRY_FIRST;
            while me.is_down() && !me.is_asleep() {
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
        // A health check never wakes a sleeping database.
        if self.is_asleep() {
            return false;
        }
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
            asleep: self.is_asleep(),
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
