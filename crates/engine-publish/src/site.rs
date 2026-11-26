//! Opening a database-backed site with publishing: the steps `engine build`,
//! `engine serve` and `engine rebuild` share, also used by tests.

use std::path::Path;

use anyhow::{Result, bail};
use engine_content::ContentApp;
use engine_core::builder::{self, BuildOptions, BuildReport, Renderer};
use engine_db::{Db, Migration, migrate};

use crate::{DiskTarget, PublishConfig, Publisher, TagIndex, TargetKind, index_build};

pub use engine_content::site::{db_config, uses_content};

/// Every migration a publishing site needs, in order: the engine's
/// (content, publish), then the site's own.
pub fn all_migrations(site: &Path, backend: engine_db::Backend) -> Result<Vec<Migration>> {
    let mut all = engine_content::migrations(backend);
    all.extend(crate::migrations(backend));
    all.extend(migrate::site_migrations(site, backend)?);
    Ok(all)
}

/// A content site with publishing. Derefs to [`ContentApp`].
pub struct CmsSite {
    app: ContentApp,
}

impl std::ops::Deref for CmsSite {
    type Target = ContentApp;
    fn deref(&self) -> &ContentApp {
        &self.app
    }
}

impl CmsSite {
    /// Open the site; errors if types are unsynced or migrations pending.
    pub async fn open(dir: &Path, db: Db, rt: tokio::runtime::Handle) -> Result<Self> {
        let migrations = all_migrations(dir, db.backend())?;
        Ok(CmsSite {
            app: ContentApp::open(dir, db, rt, &migrations).await?,
        })
    }

    pub fn index(&self) -> TagIndex {
        TagIndex::new(self.db.clone())
    }

    /// `engine build` for a database site: build every route, then record
    /// the publish routes in the tag index. Blocks; call off the runtime.
    pub fn build(&self, opts: &BuildOptions) -> Result<BuildReport> {
        let index = self.index();
        // Changes made while the build runs are after this cursor, so the
        // publisher picks them up again later.
        let cursor = self.rt.block_on(index.latest_change())?;
        let report = builder::build_site_with(&self.dir, opts, Some(self.extension()))?;
        self.rt.block_on(index_build(&index, &report, cursor))?;
        Ok(report)
    }

    /// A publisher writing to the site's output folder. Blocks; call off
    /// the runtime.
    pub fn publisher(&self) -> Result<Publisher> {
        let pc = PublishConfig::from_site(self.cfg.publish.as_ref())?;
        let renderer = Renderer::load(&self.dir, false, Some(self.extension()))?;
        let output = builder::output_dir(&self.dir, &self.cfg, &BuildOptions::default());
        if !output.join(builder::MANIFEST).is_file() {
            bail!(
                "{} has not been built yet; run `engine build` first",
                output.display()
            );
        }
        let target = match pc.target {
            TargetKind::Disk => Box::new(DiskTarget::new(output)),
            TargetKind::S3 => unreachable!("refused by PublishConfig"),
        };
        Ok(Publisher::new(
            renderer,
            self.content.clone(),
            self.index(),
            target,
            self.rt.clone(),
        ))
    }
}
