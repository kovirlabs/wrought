//! `[jobs]` in `site.toml`.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsConfig {
    /// Jobs one process runs at the same time.
    #[serde(default = "d_workers")]
    pub workers: usize,
    /// How often idle workers look for due jobs (NOTIFY wakes them sooner).
    #[serde(default = "d_poll")]
    pub poll_seconds: f64,
    /// How long a claimed job may go without a heartbeat before another
    /// worker treats its worker as dead and re-queues it.
    #[serde(default = "d_lease")]
    pub lease_seconds: f64,
    /// On shutdown, how long running jobs may take to finish.
    #[serde(default = "d_grace")]
    pub shutdown_grace_seconds: f64,
    #[serde(default = "d_base")]
    pub retry_base_seconds: f64,
    #[serde(default = "d_max")]
    pub retry_max_seconds: f64,
    /// How long a single job may run.
    #[serde(default = "d_timeout")]
    pub timeout_seconds: f64,
    #[serde(default)]
    pub schedule: Vec<ScheduleConfig>,
}

fn d_workers() -> usize {
    4
}
fn d_poll() -> f64 {
    2.0
}
fn d_lease() -> f64 {
    60.0
}
fn d_grace() -> f64 {
    30.0
}
fn d_base() -> f64 {
    10.0
}
fn d_max() -> f64 {
    3600.0
}
fn d_timeout() -> f64 {
    600.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    pub name: String,
    pub job: String,
    pub every: String,
    #[serde(default)]
    pub payload: Option<toml::Table>,
}

/// A recurring job.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub name: String,
    pub kind: String,
    pub every: Duration,
    pub payload: serde_json::Value,
}

impl Default for JobsConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl JobsConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let cfg: JobsConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[jobs]: {e}"))?,
            None => JobsConfig::default(),
        };
        if cfg.workers == 0 || cfg.workers > 256 {
            bail!("[jobs] workers must be between 1 and 256");
        }
        for (name, v) in [
            ("poll_seconds", cfg.poll_seconds),
            ("lease_seconds", cfg.lease_seconds),
            ("retry_base_seconds", cfg.retry_base_seconds),
            ("retry_max_seconds", cfg.retry_max_seconds),
            ("timeout_seconds", cfg.timeout_seconds),
        ] {
            if !(v.is_finite() && v > 0.0) {
                bail!("[jobs] {name} must be a positive number");
            }
        }
        if !(cfg.shutdown_grace_seconds.is_finite() && cfg.shutdown_grace_seconds >= 0.0) {
            bail!("[jobs] shutdown_grace_seconds must not be negative");
        }
        if cfg.lease_seconds < 2.0 * cfg.poll_seconds.min(5.0) {
            bail!("[jobs] lease_seconds must be well above poll_seconds");
        }
        cfg.schedules()?;
        Ok(cfg)
    }

    /// The `[[jobs.schedule]]` entries, checked.
    pub fn schedules(&self) -> Result<Vec<Schedule>> {
        let mut out: Vec<Schedule> = Vec::new();
        for s in &self.schedule {
            if !crate::valid_kind(&s.name) {
                bail!(
                    "[[jobs.schedule]] name `{}` must be lower-case letters, digits, `.`, `_`, `-`",
                    s.name
                );
            }
            if !crate::valid_kind(&s.job) {
                bail!("[[jobs.schedule]] `{}`: invalid job `{}`", s.name, s.job);
            }
            if out.iter().any(|o| o.name == s.name) {
                bail!("[[jobs.schedule]] `{}` is listed twice", s.name);
            }
            let every = parse_duration(&s.every)
                .map_err(|e| anyhow::anyhow!("[[jobs.schedule]] `{}`: every: {e}", s.name))?;
            let payload = match &s.payload {
                Some(t) => serde_json::to_value(t)?,
                None => serde_json::json!({}),
            };
            out.push(Schedule {
                name: s.name.clone(),
                kind: s.job.clone(),
                every,
                payload,
            });
        }
        Ok(out)
    }

    pub fn poll(&self) -> Duration {
        Duration::from_secs_f64(self.poll_seconds)
    }
    pub fn lease(&self) -> Duration {
        Duration::from_secs_f64(self.lease_seconds)
    }
    pub fn grace(&self) -> Duration {
        Duration::from_secs_f64(self.shutdown_grace_seconds)
    }
}

/// `90s`, `15m`, `1h`, `7d` (a whole number and a unit).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("`{s}` is not like `90s`, `15m`, `1h` or `7d`"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("`{s}` is not like `90s`, `15m`, `1h` or `7d`"),
    };
    if secs == 0 || secs > 366 * 86_400 {
        bail!("`{s}` must be between 1 second and a year");
    }
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap().as_secs(), 90);
        assert_eq!(parse_duration("15m").unwrap().as_secs(), 900);
        assert_eq!(parse_duration("2h").unwrap().as_secs(), 7200);
        assert_eq!(parse_duration("1d").unwrap().as_secs(), 86_400);
        for bad in ["", "h", "0s", "5", "5 h", "1w", "-1h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config() {
        let c = JobsConfig::default();
        assert_eq!(c.workers, 4);
        let t: toml::Table = toml::from_str(
            "workers = 2\n[[schedule]]\nname = 'a'\njob = 'x.y'\nevery = '1h'\npayload = { k = 1 }\n",
        )
        .unwrap();
        let c = JobsConfig::from_site(Some(&t)).unwrap();
        let s = c.schedules().unwrap();
        assert_eq!(s[0].every.as_secs(), 3600);
        assert_eq!(s[0].payload["k"], 1);
        for bad in [
            "workers = 0",
            "wrkers = 1",
            "[[schedule]]\nname = 'A'\njob = 'x'\nevery = '1h'",
            "[[schedule]]\nname = 'a'\njob = 'x'\nevery = 'often'",
        ] {
            let t: toml::Table = toml::from_str(bad).unwrap();
            assert!(JobsConfig::from_site(Some(&t)).is_err(), "{bad}");
        }
    }
}
