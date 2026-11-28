//! Settings of the hosting admin, not of a site's owner (DECISIONS.md,
//! "Start on demand"): whether the engine exits when idle. They come from
//! the environment the service runs with, or from the host's file
//! (`/etc/wrought/host.toml`, or `ENGINE_HOST_CONFIG`); never from a site's
//! `site.toml`, so a site cannot turn them on or off for itself.
//!
//! ```toml
//! [defaults]
//! engine_idle_exit_seconds = 0      # 0: never (the default)
//!
//! [sites.blog]                      # by ENGINE_SITE_ID
//! engine_idle_exit_seconds = 300
//! ```
//!
//! `ENGINE_IDLE_EXIT_SECONDS` in the environment wins over the file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Seconds of quiet before the engine exits (overrides the file).
pub const IDLE_EXIT_ENV: &str = "ENGINE_IDLE_EXIT_SECONDS";
/// The host file's path (default `/etc/wrought/host.toml`).
pub const HOST_CONFIG_ENV: &str = "ENGINE_HOST_CONFIG";
/// This site's name in the host file (the systemd instance name).
pub const SITE_ID_ENV: &str = "ENGINE_SITE_ID";

const DEFAULT_PATH: &str = "/etc/wrought/host.toml";

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
    let path = std::env::var_os(HOST_CONFIG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
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
