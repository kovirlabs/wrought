//! The database connection and migration list shared by the commands of
//! database-backed features (`content`, `auth`, `forms`, `jobs`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use engine_core::builder;
use engine_db::{Db, DbConfig, Migration, migrate};

/// Every migration this binary knows for a site, in order: the engine's
/// (per compiled feature), then the site's own.
pub fn migrations_for(site: &Path, backend: engine_db::Backend) -> Result<Vec<Migration>> {
    #[allow(unused_mut)]
    let mut all: Vec<Migration> = Vec::new();
    #[cfg(feature = "content")]
    all.extend(engine_content::migrations(backend));
    #[cfg(feature = "publish")]
    all.extend(engine_publish::migrations(backend));
    #[cfg(feature = "auth")]
    all.extend(engine_auth::migrations(backend));
    #[cfg(feature = "workflow")]
    all.extend(engine_admin::workflow_migrations(backend));
    #[cfg(feature = "forms")]
    all.extend(engine_forms::migrations(backend));
    #[cfg(feature = "jobs")]
    all.extend(engine_jobs::migrations(backend));
    #[cfg(feature = "media")]
    all.extend(engine_media::migrations(backend));
    #[cfg(all(feature = "media", feature = "content"))]
    all.extend(engine_media::change_log_migrations(backend));
    #[cfg(feature = "search")]
    all.extend(engine_search::migrations(backend));
    #[cfg(feature = "api")]
    all.extend(engine_api::migrations(backend));
    #[cfg(feature = "payments")]
    all.extend(engine_payments::migrations(backend));
    all.extend(migrate::site_migrations(site, backend)?);
    Ok(all)
}

/// A runtime plus a database connection for a site.
pub struct Conn {
    pub rt: tokio::runtime::Runtime,
    pub db: Db,
    pub site: PathBuf,
}

/// The site's database settings, if it names a database.
pub fn db_config(site: &Path) -> Result<Option<DbConfig>> {
    let (cfg, _) = builder::load_config(site)?;
    DbConfig::from_site(cfg.database.as_ref(), site)
}

pub fn connect(site: &Path) -> Result<Conn> {
    let (cfg, warnings) = builder::load_config(site)?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let dbc = DbConfig::from_site(cfg.database.as_ref(), site)?.with_context(|| {
        format!(
            "this command needs the site's database: set {} or `[database] url` in site.toml",
            engine_db::config::URL_ENV
        )
    })?;
    let rt = crate::runtime()?;
    let db = rt.block_on(Db::connect(&dbc))?;
    Ok(Conn {
        rt,
        db,
        site: site.to_path_buf(),
    })
}

/// A sleeping database at the start (DECISIONS.md, "Sleeping Postgres
/// watchdog"): the host's wake command and where the sleep is recorded.
pub struct AsleepStart<'a> {
    pub wake: &'a engine_db::sleep::Hook,
    pub wake_timeout: std::time::Duration,
    pub state_file: &'a Path,
    /// The engine's idle exit: while the database sleeps and nothing asks
    /// for it, the engine may exit without ever waking it.
    pub idle_exit: Option<std::time::Duration>,
}

/// Connect to the site's database and check its migrations, waiting as long
/// as it takes while the server is unreachable (asleep, starting, down):
/// listen first, the engine serves published pages meanwhile
/// (`Starting::early`). `None` if a shutdown came first. A database that
/// answers and refuses (password, no such database) or misses migrations
/// is an error, as before.
///
/// With a sleeping database (`asleep`): if the state file says it sleeps
/// and no job is due, it is left asleep until a request needs it (or the
/// next job is due); an unreachable database is woken with the host's
/// command, at most once per `WAKE_RETRY` and only when asked for.
pub fn connect_when_reachable(
    site: &Path,
    rt: &tokio::runtime::Runtime,
    starting: &engine_core::serve::Starting,
    asleep: Option<AsleepStart<'_>>,
) -> Result<Option<Db>> {
    let (cfg, warnings) = builder::load_config(site)?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let dbc = DbConfig::from_site(cfg.database.as_ref(), site)?.with_context(|| {
        format!(
            "this command needs the site's database: set {} or `[database] url` in site.toml",
            engine_db::config::URL_ENV
        )
    })?;
    rt.block_on(async {
        use engine_db::sleep::{WAKE_RETRY, read_state, unix_now};
        if let Some(a) = &asleep
            && let Some(st) = read_state(a.state_file).filter(|s| s.asleep)
        {
            let now = unix_now();
            if st.next_due.is_none_or(|d| d > now) {
                starting.set_asleep();
                let until_due = st
                    .next_due
                    .map(|d| std::time::Duration::from_secs((d - now) as u64));
                println!(
                    "The database is asleep (for {} s): serving published pages; a request that \
                     needs it wakes it{}",
                    now - st.since,
                    until_due
                        .map(|d| format!(", as does the next job in {} s", d.as_secs()))
                        .unwrap_or_default()
                );
                tokio::select! {
                    _ = starting.demanded() => {}
                    _ = tokio::time::sleep(until_due.unwrap_or(std::time::Duration::MAX)),
                        if until_due.is_some() => {
                        println!("A scheduled job is due: waking the database");
                    }
                    _ = engine_core::host::idle(
                        a.idle_exit.unwrap_or(std::time::Duration::MAX), None),
                        if a.idle_exit.is_some() => {
                        println!(
                            "Idle with the database asleep: exiting (socket activation starts \
                             the engine again on the next request)"
                        );
                        return Ok(None);
                    }
                    _ = starting.stopped() => return Ok(None),
                }
            }
        }
        let mut pause = std::time::Duration::from_millis(250);
        let mut waited = false;
        let mut woken_at: Option<std::time::Instant> = None;
        loop {
            let attempt = async {
                if !engine_db::reachable(&dbc).await? {
                    return Ok(None);
                }
                let db = match Db::connect(&dbc).await {
                    Ok(db) => db,
                    Err(e) if engine_db::health::is_unavailable_any(&e) => return Ok(None),
                    Err(e) => return Err(e),
                };
                let all = migrations_for(site, db.backend())?;
                let pending = match migrate::pending(&db, &all).await {
                    Ok(p) => p,
                    Err(e) if engine_db::health::is_unavailable_any(&e) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if !pending.is_empty() {
                    anyhow::bail!(
                        "the database is missing {} migration(s) ({}); run `engine migrate`",
                        pending.len(),
                        pending.join(", ")
                    );
                }
                Ok(Some(db))
            };
            let got = tokio::select! {
                r = attempt => r?,
                _ = starting.stopped() => return Ok(None),
            };
            if let Some(db) = got {
                // Awake: the recorded sleep is over.
                if let Some(a) = &asleep {
                    let _ = std::fs::remove_file(a.state_file);
                }
                starting.set_waiting_for_database(false);
                if waited {
                    println!("The database answers: starting.");
                }
                return Ok(Some(db));
            }
            // A sleeping database: wake it (once per WAKE_RETRY, when asked).
            if let Some(a) = &asleep
                && woken_at.is_none_or(|t| t.elapsed() >= WAKE_RETRY)
            {
                woken_at = Some(std::time::Instant::now());
                let t0 = std::time::Instant::now();
                println!("Waking the database");
                let woke = tokio::select! {
                    r = tokio::time::timeout(a.wake_timeout, async {
                        (a.wake)().await?;
                        while !engine_db::reachable(&dbc).await? {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        anyhow::Ok(())
                    }) => r,
                    _ = starting.stopped() => return Ok(None),
                };
                let failed = match woke {
                    Ok(Ok(())) => {
                        println!(
                            "The database is awake after {} ms",
                            t0.elapsed().as_millis()
                        );
                        continue;
                    }
                    Ok(Err(e)) => format!("wake failed: {e:#}"),
                    Err(_) => format!(
                        "wake failed: not reachable {} s after the wake command",
                        a.wake_timeout.as_secs()
                    ),
                };
                eprintln!(
                    "The database: {failed}; serving published pages and spooling form posts, \
                     trying again when a request needs it"
                );
                engine_db::sleep::note_wake_error(a.state_file, &failed);
            }
            if !waited {
                eprintln!(
                    "The database ({}) is not reachable: serving published pages, trying again",
                    dbc.redacted_url()
                );
                waited = true;
            }
            starting.set_waiting_for_database(true);
            tokio::select! {
                _ = tokio::time::sleep(pause) => {}
                // Asked for again: try (and wake) at once.
                _ = starting.demanded(), if asleep.is_some() => {}
                _ = starting.stopped() => return Ok(None),
            }
            pause = (pause * 2).min(std::time::Duration::from_secs(2));
        }
    })
}

impl Conn {
    /// Fail unless every migration this binary knows is applied.
    // Used by the commands of some features only (`engine serve` checks
    // in `connect_when_reachable`).
    #[allow(dead_code)]
    pub fn ensure_migrated(&self) -> Result<()> {
        let all = migrations_for(&self.site, self.db.backend())?;
        let pending = self.rt.block_on(migrate::pending(&self.db, &all))?;
        if !pending.is_empty() {
            anyhow::bail!(
                "the database is missing {} migration(s) ({}); run `engine migrate`",
                pending.len(),
                pending.join(", ")
            );
        }
        Ok(())
    }
}

/// `engine migrate`.
pub fn migrate_cmd(site: &Path, status_only: bool) -> Result<()> {
    let c = connect(site)?;
    #[cfg(feature = "content")]
    {
        let reg = engine_content::Registry::load(site)?;
        if let Err(e) = engine_content::schema::ensure_synced(site, &reg) {
            eprintln!("warning: {e:#}");
        }
    }
    let all = migrations_for(site, c.db.backend())?;
    if status_only {
        for s in c.rt.block_on(migrate::status(&c.db, &all))? {
            println!(
                "{}  {}",
                if s.applied { "applied" } else { "pending" },
                s.id
            );
        }
        return Ok(());
    }
    let ran = c.rt.block_on(migrate::apply(&c.db, &all))?;
    // Secrets the engine keeps in the database are made here, once, so a
    // build or a server start never has to write (and wait for a sync).
    #[cfg(feature = "forms")]
    c.rt.block_on(engine_forms::store::FormStore::new(c.db.clone()).engine_key("forms.stamp"))?;
    if ran.is_empty() {
        println!("Database is up to date ({} migrations).", all.len());
    } else {
        for id in &ran {
            println!("applied {id}");
        }
        println!("Applied {} migration(s).", ran.len());
    }
    Ok(())
}
