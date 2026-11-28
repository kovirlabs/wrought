//! Template extensions of the features that are not content (`auth`'s
//! Accounts components, `forms`' Forms components and functions,
//! `realtime`'s live region), so every
//! command that renders pages (`build`, `dev`, `serve`, `rebuild`) sees the
//! same templates.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use engine_core::builder::SiteExtension;
#[cfg(any(
    feature = "auth",
    feature = "forms",
    feature = "media",
    feature = "realtime"
))]
use engine_core::builder::{self, BuildOptions, BuildReport, Extensions};
use engine_core::config::SiteConfig;

/// The extensions plus the handles `serve` needs.
#[derive(Default)]
pub struct Extras {
    pub list: Vec<Arc<dyn SiteExtension>>,
    #[cfg(feature = "forms")]
    pub forms: Option<engine_forms::Forms>,
    #[cfg(feature = "media")]
    pub media: Option<engine_media::MediaStore>,
    #[cfg(feature = "payments")]
    pub shop: Option<crate::payments::Shop>,
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
    // The site crate's named queries (App::query), prepared now.
    if let Some(db) = db
        && let Some(q) = crate::app::query_extension(db, rt)
    {
        rt.block_on(crate::app::check_queries(db))?;
        x.list.push(q);
    }
    // The live_region component (no database needed to render it).
    #[cfg(feature = "realtime")]
    x.list.push(Arc::new(engine_realtime::LiveExtension));
    #[cfg(feature = "auth")]
    {
        let ac = engine_auth::AuthConfig::from_site(cfg.auth.as_ref())?;
        x.list
            .push(Arc::new(engine_auth::site::AccountsExtension::new(&ac)));
    }
    #[cfg(feature = "media")]
    if let Some(db) = db {
        let store = crate::media::store(site, db)?;
        x.list.push(Arc::new(engine_media::MediaExtension::new(
            store.clone(),
            rt.clone(),
        )));
        x.media = Some(store);
    }
    #[cfg(feature = "forms")]
    {
        #[allow(unused_mut)]
        let mut forms = rt.block_on(engine_forms::Forms::open(site, db.cloned(), rt.clone()))?;
        #[cfg(feature = "media")]
        if let Some(m) = &x.media {
            forms = forms.with_media(m.clone());
        }
        x.list.push(forms.extension());
        x.forms = Some(forms);
    }
    #[cfg(feature = "payments")]
    if let Some(shop) = crate::payments::shop(site, cfg, db)? {
        let db = db.expect("checked by payments::shop");
        x.list
            .push(Arc::new(engine_payments::site::ShopExtension::new(
                engine_payments::OrderStore::new(db.clone()),
                shop.catalogue.clone(),
                shop.config.clone(),
                rt.clone(),
            )));
        x.shop = Some(shop);
    }
    Ok(x)
}

/// The search index of a content site (`search` feature).
#[cfg(feature = "search")]
pub fn search_index(
    cfg: &SiteConfig,
    db: &engine_db::Db,
    content: &engine_content::ContentSite,
) -> Result<engine_search::SearchIndex> {
    let sc = engine_search::SearchConfig::from_site(cfg.search.as_ref())?;
    Ok(engine_search::SearchIndex::new(
        db.clone(),
        content.clone(),
        sc,
    ))
}

/// Extensions that need the content site itself (`search`).
#[cfg(feature = "content")]
#[allow(unused_variables, unused_mut)]
pub fn content_extras(
    cfg: &SiteConfig,
    db: &engine_db::Db,
    rt: &tokio::runtime::Handle,
    content: &engine_content::ContentSite,
) -> Result<Vec<Arc<dyn SiteExtension>>> {
    #[cfg(feature = "search")]
    let v: Vec<Arc<dyn SiteExtension>> = vec![Arc::new(engine_search::SearchExtension::new(
        search_index(cfg, db, content)?,
        rt.clone(),
    ))];
    #[cfg(not(feature = "search"))]
    let v: Vec<Arc<dyn SiteExtension>> = Vec::new();
    Ok(v)
}

#[cfg(any(
    feature = "auth",
    feature = "forms",
    feature = "media",
    feature = "realtime"
))]
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
