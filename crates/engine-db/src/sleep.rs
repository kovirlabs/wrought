//! A sleeping PostgreSQL (DECISIONS.md, "Sleeping Postgres watchdog"): the
//! hosting admin can have a site's database server stopped when it has not
//! been used for a while and started again when the engine needs it.
//!
//! * **Wake on demand.** A checkout ([`crate::sql`]) that finds the database
//!   asleep runs the host's wake command once ([`crate::Health::wake`]: one
//!   wake in flight, whoever asks) and waits up to [`WAKE_WAIT`] for it;
//!   then it gets the outage answer. Background work (job polling, the
//!   publish listener's safety poll, the search check) runs [`quietly`]: it
//!   never wakes the database and never counts as use.
//! * **Sleep when idle** ([`watch`]): no checkout outside [`quietly`] for
//!   the host's idle period, no running job, no post waiting in the form
//!   spool, no publish under way ([`crate::Health::hold_awake`]), no
//!   connection of this pool in use and no other session on the server (a
//!   backup, a migration, another engine process, another site): then the
//!   next due job is recorded in the state file and the host's sleep
//!   command is run (a clean shutdown; the shipped hooks never kill).
//! * **Jobs** keep their times: while asleep the watchdog wakes the
//!   database when the next job is due, and an engine started later (the
//!   wake timer) reads the due time from the state file instead of asking
//!   the database.

use std::cell::Cell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::Db;

/// What a [`Hook`] returns.
pub type HookFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// A host command (wake or sleep), run as a future.
pub type Hook = Arc<dyn Fn() -> HookFuture + Send + Sync>;

/// What a [`NextDue`] returns.
pub type DueFuture = Pin<Box<dyn Future<Output = Result<Option<i64>>> + Send>>;

/// When the next job is due (Unix seconds), read from the database.
pub type NextDue = Arc<dyn Fn() -> DueFuture + Send + Sync>;

/// How long a request that needs a sleeping database waits for the wake
/// before it gets the outage answer (the same bound as during the start,
/// `engine_core::serve::START_WAIT`).
pub const WAKE_WAIT: Duration = Duration::from_secs(10);

/// After a failed wake, demands within this long get the outage answer at
/// once instead of running the wake command again.
pub const WAKE_RETRY: Duration = Duration::from_secs(10);

/// The longest a sleep command may take.
pub const SLEEP_TIMEOUT: Duration = Duration::from_secs(300);

/// The watchdog's settings (the hosting admin's, `engine_core::host`).
pub struct Sleeper {
    /// No use for this long: sleep.
    pub idle: Duration,
    pub wake: Hook,
    pub sleep: Hook,
    /// A wake that has not made the database reachable after this long
    /// failed.
    pub wake_timeout: Duration,
    /// Work that keeps the database awake (running jobs, spooled posts).
    pub busy: Arc<dyn Fn() -> bool + Send + Sync>,
    pub next_due: NextDue,
    /// Where the sleep is recorded (`<site>/.engine/db-sleep.json`).
    pub state_file: PathBuf,
}

/// What the state file records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub asleep: bool,
    /// Unix seconds.
    pub since: i64,
    /// The next job's due time (Unix seconds), if any.
    pub next_due: Option<i64>,
    pub last_wake_error: Option<String>,
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The state file, if there is a readable one.
pub fn read_state(path: &Path) -> Option<State> {
    let b = std::fs::read(path).ok()?;
    serde_json::from_slice(&b).ok()
}

fn write_state(path: &Path, s: &State) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(&serde_json::to_vec(s)?)
        .and_then(|_| f.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("naming {}", path.display()))
}

/// Record a failed wake in the state file (for `engine doctor`).
pub fn note_wake_error(path: &Path, e: &str) {
    if let Some(mut st) = read_state(path) {
        st.last_wake_error = Some(e.to_string());
        let _ = write_state(path, &st);
    }
}

fn remove_state(path: &Path) {
    let _ = std::fs::remove_file(path);
}

tokio::task_local! {
    static QUIET: ();
}

thread_local! {
    static QUIET_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` as background work: its queries never wake a sleeping database
/// (they get the outage error at once) and do not count as use.
pub async fn quietly<F: Future>(f: F) -> F::Output {
    QUIET.scope((), f).await
}

/// [`quietly`] for blocking work on this thread.
pub fn quietly_blocking<R>(f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            QUIET_THREAD.with(|q| q.set(self.0));
        }
    }
    let _reset = Reset(QUIET_THREAD.with(|q| q.replace(true)));
    f()
}

/// Whether the current work runs [`quietly`].
pub(crate) fn is_quiet() -> bool {
    QUIET.try_with(|_| ()).is_ok() || QUIET_THREAD.with(|q| q.get())
}

/// A command from the host's settings (`argv[0]` and its arguments, no
/// shell), as a [`Hook`]: an error if it cannot be started, exits non-zero
/// or runs longer than `timeout` (it is then stopped; a stop already asked
/// of a service manager goes on).
pub fn command(argv: Vec<String>, timeout: Duration, what: &'static str) -> Hook {
    let argv = Arc::new(argv);
    Arc::new(move || -> HookFuture {
        let argv = argv.clone();
        Box::pin(async move {
            let (prog, args) = argv
                .split_first()
                .with_context(|| format!("the {what} command is empty"))?;
            let mut child = std::process::Command::new(prog)
                .args(args)
                .stdin(std::process::Stdio::null())
                .spawn()
                .with_context(|| format!("starting the {what} command `{prog}`"))?;
            // Polled (10 ms) rather than waited for on a thread, so a
            // command that hangs is stopped at the timeout.
            let deadline = Instant::now() + timeout;
            loop {
                match child.try_wait() {
                    Ok(Some(st)) if st.success() => return Ok(()),
                    Ok(Some(st)) => anyhow::bail!("the {what} command `{prog}` failed ({st})"),
                    Ok(None) => {}
                    Err(e) => {
                        return Err(e).with_context(|| format!("the {what} command `{prog}`"));
                    }
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "the {what} command `{prog}` did not finish in {} s",
                        timeout.as_secs()
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    })
}

/// Turn sleeping on for `db` (PostgreSQL): demands wake it from now on.
pub fn enable(db: &Db, s: &Sleeper) {
    db.health().enable_sleep(s.wake.clone(), s.wake_timeout);
}

/// Put the database to sleep now if nothing needs it (the engine's idle
/// exit: it is the database's only user). `Ok(true)` if it was.
pub async fn sleep_now(db: &Db, s: &Sleeper) -> Result<bool> {
    if db.health().is_down() {
        return Ok(false);
    }
    Ok(try_sleep(db, s).await?.is_some())
}

/// Sleep if nothing needs the database: `Some(next due)` if it was put to
/// sleep, `None` if not now.
async fn try_sleep(db: &Db, s: &Sleeper) -> Result<Option<Option<i64>>> {
    let h = db.health();
    if h.holds() > 0 || (s.busy)() {
        return Ok(None);
    }
    let due = quietly((s.next_due)()).await?;
    let now = unix_now();
    if due.is_some_and(|d| d <= now) {
        // Due now: the job workers take it first.
        return Ok(None);
    }
    if !h.begin_sleep() {
        return Ok(None);
    }
    let r: Result<bool> = async {
        // Nobody else on the server, and none of this process's sessions
        // in a query or a transaction (a request that started a moment
        // ago). The pool's own count is no guide: a connection goes back
        // to it a moment after its query.
        let others = h.other_sessions().await?;
        if others > 0 {
            eprintln!(
                "database: idle, but not put to sleep: {others} other or busy session(s) on the \
                 server (a backup, a migration, another engine process or site)"
            );
            return Ok(false);
        }
        write_state(
            &s.state_file,
            &State {
                asleep: true,
                since: now,
                next_due: due,
                last_wake_error: None,
            },
        )?;
        (s.sleep)().await?;
        Ok(true)
    }
    .await;
    match r {
        Ok(true) => {
            h.end_sleep(true);
            let next = match due {
                Some(d) => format!("; the next job is due in {} s", d - now),
                None => String::new(),
            };
            eprintln!(
                "database: unused for {} s: put to sleep{next}",
                s.idle.as_secs()
            );
            Ok(Some(due))
        }
        Ok(false) => {
            h.end_sleep(false);
            Ok(None)
        }
        Err(e) => {
            // The command failed: if the server still answers, carry on;
            // if it is gone after all, it is asleep (a demand wakes it).
            let gone = !h.connects().await;
            if !gone {
                remove_state(&s.state_file);
            }
            h.end_sleep(gone);
            h.note_sleep_error(&format!("{e:#}"));
            Err(e.context("putting the database to sleep"))
        }
    }
}

/// The watchdog: until `shutdown`, put the database to sleep after the
/// idle period, keep the state file, and wake it when a job is due.
pub async fn watch(db: Db, s: Arc<Sleeper>, shutdown: impl Future<Output = ()>) {
    let h = db.health().clone();
    let step = (s.idle / 10).clamp(Duration::from_millis(100), Duration::from_secs(5));
    let mut seen = h.uses();
    let mut since = Instant::now();
    let mut due: Option<i64> = read_state(&s.state_file).and_then(|st| st.next_due);
    let mut failures = h.sleep_status().wake_failures;
    tokio::pin!(shutdown);
    loop {
        if h.is_asleep() {
            let mut up = h.subscribe();
            let until_due = due.map(|d| Duration::from_secs((d - unix_now()).max(0) as u64));
            tokio::select! {
                _ = &mut shutdown => return,
                _ = up.wait_for(|u| *u) => {}
                _ = tokio::time::sleep(until_due.unwrap_or(Duration::MAX)), if until_due.is_some() => {
                    eprintln!("database: a scheduled job is due: waking it");
                    // Tried again in a minute if this wake fails.
                    due = Some(unix_now() + 60);
                    h.wake();
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            let st = h.sleep_status();
            if st.wake_failures != failures {
                failures = st.wake_failures;
                if let Some(e) = h.status().last_error {
                    note_wake_error(&s.state_file, &e);
                }
            }
            if !h.is_asleep() {
                remove_state(&s.state_file);
                due = None;
                seen = h.uses();
                since = Instant::now();
            }
            continue;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tokio::time::sleep(step) => {}
        }
        let u = h.uses();
        if u != seen || h.is_down() || h.holds() > 0 || (s.busy)() {
            seen = u;
            since = Instant::now();
            continue;
        }
        if since.elapsed() >= s.idle {
            match try_sleep(&db, &s).await {
                Ok(Some(d)) => due = d,
                Ok(None) => {}
                Err(e) => eprintln!("database: {e:#}"),
            }
            seen = h.uses();
            since = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quiet_work_is_marked() {
        assert!(!is_quiet());
        assert!(quietly(async { is_quiet() }).await);
        assert!(quietly_blocking(is_quiet));
        assert!(!is_quiet());
    }

    #[tokio::test]
    async fn commands_report_failure() {
        let ok = command(vec!["true".into()], Duration::from_secs(5), "wake");
        assert!(ok().await.is_ok());
        let bad = command(vec!["false".into()], Duration::from_secs(5), "wake");
        assert!(bad().await.is_err());
        let slow = command(
            vec!["sleep".into(), "5".into()],
            Duration::from_millis(100),
            "sleep",
        );
        assert!(
            slow()
                .await
                .unwrap_err()
                .to_string()
                .contains("did not finish")
        );
        let missing = command(
            vec!["/nonexistent/x".into()],
            Duration::from_secs(1),
            "wake",
        );
        assert!(missing().await.is_err());
    }

    #[test]
    fn state_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("db-sleep.json");
        assert_eq!(read_state(&p), None);
        let s = State {
            asleep: true,
            since: 5,
            next_due: Some(9),
            last_wake_error: None,
        };
        write_state(&p, &s).unwrap();
        assert_eq!(read_state(&p), Some(s));
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
