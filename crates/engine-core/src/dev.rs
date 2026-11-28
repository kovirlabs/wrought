//! What `engine dev` records for its developer tools: the queries each
//! request ran (with their shape, duration, rows and call site) and the
//! error behind a `500`.
//!
//! **Off unless `engine dev` turns it on.** [`enable`] is called by the
//! `engine dev` command and nothing else: there is no configuration key or
//! environment variable for it, so a site cannot ship it switched on and
//! `engine serve` never has it (DECISIONS.md, "Dev server tools"). While it
//! is off, everything here is one relaxed atomic load: [`enabled`] is
//! `false`, [`current`] is `None` without touching a task-local, a query
//! records nothing, and [`attach_error`] keeps nothing.
//!
//! A request's log is entered by `engine dev`'s layer with [`scope`] (the
//! request's future) and carried into blocking renders with [`carry`]
//! (a thread-local while the closure runs: template functions block on
//! queries on that thread).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn the developer tools on for this process. Only `engine dev` calls
/// this (see the module documentation); it cannot be turned off again.
#[doc(hidden)]
pub fn enable_for_engine_dev() {
    ENABLED.store(true, Ordering::Release);
}

/// Whether this process is `engine dev`.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// One query a request ran.
#[derive(Debug, Clone)]
pub struct QueryRecord {
    /// The statement's text (parameters are `$1`, `$2`...: this is its
    /// shape; bound values are never recorded).
    pub sql: String,
    /// The catalogue name of the statement (`engine_content::sql::GET`), or
    /// what ran it (`query:signups`), when known.
    pub name: Option<std::borrow::Cow<'static, str>>,
    /// Where the query was made, in Rust.
    pub caller: Option<&'static std::panic::Location<'static>>,
    pub duration: Duration,
    /// Rows returned (or affected, for a statement).
    pub rows: Option<u64>,
    /// The database's error, if it failed.
    pub error: Option<String>,
}

impl QueryRecord {
    /// The statement's text on one line (whitespace collapsed): what N+1
    /// detection compares.
    pub fn shape(&self) -> String {
        self.sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// `name at file:line`, or whichever is known.
    pub fn call_site(&self) -> Option<String> {
        match (self.name.as_deref(), self.caller) {
            (Some(n), Some(c)) => Some(format!("{n} at {}:{}", c.file(), c.line())),
            (Some(n), None) => Some(n.to_string()),
            (None, Some(c)) => Some(format!("{}:{}", c.file(), c.line())),
            (None, None) => None,
        }
    }
}

/// The queries of one request (shared by everything the request runs).
#[derive(Debug, Default)]
pub struct RequestLog {
    queries: Mutex<Vec<QueryRecord>>,
}

/// At most this many queries are kept per request (a runaway loop does not
/// grow the dev server without bound; the count goes on).
pub const MAX_QUERIES: usize = 2000;

impl RequestLog {
    pub fn new() -> Arc<RequestLog> {
        Arc::new(RequestLog::default())
    }

    pub fn record(&self, q: QueryRecord) {
        let mut v = self.queries.lock().unwrap_or_else(|p| p.into_inner());
        if v.len() < MAX_QUERIES {
            v.push(q);
        }
    }

    /// The queries so far, in the order they finished.
    pub fn queries(&self) -> Vec<QueryRecord> {
        self.queries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

tokio::task_local! {
    static TASK: Arc<RequestLog>;
}

thread_local! {
    static THREAD: std::cell::RefCell<Option<Arc<RequestLog>>> = const { std::cell::RefCell::new(None) };
}

/// Run `fut` with `log` as its request's log.
pub async fn scope<F: std::future::Future>(log: Arc<RequestLog>, fut: F) -> F::Output {
    TASK.scope(log, fut).await
}

/// The log of the request this code runs for, if `engine dev` is on and it
/// runs for one.
#[inline]
pub fn current() -> Option<Arc<RequestLog>> {
    if !enabled() {
        return None;
    }
    TASK.try_with(Arc::clone)
        .ok()
        .or_else(|| THREAD.with(|t| t.borrow().clone()))
}

/// `f` with the current request's log carried along, for work that runs on
/// another thread (`spawn_blocking` of a render). Without `engine dev`, `f`
/// itself.
pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    let log = current();
    move || match log {
        None => f(),
        Some(log) => {
            let prev = THREAD.with(|t| t.replace(Some(log)));
            struct Restore(Option<Arc<RequestLog>>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    let prev = self.0.take();
                    THREAD.with(|t| *t.borrow_mut() = prev);
                }
            }
            let _r = Restore(prev);
            f()
        }
    }
}

/// The error behind a `500`, kept on the response (an extension, never
/// sent) for `engine dev`'s error page.
#[derive(Clone, Debug)]
pub struct ErrorDetail(pub Arc<anyhow::Error>);

/// Keep `e` on `resp` for `engine dev`'s error page. Does nothing unless
/// `engine dev` is on: the response is what it would be anyway.
pub fn attach_error(resp: &mut axum::response::Response, e: anyhow::Error) {
    if enabled() {
        resp.extensions_mut().insert(ErrorDetail(Arc::new(e)));
    }
}

/// Queries whose shape ran at least `threshold` times: (shape, count, call
/// site of the first), most repeated first.
pub fn repeated(queries: &[QueryRecord], threshold: usize) -> Vec<(String, usize, Option<String>)> {
    let mut seen: Vec<(String, usize, Option<String>)> = Vec::new();
    for q in queries {
        let s = q.shape();
        match seen.iter_mut().find(|(k, ..)| *k == s) {
            Some(e) => e.1 += 1,
            None => seen.push((s, 1, q.call_site())),
        }
    }
    seen.retain(|(_, n, _)| *n >= threshold.max(2));
    seen.sort_by_key(|a| std::cmp::Reverse(a.1));
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(sql: &str) -> QueryRecord {
        QueryRecord {
            sql: sql.into(),
            name: None,
            caller: None,
            duration: Duration::from_millis(1),
            rows: Some(1),
            error: None,
        }
    }

    #[test]
    fn repeated_shapes_are_counted_by_their_text() {
        let v = vec![
            q("SELECT * FROM a WHERE id = $1"),
            q("SELECT *\n  FROM a WHERE id = $1"),
            q("SELECT * FROM b"),
            q("SELECT * FROM a WHERE id = $1"),
        ];
        let r = repeated(&v, 3);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "SELECT * FROM a WHERE id = $1");
        assert_eq!(r[0].1, 3);
        assert!(repeated(&v, 4).is_empty());
    }

    #[test]
    fn off_records_nothing() {
        // `enabled` is process-wide; this test never turns it on, so
        // nothing is current and nothing is attached.
        if enabled() {
            return;
        }
        assert!(current().is_none());
        let mut r = axum::response::Response::new(axum::body::Body::empty());
        attach_error(&mut r, anyhow::anyhow!("x"));
        assert!(r.extensions().get::<ErrorDetail>().is_none());
    }
}
