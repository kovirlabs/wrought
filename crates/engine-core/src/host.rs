//! Settings of the hosting admin, not of a site's owner (DECISIONS.md,
//! "Start on demand", "Sleeping Postgres watchdog"): whether the engine
//! exits when idle, and whether the site's PostgreSQL is put to sleep when
//! unused. They come from the environment the service runs with, or from
//! the host's file (`/etc/hammerwright/host.toml`, or `ENGINE_HOST_CONFIG`);
//! never from a site's `site.toml`, so a site cannot turn them on or off
//! for itself. Engine and database are switched separately.
//!
//! ```toml
//! [defaults]
//! engine_idle_exit_seconds = 0      # 0: never (the default)
//! db_sleep_seconds = 0              # 0: never (the default)
//! # {site} is replaced by ENGINE_SITE_ID
//! db_wake_command  = ["/usr/local/lib/hammerwright/hammerwright-db-hook", "wake",  "systemd:hammerwright-pg@{site}.service"]
//! db_sleep_command = ["/usr/local/lib/hammerwright/hammerwright-db-hook", "sleep", "systemd:hammerwright-pg@{site}.service"]
//! live_keeps_awake = false          # open live-region streams count as use
//!
//! [sites.blog]                      # by ENGINE_SITE_ID
//! engine_idle_exit_seconds = 300
//! db_sleep_seconds = 600
//! ```
//!
//! `ENGINE_IDLE_EXIT_SECONDS` in the environment wins over the file. A
//! `db_sleep_seconds` in `[defaults]` does not apply to an engine of the
//! `app` preset (web apps keep their database running): such a site sleeps
//! only with a `[sites.<id>]` entry of its own.
//!
//! Open live-region streams (server-sent events, the `realtime` feature)
//! do not keep the engine or its database awake past the quiet period
//! unless `live_keeps_awake = true` (DECISIONS.md, "Real-time").

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Seconds of quiet before the engine exits (overrides the file).
pub const IDLE_EXIT_ENV: &str = "ENGINE_IDLE_EXIT_SECONDS";
/// The host file's path (default `/etc/hammerwright/host.toml`).
pub const HOST_CONFIG_ENV: &str = "ENGINE_HOST_CONFIG";
/// This site's name in the host file (the systemd instance name).
pub const SITE_ID_ENV: &str = "ENGINE_SITE_ID";

const DEFAULT_PATH: &str = "/etc/hammerwright/host.toml";

/// A wake that has not made the database reachable after this long failed
/// (unless `db_wake_timeout_seconds` says otherwise).
pub const DEFAULT_WAKE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostFile {
    #[serde(default)]
    defaults: Level,
    #[serde(default)]
    sites: std::collections::BTreeMap<String, Level>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Level {
    engine_idle_exit_seconds: Option<u64>,
    db_sleep_seconds: Option<u64>,
    db_wake_command: Option<Vec<String>>,
    db_sleep_command: Option<Vec<String>>,
    db_wake_timeout_seconds: Option<u64>,
    live_keeps_awake: Option<bool>,
}

/// A sleeping database, as the hosting admin set it for this site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSleep {
    /// Unused this long: put to sleep.
    pub idle: Duration,
    /// Starts the server (argv, no shell).
    pub wake: Vec<String>,
    /// Stops it cleanly.
    pub sleep: Vec<String>,
    pub wake_timeout: Duration,
}

fn host_file() -> Result<Option<(PathBuf, String)>> {
    let path = std::env::var_os(HOST_CONFIG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH));
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(Some((path, t))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The idle exit the hosting admin set for this process (`None`: never).
pub fn engine_idle_exit() -> Result<Option<Duration>> {
    if let Ok(v) = std::env::var(IDLE_EXIT_ENV)
        && !v.is_empty()
    {
        let s: u64 = v
            .parse()
            .with_context(|| format!("{IDLE_EXIT_ENV}={v}: expected seconds"))?;
        return Ok((s > 0).then(|| Duration::from_secs(s)));
    }
    let Some((path, text)) = host_file()? else {
        return Ok(None);
    };
    let site = std::env::var(SITE_ID_ENV).ok();
    idle_exit_from(&text, site.as_deref()).with_context(|| format!("in {}", path.display()))
}

fn idle_exit_from(text: &str, site: Option<&str>) -> Result<Option<Duration>> {
    let f: HostFile = toml::from_str(text)?;
    let s = site
        .and_then(|id| f.sites.get(id))
        .and_then(|l| l.engine_idle_exit_seconds)
        .or(f.defaults.engine_idle_exit_seconds)
        .unwrap_or(0);
    Ok((s > 0).then(|| Duration::from_secs(s)))
}

/// The sleeping database the hosting admin set for this site (`None`:
/// off, the default). Read from the host file only.
pub fn db_sleep() -> Result<Option<DbSleep>> {
    let Some((path, text)) = host_file()? else {
        return Ok(None);
    };
    let site = std::env::var(SITE_ID_ENV).ok();
    db_sleep_from(&text, site.as_deref(), crate::features::PRESET == "app")
        .with_context(|| format!("in {}", path.display()))
}

fn db_sleep_from(text: &str, site: Option<&str>, app: bool) -> Result<Option<DbSleep>> {
    let f: HostFile = toml::from_str(text)?;
    let own = site.and_then(|id| f.sites.get(id));
    let d = &f.defaults;
    // Web apps keep their database: the host-wide default does not apply.
    let secs = own
        .and_then(|l| l.db_sleep_seconds)
        .or(if app { None } else { d.db_sleep_seconds })
        .unwrap_or(0);
    if secs == 0 {
        return Ok(None);
    }
    let id = site.unwrap_or("");
    let cmd = |pick: fn(&Level) -> &Option<Vec<String>>, key: &str| -> Result<Vec<String>> {
        let v = own
            .and_then(|l| pick(l).clone())
            .or_else(|| pick(d).clone())
            .filter(|v| !v.is_empty())
            .with_context(|| format!("db_sleep_seconds is set, but {key} is not"))?;
        if v.iter().any(|a| a.contains("{site}")) && id.is_empty() {
            anyhow::bail!("{key} uses {{site}}, but {SITE_ID_ENV} is not set");
        }
        Ok(v.into_iter().map(|a| a.replace("{site}", id)).collect())
    };
    let timeout = own
        .and_then(|l| l.db_wake_timeout_seconds)
        .or(d.db_wake_timeout_seconds)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAKE_TIMEOUT)
        .max(Duration::from_secs(1));
    Ok(Some(DbSleep {
        idle: Duration::from_secs(secs),
        wake: cmd(|l| &l.db_wake_command, "db_wake_command")?,
        sleep: cmd(|l| &l.db_sleep_command, "db_sleep_command")?,
        wake_timeout: timeout,
    }))
}

/// Whether open live-region streams keep this site's engine and database
/// awake (the hosting admin's `live_keeps_awake`; off by default).
pub fn live_keeps_awake() -> Result<bool> {
    let Some((path, text)) = host_file()? else {
        return Ok(false);
    };
    let site = std::env::var(SITE_ID_ENV).ok();
    live_keeps_awake_from(&text, site.as_deref()).with_context(|| format!("in {}", path.display()))
}

fn live_keeps_awake_from(text: &str, site: Option<&str>) -> Result<bool> {
    let f: HostFile = toml::from_str(text)?;
    Ok(site
        .and_then(|id| f.sites.get(id))
        .and_then(|l| l.live_keeps_awake)
        .or(f.defaults.live_keeps_awake)
        .unwrap_or(false))
}

static IDLE_EXITED: AtomicBool = AtomicBool::new(false);

/// Whether the server stopped for its idle exit (not a signal).
pub fn idle_exited() -> bool {
    IDLE_EXITED.load(Ordering::Acquire)
}

pub(crate) fn note_idle_exit() {
    IDLE_EXITED.store(true, Ordering::Release);
}

/// Resolves after `quiet` with no request (counted by the server) while
/// `busy` (running jobs) said nothing was running.
pub async fn idle(quiet: Duration, busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>) {
    let step = (quiet / 10).clamp(Duration::from_millis(100), Duration::from_secs(5));
    let mut tick = tokio::time::interval(step);
    tick.tick().await;
    let mut seen = crate::memory::requests();
    let mut since = tokio::time::Instant::now();
    loop {
        tick.tick().await;
        let now = crate::memory::requests();
        if now != seen || busy.as_ref().is_some_and(|b| b()) {
            seen = now;
            since = tokio::time::Instant::now();
        } else if since.elapsed() >= quiet {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_level_wins_over_defaults() {
        let t = "[defaults]\nengine_idle_exit_seconds = 600\n[sites.blog]\nengine_idle_exit_seconds = 0\n[sites.shop]\nengine_idle_exit_seconds = 30\n";
        assert_eq!(idle_exit_from(t, Some("blog")).unwrap(), None);
        assert_eq!(
            idle_exit_from(t, Some("shop")).unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            idle_exit_from(t, Some("other")).unwrap(),
            Some(Duration::from_secs(600))
        );
        assert_eq!(idle_exit_from("", None).unwrap(), None);
        assert!(idle_exit_from("[defaults]\nnonsense = 1\n", None).is_err());
    }

    #[test]
    fn database_sleep_per_site_and_never_by_default_for_apps() {
        let t = r#"
[defaults]
db_sleep_seconds = 600
db_wake_command = ["hook", "wake", "systemd:hammerwright-pg@{site}.service"]
db_sleep_command = ["hook", "sleep", "systemd:hammerwright-pg@{site}.service"]
[sites.blog]
db_wake_timeout_seconds = 20
[sites.shop]
db_sleep_seconds = 0
[sites.app]
db_sleep_seconds = 120
"#;
        let blog = db_sleep_from(t, Some("blog"), false).unwrap().unwrap();
        assert_eq!(blog.idle, Duration::from_secs(600));
        assert_eq!(
            blog.wake,
            ["hook", "wake", "systemd:hammerwright-pg@blog.service"]
        );
        assert_eq!(blog.sleep[1], "sleep");
        assert_eq!(blog.wake_timeout, Duration::from_secs(20));
        assert_eq!(db_sleep_from(t, Some("shop"), false).unwrap(), None);
        // The app preset: not from [defaults], only from its own entry.
        assert_eq!(db_sleep_from(t, Some("blog"), true).unwrap(), None);
        let app = db_sleep_from(t, Some("app"), true).unwrap().unwrap();
        assert_eq!(app.idle, Duration::from_secs(120));
        assert_eq!(app.wake_timeout, DEFAULT_WAKE_TIMEOUT);
        // Off unless set; on without commands is an error.
        assert_eq!(db_sleep_from("", Some("blog"), false).unwrap(), None);
        assert!(db_sleep_from("[defaults]\ndb_sleep_seconds = 5\n", None, false).is_err());
        assert!(
            db_sleep_from(t, None, false).is_err(),
            "{{site}} needs the id"
        );
    }

    #[test]
    fn live_streams_keep_nothing_awake_unless_set() {
        let t = "[defaults]\nlive_keeps_awake = true\n[sites.blog]\nlive_keeps_awake = false\n";
        assert!(live_keeps_awake_from(t, Some("app")).unwrap());
        assert!(!live_keeps_awake_from(t, Some("blog")).unwrap());
        assert!(!live_keeps_awake_from("", None).unwrap());
    }

    #[tokio::test]
    async fn idle_waits_for_quiet_and_for_busy_work() {
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let b = busy.clone();
        let f = tokio::spawn(idle(
            Duration::from_millis(300),
            Some(Arc::new(move || {
                b.load(std::sync::atomic::Ordering::Relaxed)
            })),
        ));
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!f.is_finished(), "a running job keeps it awake");
        busy.store(false, std::sync::atomic::Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(3), f)
            .await
            .expect("idle once the job is done")
            .unwrap();
    }
}
