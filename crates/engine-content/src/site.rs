//! Opening a site that keeps content in its database.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use engine_core::builder::{self, SiteExtension};
use engine_core::config::SiteConfig;
use engine_db::{Db, DbConfig, Migration, migrate};

use crate::{ContentSite, Registry, Store, schema};

/// Whether a site uses database content: it has `types/*.toml` or pattern
/// routes.
pub fn uses_content(site: &Path, cfg: &SiteConfig) -> bool {
    let has_types = std::fs::read_dir(site.join("types"))
        .map(|mut d| {
            d.any(|e| {
                e.map(|e| e.path().extension().is_some_and(|x| x == "toml"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    has_types || cfg.routes.iter().any(|r| r.is_pattern())
}

/// The site's database settings, or an error explaining how to set them.
pub fn db_config(cfg: &SiteConfig) -> Result<DbConfig> {
    DbConfig::from_site(cfg.database.as_ref())?.with_context(|| {
        format!(
            "this site keeps content in a database: set {} or `[database] url` in site.toml",
            engine_db::config::URL_ENV
        )
    })
}

/// A site opened against its database.
pub struct ContentApp {
    pub dir: PathBuf,
    pub cfg: SiteConfig,
    pub db: Db,
    pub store: Store,
    pub content: ContentSite,
    pub rt: tokio::runtime::Handle,
}

impl ContentApp {
    /// Load the types, check that they were synced and that `migrations`
    /// are all applied, and set up the template extension. `rt` must be the
    /// runtime `db` runs on.
    pub async fn open(
        dir: &Path,
        db: Db,
        rt: tokio::runtime::Handle,
        migrations: &[Migration],
    ) -> Result<Self> {
        let (cfg, _) = builder::load_config(dir)?;
        let reg = Registry::load(dir)?;
        schema::ensure_synced(dir, &reg)?;
        let pending = migrate::pending(&db, migrations).await?;
        if !pending.is_empty() {
            bail!(
                "the database is missing {} migration(s) ({}); run `engine migrate`",
                pending.len(),
                pending.join(", ")
            );
        }
        let store = Store::new(db.clone(), Arc::new(reg));
        let content = ContentSite::new(store.clone(), rt.clone(), &cfg)?;
        Ok(ContentApp {
            dir: dir.to_path_buf(),
            cfg,
            db,
            store,
            content,
            rt,
        })
    }

    pub fn extension(&self) -> Arc<dyn SiteExtension> {
        Arc::new(self.content.clone())
    }
}
