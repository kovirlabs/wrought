//! Limits scaled to the machine: worker threads, the database pool, caches
//! and background work, each sized from what this process may use (CPUs
//! and memory, honouring container limits: [`crate::resources`]). There is
//! no "small mode": a 1-CPU, 512 MiB container and a 64-CPU server run the
//! same formulas, and every value can be set in the site's configuration
//! (`[limits]`, or the section that has always held it: `[database]
//! max_connections`, `[jobs] workers`, `[cache] max_mb`). `engine doctor`
//! prints each value, its formula and whether it was set by hand.
//!
//! The formulas never give a large machine less than it can use, and never
//! give a small one more than it can hold: caches are a fraction of memory
//! between a floor and a ceiling; thread and connection counts follow the
//! CPUs, capped where more stop helping.

use std::sync::RwLock;

use serde::Deserialize;

use crate::resources::Resources;

const MIB: u64 = 1 << 20;

/// `[limits]` in site.toml: each field overrides one computed value.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    /// Async worker threads (they serve requests and run async tasks).
    pub worker_threads: Option<usize>,
    /// Most threads for blocking work at once (file reads, renders).
    pub blocking_threads: Option<usize>,
    /// Image variants made at once (heavy background work; a bulk
    /// publish compresses on one low-priority thread per CPU).
    pub background_threads: Option<usize>,
    /// Cache of items' rendered template values, in MiB (0: off).
    pub value_cache_mb: Option<u64>,
    /// Entries in the static server's cache of file ETags.
    pub etag_cache_entries: Option<usize>,
}

/// Overrides that live in their own sections, collected by the engine
/// (`None`: not set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionOverrides {
    /// `[database] max_connections` or `ENGINE_DATABASE_MAX_CONNECTIONS`.
    pub db_connections: Option<u32>,
    /// `[jobs] workers`.
    pub job_workers: Option<usize>,
    /// `[cache] max_mb`.
    pub fragment_cache_mb: Option<u64>,
}

/// The environment variable that overrides `[database] max_connections`.
pub const DB_MAX_ENV: &str = "ENGINE_DATABASE_MAX_CONNECTIONS";

impl SectionOverrides {
    /// Read the overrides from their sections of `cfg` (and the database
    /// one from the environment, which wins).
    pub fn from_site(cfg: &crate::config::SiteConfig) -> Result<Self, crate::config::ConfigError> {
        use crate::config::ConfigError;
        let int = |t: Option<&toml::Table>, key: &str| -> Result<Option<u64>, ConfigError> {
            match t.and_then(|t| t.get(key)) {
                None => Ok(None),
                Some(v) => v
                    .as_integer()
                    .and_then(|i| u64::try_from(i).ok())
                    .map(Some)
                    .ok_or_else(|| ConfigError::Invalid(format!("{key} must be a whole number"))),
            }
        };
        let db = match std::env::var(DB_MAX_ENV) {
            Ok(v) if !v.trim().is_empty() => Some(v.trim().parse::<u64>().map_err(|_| {
                ConfigError::Invalid(format!("{DB_MAX_ENV} must be a whole number above 0"))
            })?),
            _ => int(cfg.database.as_ref(), "max_connections")?,
        };
        Ok(SectionOverrides {
            db_connections: db.map(|n| n.min(u32::MAX as u64) as u32),
            job_workers: int(cfg.jobs.as_ref(), "workers")?.map(|n| n as usize),
            fragment_cache_mb: cfg.cache.max_mb,
        })
    }
}

/// Where a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Computed from the machine.
    Auto,
    /// Set in the configuration (named).
    Configured(&'static str),
}

/// One limit, for `engine doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    pub name: &'static str,
    /// The value, formatted with its unit.
    pub value: String,
    /// How it is computed when not set.
    pub formula: &'static str,
    pub source: Source,
}

/// Every limit, sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub worker_threads: usize,
    pub blocking_threads: usize,
    pub db_connections: u32,
    pub job_workers: usize,
    pub background_threads: usize,
    pub value_cache_bytes: u64,
    pub fragment_cache_bytes: u64,
    pub etag_cache_entries: usize,
}

/// Async workers: one per CPU (a request never waits for a free worker while
/// a CPU is idle; more would only switch between them).
pub fn worker_threads(r: Resources) -> usize {
    r.cpus.max(1)
}

/// Blocking threads (file reads through `tokio::fs`, renders): sixteen per
/// CPU, at least 32 so slow disks or database waits overlap, at most 512
/// (Tokio's own ceiling), and no more than one per 4 MiB of memory (each
/// can hold a stack and a render's working set).
pub fn blocking_threads(r: Resources) -> usize {
    let by_mem = (r.memory / (4 * MIB)).max(8) as usize;
    (r.cpus * 16).clamp(32, 512).min(by_mem)
}

/// Database connections (and so request renders at once): CPUs plus four,
/// so renders waiting on a query overlap with ones using a CPU; 4 to 32
/// (past that, a PostgreSQL server spends more on connections than it
/// gains), and at most one per 64 MiB of memory, at least 2.
pub fn db_connections(r: Resources) -> u32 {
    let by_mem = (r.memory / (64 * MIB)).max(2);
    ((r.cpus as u64 + 4).clamp(4, 32)).min(by_mem) as u32
}

/// Job workers: one per CPU, 2 to 16 (jobs mostly wait on the network or
/// the database; heavy ones also take a background slot).
pub fn job_workers(r: Resources) -> usize {
    r.cpus.clamp(2, 16)
}

/// Heavy background work at once: one per CPU, and one per 256 MiB of
/// memory (a decoded photograph can take 100 MiB), at least one. These
/// threads run at a lower scheduling priority, so serving always comes
/// first and an idle machine still gives them every CPU.
pub fn background_threads(r: Resources) -> usize {
    let by_mem = (r.memory / (256 * MIB)) as usize;
    r.cpus.min(by_mem).max(1)
}

/// Items' rendered values: 1/256 of memory, 1 to 64 MiB.
pub fn value_cache_bytes(r: Resources) -> u64 {
    (r.memory / 256).clamp(MIB, 64 * MIB)
}

/// Fragment cache of `request` renders: 1/64 of memory, 4 to 256 MiB.
pub fn fragment_cache_bytes(r: Resources) -> u64 {
    (r.memory / 64).clamp(4 * MIB, 256 * MIB)
}

/// File ETags kept by the static server (about 200 bytes each): 1/1024 of
/// memory, at least 4096.
pub fn etag_cache_entries(r: Resources) -> usize {
    ((r.memory / 1024 / 200) as usize).max(4096)
}

impl Limits {
    /// Size every limit for `r`, then apply the overrides.
    pub fn size(r: Resources, cfg: &LimitsConfig, sections: &SectionOverrides) -> Limits {
        Limits {
            worker_threads: cfg
                .worker_threads
                .unwrap_or_else(|| worker_threads(r))
                .max(1),
            blocking_threads: cfg
                .blocking_threads
                .unwrap_or_else(|| blocking_threads(r))
                .max(1),
            db_connections: sections
                .db_connections
                .unwrap_or_else(|| db_connections(r))
                .max(1),
            job_workers: sections
                .job_workers
                .unwrap_or_else(|| job_workers(r))
                .max(1),
            background_threads: cfg
                .background_threads
                .unwrap_or_else(|| background_threads(r))
                .max(1),
            value_cache_bytes: cfg
                .value_cache_mb
                .map_or_else(|| value_cache_bytes(r), |m| m * MIB),
            fragment_cache_bytes: sections
                .fragment_cache_mb
                .map_or_else(|| fragment_cache_bytes(r), |m| m.max(1) * MIB),
            etag_cache_entries: cfg
                .etag_cache_entries
                .unwrap_or_else(|| etag_cache_entries(r))
                .max(16),
        }
    }

    /// Each value with its formula and source, in a fixed order.
    pub fn explain(&self, cfg: &LimitsConfig, sections: &SectionOverrides) -> Vec<Explained> {
        fn src(set: bool, key: &'static str) -> Source {
            if set {
                Source::Configured(key)
            } else {
                Source::Auto
            }
        }
        let mib = |b: u64| format!("{} MiB", b / MIB);
        vec![
            Explained {
                name: "worker threads",
                value: self.worker_threads.to_string(),
                formula: "one per CPU",
                source: src(cfg.worker_threads.is_some(), "[limits] worker_threads"),
            },
            Explained {
                name: "blocking threads",
                value: self.blocking_threads.to_string(),
                formula: "16 per CPU, 32 to 512, at most one per 4 MiB of memory",
                source: src(cfg.blocking_threads.is_some(), "[limits] blocking_threads"),
            },
            Explained {
                name: "database connections (= renders at once)",
                value: self.db_connections.to_string(),
                formula: "CPUs + 4, 4 to 32, at most one per 64 MiB of memory",
                source: src(
                    sections.db_connections.is_some(),
                    "[database] max_connections or ENGINE_DATABASE_MAX_CONNECTIONS",
                ),
            },
            Explained {
                name: "job workers",
                value: self.job_workers.to_string(),
                formula: "one per CPU, 2 to 16",
                source: src(sections.job_workers.is_some(), "[jobs] workers"),
            },
            Explained {
                name: "background threads (image variants; low priority)",
                value: self.background_threads.to_string(),
                formula: "one per CPU and per 256 MiB of memory, at least 1",
                source: src(
                    cfg.background_threads.is_some(),
                    "[limits] background_threads",
                ),
            },
            Explained {
                name: "rendered value cache",
                value: mib(self.value_cache_bytes),
                formula: "1/256 of memory, 1 to 64 MiB",
                source: src(cfg.value_cache_mb.is_some(), "[limits] value_cache_mb"),
            },
            Explained {
                name: "fragment cache",
                value: mib(self.fragment_cache_bytes),
                formula: "1/64 of memory, 4 to 256 MiB",
                source: src(sections.fragment_cache_mb.is_some(), "[cache] max_mb"),
            },
            Explained {
                name: "file ETag cache",
                value: format!("{} entries", self.etag_cache_entries),
                formula: "1/1024 of memory at ~200 bytes an entry, at least 4096",
                source: src(
                    cfg.etag_cache_entries.is_some(),
                    "[limits] etag_cache_entries",
                ),
            },
        ]
    }
}

static CURRENT: RwLock<Option<(LimitsConfig, SectionOverrides)>> = RwLock::new(None);

/// Record the site's overrides (called when its configuration is loaded),
/// for the parts of the engine that size themselves.
pub fn configure(cfg: &LimitsConfig, sections: SectionOverrides) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some((cfg.clone(), sections));
}

/// The overrides recorded by [`configure`] (none before it).
pub fn overrides() -> (LimitsConfig, SectionOverrides) {
    CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// The limits for this process: its resources and the recorded overrides.
pub fn current() -> Limits {
    let (cfg, sections) = overrides();
    Limits::size(crate::resources::get(), &cfg, &sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    const SMALL: Resources = Resources {
        cpus: 1,
        memory: 512 * MIB,
    };
    const PI: Resources = Resources {
        cpus: 4,
        memory: 8 * GIB,
    };
    const LARGE: Resources = Resources {
        cpus: 64,
        memory: 256 * GIB,
    };

    fn auto(r: Resources) -> Limits {
        Limits::size(r, &LimitsConfig::default(), &SectionOverrides::default())
    }

    #[test]
    fn small_container() {
        let l = auto(SMALL);
        assert_eq!(l.worker_threads, 1);
        assert_eq!(l.blocking_threads, 32);
        assert_eq!(l.db_connections, 5);
        assert_eq!(l.job_workers, 2);
        assert_eq!(l.background_threads, 1);
        assert_eq!(l.value_cache_bytes, 2 * MIB);
        assert_eq!(l.fragment_cache_bytes, 8 * MIB);
        assert_eq!(l.etag_cache_entries, 4096);
    }

    #[test]
    fn pi_class() {
        let l = auto(PI);
        assert_eq!(l.worker_threads, 4);
        assert_eq!(l.blocking_threads, 64);
        assert_eq!(l.db_connections, 8);
        assert_eq!(l.job_workers, 4);
        assert_eq!(l.background_threads, 4);
        assert_eq!(l.value_cache_bytes, 32 * MIB);
        assert_eq!(l.fragment_cache_bytes, 128 * MIB);
        assert_eq!(l.etag_cache_entries, 41_943);
    }

    #[test]
    fn large_server() {
        let l = auto(LARGE);
        assert_eq!(l.worker_threads, 64);
        assert_eq!(l.blocking_threads, 512);
        assert_eq!(l.db_connections, 32);
        assert_eq!(l.job_workers, 16);
        assert_eq!(l.background_threads, 64);
        assert_eq!(l.value_cache_bytes, 64 * MIB);
        assert_eq!(l.fragment_cache_bytes, 256 * MIB);
        assert_eq!(l.etag_cache_entries, 1_342_177);
    }

    #[test]
    fn tiny_memory_caps_counts() {
        // 4 CPUs but 128 MiB: memory, not CPUs, bounds what may run.
        let l = auto(Resources {
            cpus: 4,
            memory: 128 * MIB,
        });
        assert_eq!(l.db_connections, 2);
        assert_eq!(l.blocking_threads, 32);
        assert_eq!(l.background_threads, 1);
        assert_eq!(l.value_cache_bytes, MIB);
        assert_eq!(l.fragment_cache_bytes, 4 * MIB);
    }

    #[test]
    fn overrides_win_and_are_reported() {
        let cfg = LimitsConfig {
            worker_threads: Some(2),
            value_cache_mb: Some(0),
            ..Default::default()
        };
        let sec = SectionOverrides {
            db_connections: Some(20),
            job_workers: None,
            fragment_cache_mb: Some(16),
        };
        let l = Limits::size(LARGE, &cfg, &sec);
        assert_eq!(l.worker_threads, 2);
        assert_eq!(l.value_cache_bytes, 0);
        assert_eq!(l.db_connections, 20);
        assert_eq!(l.fragment_cache_bytes, 16 * MIB);
        assert_eq!(l.job_workers, 16);
        let e = l.explain(&cfg, &sec);
        let by = |n: &str| e.iter().find(|x| x.name.starts_with(n)).unwrap().clone();
        assert_eq!(
            by("worker threads").source,
            Source::Configured("[limits] worker_threads")
        );
        assert_eq!(by("worker threads").value, "2");
        assert_eq!(by("job workers").source, Source::Auto);
        assert_eq!(by("fragment cache").value, "16 MiB");
        assert!(matches!(by("database").source, Source::Configured(_)));
    }

    #[test]
    fn section_overrides_are_read_from_the_site() {
        let cfg = crate::config::SiteConfig::parse(
            "[site]\nname = 'x'\n[database]\nmax_connections = 7\n[jobs]\nworkers = 3\n\
             [cache]\nmax_mb = 12\n[limits]\nblocking_threads = 40\n",
        )
        .unwrap();
        assert_eq!(cfg.limits.blocking_threads, Some(40));
        if std::env::var(DB_MAX_ENV).is_err() {
            let s = SectionOverrides::from_site(&cfg).unwrap();
            assert_eq!(
                s,
                SectionOverrides {
                    db_connections: Some(7),
                    job_workers: Some(3),
                    fragment_cache_mb: Some(12),
                }
            );
        }
        let bad =
            crate::config::SiteConfig::parse("[site]\nname = 'x'\n[limits]\nworker_threads = 0\n")
                .unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(toml::from_str::<LimitsConfig>("worker_threads = 3").is_ok());
        assert!(toml::from_str::<LimitsConfig>("small_mode = true").is_err());
    }
}
