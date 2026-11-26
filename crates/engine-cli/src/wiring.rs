//! Template extensions of the features that are not content (`auth`'s
//! Accounts components, `forms`' Forms components and functions), so every
//! command that renders pages (`build`, `dev`, `serve`, `rebuild`) sees the
//! same templates.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use engine_core::builder::SiteExtension;
#[cfg(any(feature = "auth", feature = "forms"))]
use engine_core::builder::{self, BuildOptions, BuildReport, Extensions};
use engine_core::config::SiteConfig;

/// The extensions plus the handles `serve` needs.
#[derive(Default)]
pub struct Extras {
    pub list: Vec<Arc<dyn SiteExtension>>,
    #[cfg(feature = "forms")]
    pub forms: Option<engine_forms::Forms>,
}

/// Build the extensions for a site. `db` is the site's database, if it has
/// one; `rt` the runtime it runs on.
#[allow(unused_variables, unused_mut)]
pub fn extras(
    site: &Path,
    cfg: &SiteConfig,
    db: Option<&engine_db::Db>,
    rt: &tokio::runtime::Handle,
) -> Result<Extras> {
    let mut x = Extras::default();
    #[cfg(feature = "auth")]
    {
        let ac = engine_auth::AuthConfig::from_site(cfg.auth.as_ref())?;
        x.list
            .push(Arc::new(engine_auth::site::AccountsExtension::new(&ac)));
    }
    #[cfg(feature = "forms")]
    {
        let forms = rt.block_on(engine_forms::Forms::open(site, db.cloned(), rt.clone()))?;
        x.list.push(forms.extension());
        x.forms = Some(forms);
    }
    Ok(x)
}

#[cfg(any(feature = "auth", feature = "forms"))]
/// `engine build` of a site without database content, with the
/// extensions of this build (connecting to the database when the site has
/// one). `Ok(None)`: nothing to add, use the plain build.
pub fn plain_build(site: &Path, opts: &BuildOptions) -> Result<Option<BuildReport>> {
    let (cfg, _) = builder::load_config(site)?;
    let (rt, db) = match crate::db::db_config(site)? {
        Some(_) => {
            let c = crate::db::connect(site)?;
            (c.rt, Some(c.db))
        }
        None => (crate::runtime()?, None),
    };
    let x = extras(site, &cfg, db.as_ref(), rt.handle())?;
    let ext = Extensions(x.list).into_option();
    // Renders block on the runtime for database queries: run off it.
    let site = site.to_path_buf();
    let opts = opts.clone();
    let report = std::thread::spawn(move || builder::build_site_with(&site, &opts, ext))
        .join()
        .map_err(|_| anyhow::anyhow!("the build panicked"))??;
    drop(rt);
    Ok(Some(report))
}
