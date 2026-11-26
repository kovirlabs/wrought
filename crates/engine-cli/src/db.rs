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

impl Conn {
    /// Fail unless every migration this binary knows is applied.
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
