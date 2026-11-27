//! Opening a database-backed site with publishing: the steps `engine build`,
//! `engine serve` and `engine rebuild` share, also used by tests.

use std::path::Path;

use anyhow::{Result, bail};
use engine_content::ContentApp;
use engine_core::builder::{self, BuildOptions, BuildReport, Renderer};
use engine_db::{Db, Migration, migrate};

use crate::{
    DiskTarget, PublishConfig, PublishTarget, Publisher, TagIndex, TargetKind, index_build,
};

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

    /// Wrap an already opened content site (its migrations must include
    /// the publish feature's).
    pub fn from_app(app: ContentApp) -> Self {
        CmsSite { app }
    }

    /// Render the extensions of other features (`auth`, `forms`...) along
    /// with the content's.
    pub fn with_extensions(
        mut self,
        extra: Vec<std::sync::Arc<dyn engine_core::SiteExtension>>,
    ) -> Self {
        self.app.extra = extra;
        self
    }

    pub fn index(&self) -> TagIndex {
        TagIndex::new(self.db.clone())
    }

    /// `engine build` for a database site: build every route, then record
    /// the publish routes in the tag index. Blocks; call off the runtime.
    pub fn build(&self, opts: &BuildOptions) -> Result<BuildReport> {
        use engine_core::timings::time;
        let index = self.index();
        // Changes made while the build runs are after this cursor, so the
        // publisher picks them up again later.
        let cursor = time("change cursor", || self.rt.block_on(index.latest_change()))?;
        let report = time("build", || {
            builder::build_site_with(&self.dir, opts, Some(self.extension()))
        })?;
        time("tag index", || {
            self.rt.block_on(index_build(&index, &report, cursor))
        })?;
        Ok(report)
    }

    /// The site's output folder, checked to hold a build.
    pub fn built_output(&self) -> Result<std::path::PathBuf> {
        let output = builder::output_dir(&self.dir, &self.cfg, &BuildOptions::default());
        if !output.join(builder::MANIFEST).is_file() {
            bail!(
                "{} has not been built yet; run `engine build` first",
                output.display()
            );
        }
        Ok(output)
    }

    /// A publisher writing to the site's output folder (`target = "disk"`).
    /// Blocks; call off the runtime.
    pub fn publisher(&self) -> Result<Publisher> {
        let pc = PublishConfig::from_site(self.cfg.publish.as_ref())?;
        let output = self.built_output()?;
        let target: Box<dyn PublishTarget> = match pc.target {
            TargetKind::Disk => Box::new(DiskTarget::new(output)),
            TargetKind::S3 => bail!(
                "[publish] target = \"s3\" needs the `s3` feature, which is not in this build"
            ),
        };
        self.publisher_with(target)
    }

    /// A publisher writing to `target`. Blocks; call off the runtime.
    pub fn publisher_with(&self, target: Box<dyn PublishTarget>) -> Result<Publisher> {
        let renderer = Renderer::load(&self.dir, false, Some(self.extension()))?;
        Ok(Publisher::new(
            renderer,
            self.content.clone(),
            self.index(),
            target,
            self.rt.clone(),
        ))
    }
}
