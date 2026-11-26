//! Wrought's `jobs` feature: background jobs and schedules on PostgreSQL.
//!
//! * [`Jobs`] puts jobs in the queue (`engine_jobs`) and inspects it.
//! * [`Handlers`] maps a job `kind` (e.g. `mail.send`) to async code.
//! * [`run`] runs workers until shutdown: `engine serve` runs them next to
//!   the web server, `engine jobs work` runs them alone.
//!
//! How it stays correct with any number of workers and processes:
//!
//! * **Claiming** is one `UPDATE ... WHERE id = (SELECT ... FOR UPDATE SKIP
//!   LOCKED LIMIT 1)`: two workers never claim the same job; a worker skips
//!   rows another one is claiming instead of waiting for them.
//! * **Leases:** a claimed job is `running` until `locked_until`, which its
//!   worker extends (a heartbeat) while the handler runs. If the worker dies
//!   (crash, kill -9, lost connection), the lease runs out and any worker
//!   puts the job back in the queue; that counts as a failed attempt.
//! * **Retries:** a failed job is queued again after an exponential backoff
//!   with jitter (`retry_base_seconds` · 2^(attempt-1), capped at
//!   `retry_max_seconds`), until `max_attempts`; then it is `dead` and stays
//!   in the table for inspection (`engine jobs list --status dead`) and a
//!   manual `engine jobs retry`.
//! * **Graceful shutdown:** on shutdown workers stop claiming, let running
//!   jobs finish for `shutdown_grace_seconds`, then stop the rest and put
//!   them back in the queue without counting the attempt.
//! * **Wake-ups:** an insert trigger sends `NOTIFY engine_jobs`; workers
//!   LISTEN and also poll every `poll_seconds`, so a lost notification only
//!   delays a job.
//!
//! Delivery is *at least once*: a worker that loses its database connection
//! mid-job can have its job re-run elsewhere after the lease. Handlers
//! should be idempotent where it matters.
//!
//! `[jobs]` in `site.toml` (all optional, see [`JobsConfig`]):
//!
//! ```toml
//! [jobs]
//! workers = 4                  # jobs run at once per process
//! poll_seconds = 2
//! lease_seconds = 60
//! shutdown_grace_seconds = 30
//! retry_base_seconds = 10
//! retry_max_seconds = 3600
//!
//! [[jobs.schedule]]
//! name = "nightly-report"
//! job = "site.report"          # a handler kind
//! every = "24h"                # s, m, h or d
//! payload = { to = "office@example.org" }
//! ```

mod config;
mod queue;
mod worker;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use engine_db::{Backend, Migration};

pub use config::{JobsConfig, Schedule, parse_duration};
pub use queue::{Enqueue, JobInfo, Jobs};
pub use worker::{WorkerOptions, run, worker_id};

/// The `jobs` feature's schema.
pub fn migrations(backend: Backend) -> Vec<Migration> {
    match backend {
        Backend::Postgres => vec![Migration::new(
            "jobs/0001_jobs",
            include_str!("../migrations/postgres/0001_jobs.sql"),
        )],
        Backend::Sqlite => Vec::new(),
    }
}

/// What a handler gets.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    /// 1 on the first run.
    pub attempt: i32,
    pub max_attempts: i32,
    scrub: Arc<AtomicBool>,
}

impl JobContext {
    /// Replace the payload with `{}` once the job succeeds (for payloads
    /// that hold secrets, such as a password-reset link in an email).
    pub fn scrub_payload_on_success(&self) {
        self.scrub.store(true, Ordering::Relaxed);
    }

    fn scrubbed(&self) -> bool {
        self.scrub.load(Ordering::Relaxed)
    }
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A job handler. An `Err` (or a panic) fails the attempt; the error text
/// is stored in `last_error`.
pub type Handler = Arc<dyn Fn(JobContext) -> BoxFuture<anyhow::Result<()>> + Send + Sync>;

/// Handlers by job kind.
#[derive(Clone, Default)]
pub struct Handlers {
    map: BTreeMap<String, Handler>,
}

impl Handlers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `f` for jobs of `kind`.
    pub fn register<F, Fut>(&mut self, kind: &str, f: F) -> &mut Self
    where
        F: Fn(JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        assert!(valid_kind(kind), "invalid job kind `{kind}`");
        let f = Arc::new(f);
        self.map.insert(
            kind.to_string(),
            Arc::new(move |ctx| {
                let f = f.clone();
                Box::pin(async move { f(ctx).await })
            }),
        );
        self
    }

    pub fn kinds(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    pub fn get(&self, kind: &str) -> Option<&Handler> {
        self.map.get(kind)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Job kinds and schedule names: `[a-z][a-z0-9_.-]{0,63}`.
pub fn valid_kind(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b[1..].iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

/// Backoff before attempt `attempt + 1` after `attempt` failed:
/// base · 2^(attempt-1), capped, with ±20 % jitter from `jitter` (0..1).
pub fn backoff_seconds(base: f64, max: f64, attempt: i32, jitter: f64) -> f64 {
    let exp = 2f64.powi((attempt - 1).clamp(0, 30));
    let raw = (base * exp).min(max);
    (raw * (0.8 + 0.4 * jitter.clamp(0.0, 1.0))).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for ok in ["mail.send", "a", "site.report-2", "x_y"] {
            assert!(valid_kind(ok), "{ok}");
        }
        for bad in ["", "Mail", "1x", "a b", "a/b", &"x".repeat(65)] {
            assert!(!valid_kind(bad), "{bad}");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_seconds(10.0, 3600.0, 1, 0.5), 10.0);
        assert_eq!(backoff_seconds(10.0, 3600.0, 2, 0.5), 20.0);
        assert_eq!(backoff_seconds(10.0, 3600.0, 4, 0.5), 80.0);
        assert_eq!(backoff_seconds(10.0, 3600.0, 30, 0.5), 3600.0);
        let lo = backoff_seconds(10.0, 3600.0, 1, 0.0);
        let hi = backoff_seconds(10.0, 3600.0, 1, 1.0);
        assert!((lo - 8.0).abs() < 1e-9 && (hi - 12.0).abs() < 1e-9);
    }
}
