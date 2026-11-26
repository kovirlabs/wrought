//! `engine serve` for a site with a database: the static server plus, per
//! compiled feature, render-on-miss and the publish listener (`publish`),
//! `request`-mode pages with the fragment cache, `/account/` (`auth`),
//! `/admin/` (`admin`), form posts (`forms`), job workers (`jobs`),
//! email (`mail`) and the shop (`payments`). Template and Markdown edits
//! are picked up while it runs.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use engine_core::builder::{self, BuildOptions, Extensions, Renderer, SiteExtension};
use engine_core::serve::{self, ServeConfig};
use notify::{RecursiveMode, Watcher};

/// The renderer currently in use (swapped when templates change).
pub type CurrentRenderer = Arc<dyn Fn() -> Arc<Renderer> + Send + Sync>;

/// Options of `engine serve`.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// Run job workers in this process (`jobs` feature).
    pub workers: bool,
}

/// Serve a site that has database content, accounts, forms or jobs.
/// `Ok(false)` if it has none of them (the plain static server applies).
pub fn serve(site: &Path, addr: SocketAddr, opts: ServeOptions) -> Result<bool> {
    let site = site
        .canonicalize()
        .with_context(|| format!("site folder {}", site.display()))?;
    let (cfg, _) = builder::load_config(&site)?;
    #[cfg(feature = "content")]
    let has_content = engine_content::site::uses_content(&site, &cfg);
    #[cfg(not(feature = "content"))]
    let has_content = false;
    let has_db = crate::db::db_config(&site)?.is_some();
    let db_features = cfg!(any(
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media"
    ));
    if !(has_content || (has_db && db_features)) {
        return Ok(false);
    }
    let conn = crate::db::connect(&site)?;
    conn.ensure_migrated()?;
    let output = builder::output_dir(&site, &cfg, &BuildOptions::default());
    if !output.is_dir() {
        bail!(
            "{} does not exist; run `engine build` first",
            output.display()
        );
    }
    #[allow(unused_variables)]
    let public_url = std::env::var("ENGINE_PUBLIC_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            if cfg.site.base_url.is_empty() {
                format!("http://{addr}")
            } else {
                cfg.site.base_url.clone()
            }
        });
    #[allow(unused_variables)]
    let extras = crate::wiring::extras(&site, &cfg, Some(&conn.db), conn.rt.handle())?;

    // Content: the template extension, the store, the publisher.
    #[allow(unused_mut)]
    let mut ext: Option<Arc<dyn SiteExtension>> = Extensions(extras.list.clone()).into_option();
    #[cfg(feature = "admin")]
    let mut admin_parts = None;
    #[cfg(feature = "api")]
    let mut api_parts: Option<(engine_content::Store, engine_content::ContentSite)> = None;
    #[cfg(feature = "publish")]
    let mut publisher: Option<Arc<engine_publish::Publisher>> = None;
    #[cfg(feature = "search")]
    let mut search: Option<engine_search::SearchIndex> = None;
    #[cfg(feature = "content")]
    if has_content {
        let migrations = crate::db::migrations_for(&site, conn.db.backend())?;
        let mut app = conn.rt.block_on(engine_content::ContentApp::open(
            &site,
            conn.db.clone(),
            conn.rt.handle().clone(),
            &migrations,
        ))?;
        app.extra = extras.list.clone();
        app.extra.extend(crate::wiring::content_extras(
            &cfg,
            &conn.db,
            conn.rt.handle(),
            &app.content,
        )?);
        #[cfg(feature = "search")]
        {
            search = Some(crate::wiring::search_index(&cfg, &conn.db, &app.content)?);
        }
        ext = Some(app.extension());
        #[cfg(feature = "admin")]
        {
            admin_parts = Some((app.store.clone(), app.content.clone()));
        }
        #[cfg(feature = "api")]
        {
            api_parts = Some((app.store.clone(), app.content.clone()));
        }
        #[cfg(feature = "publish")]
        {
            let cms = engine_publish::CmsSite::from_app(app);
            publisher = Some(Arc::new(cms.publisher()?));
        }
    }

    let own = Arc::new(RwLock::new(Arc::new(Renderer::load(
        &site,
        false,
        ext.clone(),
    )?)));
    #[allow(unused_variables)]
    let current: CurrentRenderer = {
        #[cfg(feature = "publish")]
        if let Some(p) = &publisher {
            let p = p.clone();
            Arc::new(move || p.renderer())
        } else {
            let own = own.clone();
            Arc::new(move || own.read().unwrap_or_else(|p| p.into_inner()).clone())
        }
        #[cfg(not(feature = "publish"))]
        {
            let own = own.clone();
            Arc::new(move || own.read().unwrap_or_else(|p| p.into_inner()).clone())
        }
    };

    #[allow(unused_mut)]
    let mut config = ServeConfig {
        root: output.clone(),
        ..Default::default()
    };
    #[allow(unused_mut, unused_assignments)]
    let mut app_router: Option<axum::Router> = None;

    #[cfg(feature = "publish")]
    if let Some(p) = &publisher {
        let p = p.clone();
        config.on_miss = Some(Arc::new(move |path: String| {
            let p = p.clone();
            Box::pin(async move {
                let shown = path.clone();
                match tokio::task::spawn_blocking(move || p.render_missing(&path)).await {
                    Ok(Ok(found)) => found,
                    Ok(Err(e)) => {
                        eprintln!("render on miss {shown}: {e:#}");
                        false
                    }
                    Err(e) => {
                        eprintln!("render on miss {shown}: {e}");
                        false
                    }
                }
            })
        }));
    }

    // Jobs and mail (PostgreSQL only: on SQLite they are off, and a site
    // that asks for them gets an error).
    #[cfg(feature = "jobs")]
    let jobs_parts = if conn.db.backend() == engine_db::Backend::Postgres {
        Some(crate::jobs::parts(&site, &cfg, &conn.db, &public_url)?)
    } else {
        crate::jobs::refuse_on_sqlite(&site, &cfg)?;
        eprintln!(
            "note: jobs and mail need PostgreSQL; they are off for this SQLite database \
             (account messages go to this log)"
        );
        None
    };
    #[cfg(feature = "mail")]
    let mailer: Option<engine_mail::Mailer> = jobs_parts
        .as_ref()
        .map(|p| p.mailer.clone())
        .filter(|m| m.configured());
    #[cfg(feature = "mail")]
    if jobs_parts.is_some() && mailer.is_none() {
        eprintln!(
            "note: no SMTP server configured (set {} and [mail] from); account messages go to \
             this log, and queued mail waits",
            engine_mail::SMTP_URL_ENV
        );
    }

    // Accounts, the admin and request mode.
    #[cfg(feature = "auth")]
    let auth_cfg = engine_auth::AuthConfig::from_site(cfg.auth.as_ref())?;
    #[cfg(feature = "auth")]
    let (cookie_secure, behind_proxy) = (auth_cfg.cookie_secure, auth_cfg.behind_proxy);
    #[cfg(all(feature = "forms", not(feature = "auth")))]
    let (cookie_secure, behind_proxy) = (true, false);
    #[cfg(any(feature = "auth", feature = "forms"))]
    #[allow(unused_variables)]
    let anon = engine_core::web::AnonCsrf {
        secure: cookie_secure,
    };
    #[cfg(feature = "auth")]
    let auth = {
        #[allow(unused_mut)]
        let mut notifier: Arc<dyn engine_auth::AccountNotifier> =
            Arc::new(engine_auth::LogNotifier);
        #[cfg(feature = "mail")]
        if let Some(m) = &mailer {
            notifier = Arc::new(engine_mail::MailNotifier { mailer: m.clone() });
        }
        let auth = engine_auth::Auth::new(
            conn.db.clone(),
            auth_cfg,
            &cfg.site.name,
            &public_url,
            notifier,
        );
        let n = conn.rt.block_on(auth.check_totp_storage())?;
        if n > 0 {
            println!("Encrypted {n} two-factor secret(s) stored by an earlier version.");
        }
        auth
    };
    #[cfg(any(feature = "auth", feature = "forms"))]
    let rm = crate::request::RequestMode {
        current: current.clone(),
        csrf: anon,
        #[cfg(feature = "auth")]
        auth: Some(auth.clone()),
    };
    #[cfg(feature = "auth")]
    {
        #[cfg(feature = "admin")]
        let ui = if admin_parts.is_some() {
            engine_admin::ui(&cfg.site.name)
        } else {
            Arc::new(engine_auth::web::Ui::new(&cfg.site.name, &[], &[]))
        };
        #[cfg(not(feature = "admin"))]
        let ui = Arc::new(engine_auth::web::Ui::new(&cfg.site.name, &[], &[]));
        #[cfg(feature = "admin")]
        let home = if admin_parts.is_some() {
            "/admin/"
        } else {
            "/"
        };
        #[cfg(not(feature = "admin"))]
        let home = "/";
        let web = engine_auth::web::Web {
            auth: auth.clone(),
            ui,
            home: home.into(),
        };
        #[allow(unused_mut)]
        let mut app = engine_auth::web::router(web.clone());
        #[cfg(feature = "admin")]
        if let Some((store, content)) = admin_parts {
            #[allow(unused_mut)]
            let mut admin = engine_admin::Admin::new(web, store, content.clone());
            #[cfg(feature = "forms")]
            if let Some(f) = &extras.forms {
                admin = admin.with_forms(f.clone());
            }
            #[cfg(feature = "media")]
            if let Some(m) = &extras.media {
                admin = admin.with_media(m.clone());
            }
            #[cfg(feature = "jobs")]
            if jobs_parts.is_some() {
                admin = admin.with_jobs(engine_jobs::Jobs::new(conn.db.clone()));
            }
            #[cfg(feature = "payments")]
            if extras.shop.is_some() {
                admin = admin.with_orders(engine_payments::OrderStore::new(conn.db.clone()));
            }
            #[cfg(feature = "api")]
            {
                admin = admin.with_api_tokens(engine_api::TokenStore::new(conn.db.clone()));
            }
            #[cfg(feature = "workflow")]
            {
                admin = admin.with_preview(Arc::new(crate::preview::Preview {
                    rm: rm.clone(),
                    content,
                    public_url: public_url.clone(),
                }));
            }
            app = app.merge(engine_admin::router(admin));
            println!("Admin at http://{addr}/admin/");
        }
        #[cfg(feature = "api")]
        if let Some((store, content)) = api_parts {
            let api = engine_api::web::Api::new(
                auth.clone(),
                store,
                content,
                engine_api::ApiConfig::from_site(cfg.api.as_ref())?,
            );
            app = app.merge(engine_api::web::router(api));
            println!("JSON API at http://{addr}/api/v1/");
        }
        #[cfg(feature = "payments")]
        if let Some(shop) = &extras.shop {
            let provider = engine_payments::provider(&shop.config)?;
            let payments = engine_payments::Payments::new(
                conn.db.clone(),
                provider,
                shop.catalogue.clone(),
                shop.config.clone(),
                &public_url,
            )?;
            let name = payments.provider().name();
            app = app.merge(engine_payments::web::router(
                payments,
                auth.clone(),
                anon,
                &cfg.site.name,
            ));
            println!(
                "Payments: provider `{name}`, webhooks to {}/_engine/payments/webhook/{name}/",
                public_url.trim_end_matches('/')
            );
            if name == "fake" {
                eprintln!(
                    "warning: the FAKE payment provider is on ({}=1): no money moves and every \
                     payment succeeds when asked to. Never use it for a live shop.",
                    engine_payments::FAKE_ENV
                );
            }
            if !opts.workers {
                eprintln!(
                    "note: payment events are applied to orders by job workers; with \
                     --no-workers, run `engine jobs work` alongside"
                );
            }
        }
        if conn.rt.block_on(auth.user_count())? == 0 {
            println!(
                "No accounts yet. Create the first administrator with:\n  engine user create --site {} --email you@example.org",
                site.display()
            );
        }
        app_router = Some(app);
    }
    #[cfg(any(feature = "auth", feature = "forms"))]
    {
        config.on_request = Some(rm.clone().handler());
    }
    #[cfg(feature = "forms")]
    if let Some(forms) = &extras.forms {
        let hooks = Arc::new(crate::formhooks::FormHooks {
            rm: rm.clone(),
            #[cfg(feature = "mail")]
            mailer: mailer.clone(),
            public_url: public_url.clone(),
        });
        let r = engine_forms::web::router(forms.clone(), hooks, anon, behind_proxy);
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
    }
    #[cfg(any(feature = "auth", feature = "forms"))]
    let _ = behind_proxy;
    #[cfg(feature = "media")]
    if let Some(m) = &extras.media {
        let r = engine_media::web::router(m.clone());
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
    }
    config.app = app_router;

    // Pick up template and Markdown edits.
    #[cfg(feature = "publish")]
    let reload_publisher = publisher.clone();
    let reload_own = own.clone();
    let reload_ext = ext.clone();
    let reload_site = site.clone();
    let _watcher = watch_sources(&site, &output, move || {
        let fresh = match Renderer::load(&reload_site, false, reload_ext.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "templates changed, but the site does not load: {e:#} (still serving the previous version)"
                );
                return;
            }
        };
        for w in fresh.warnings() {
            eprintln!("warning: {w}");
        }
        #[cfg(feature = "publish")]
        if let Some(p) = &reload_publisher {
            match p.reload(fresh) {
                Ok(r) => println!("{}", r.summary()),
                Err(e) => eprintln!("re-rendering after a template change failed: {e:#}"),
            }
            return;
        }
        *reload_own.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(fresh);
        println!("Templates reloaded.");
    })?;

    println!(
        "Serving {} at http://{addr}/ (Ctrl+C to stop)",
        output.display()
    );
    let run_workers = opts.workers;
    #[cfg(not(feature = "jobs"))]
    let _ = run_workers;
    let db = conn.db.clone();
    let evict_current = current.clone();
    conn.rt.block_on(async move {
        let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        // A new database (or one restored without its index) gets its
        // search index built once; later the change feed keeps it current.
        #[cfg(feature = "search")]
        if let Some(s) = &search
            && s.len().await.unwrap_or(0) == 0
        {
            match s.reindex_all().await {
                Ok(n) => println!("Search index: {n} document(s)."),
                Err(e) => eprintln!("search: building the index failed: {e:#}"),
            }
        }
        #[cfg(feature = "publish")]
        let listener = if let Some(p) = publisher {
            let mut reports = p.subscribe();
            let cur = evict_current.clone();
            #[cfg(feature = "search")]
            let search = search.clone();
            tokio::spawn(async move {
                while let Ok(r) = reports.recv().await {
                    // The search index follows the same change feed.
                    #[cfg(feature = "search")]
                    if let Some(s) = &search {
                        for t in &r.tags {
                            let mut parts = t.splitn(3, ':');
                            if let (Some("i"), Some(ty), Some(id)) =
                                (parts.next(), parts.next(), parts.next())
                                && let Ok(id) = id.parse::<i64>()
                                && let Err(e) = s.reindex_item(ty, id).await
                            {
                                eprintln!("search: indexing {ty} #{id}: {e:#}");
                            }
                        }
                    }
                    // The fragment cache drops what the change touched.
                    if let Some(c) = cur().fragments() {
                        if r.reason == "rebuild" {
                            c.clear();
                        } else {
                            c.evict(&r.tags);
                        }
                    }
                    if !r.is_empty() {
                        println!("{}", r.summary());
                    }
                }
            });
            let mut stop = stop_tx.subscribe();
            Some(tokio::spawn(engine_publish::listener::listen(
                db.clone(),
                p,
                async move {
                    let _ = stop.recv().await;
                },
            )))
        } else {
            None
        };
        let _ = &evict_current;
        #[cfg(feature = "jobs")]
        let workers = if let (true, Some(parts)) = (run_workers, jobs_parts) {
            let crate::jobs::Parts {
                handlers,
                config: jobs_cfg,
                ..
            } = parts;
            let mut stop = stop_tx.subscribe();
            let wo = engine_jobs::WorkerOptions::from_config(&jobs_cfg)?;
            let mut wo = wo;
            wo.schedules.extend(crate::jobs::engine_schedules());
            println!(
                "Job workers: {} ({} kinds)",
                wo.concurrency,
                handlers.kinds().len()
            );
            Some(tokio::spawn(engine_jobs::run(
                db.clone(),
                handlers,
                wo,
                async move {
                    let _ = stop.recv().await;
                },
            )))
        } else {
            None
        };
        let _ = &db;
        let served = serve::serve(config, addr, crate::shutdown_signal()).await;
        let _ = stop_tx.send(());
        #[cfg(feature = "jobs")]
        if let Some(w) = workers {
            match w.await {
                Ok(Err(e)) => eprintln!("job workers stopped: {e:#}"),
                Err(e) => eprintln!("job workers stopped: {e}"),
                Ok(Ok(())) => {}
            }
        }
        #[cfg(feature = "publish")]
        if let Some(listener) = listener {
            match listener.await {
                Ok(Err(e)) => eprintln!("publish listener stopped: {e:#}"),
                Err(e) => eprintln!("publish listener stopped: {e}"),
                Ok(Ok(())) => {}
            }
        }
        served
    })?;
    Ok(true)
}

/// Watch `templates/` and `content/` and call `reload` after edits
/// (debounced). Type files and `site.toml` need a restart; say so.
fn watch_sources(
    site: &Path,
    output: &Path,
    reload: impl Fn() + Send + 'static,
) -> Result<notify::RecommendedWatcher> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("starting the file watcher")?;
    watcher
        .watch(site, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", site.display()))?;
    let site: PathBuf = site.to_path_buf();
    let output = output.to_path_buf();
    std::thread::spawn(move || {
        let kind = |p: &Path| -> Option<&'static str> {
            if p.starts_with(&output) {
                return None;
            }
            let rel = p.strip_prefix(&site).ok()?;
            let name = rel.file_name()?.to_string_lossy();
            if name.starts_with('.') || name.ends_with('~') || name.ends_with(".swp") {
                return None;
            }
            match rel.components().next()?.as_os_str().to_str()? {
                "templates" | "content" => Some("reload"),
                "types" | "site.toml" | "migrations" | "forms" => Some("restart"),
                _ => None,
            }
        };
        while let Ok(res) = rx.recv() {
            let Ok(ev) = res else { continue };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                continue;
            }
            let kinds: Vec<&str> = ev.paths.iter().filter_map(|p| kind(p)).collect();
            if kinds.is_empty() {
                continue;
            }
            let mut restart = kinds.contains(&"restart");
            let mut reload_needed = kinds.contains(&"reload");
            while let Ok(Ok(more)) = rx.recv_timeout(Duration::from_millis(200)) {
                for p in &more.paths {
                    match kind(p) {
                        Some("restart") => restart = true,
                        Some("reload") => reload_needed = true,
                        _ => {}
                    }
                }
            }
            if restart {
                eprintln!(
                    "site.toml, types/, forms/ or migrations/ changed: restart `engine serve` to \
                     use them (after `engine types sync` and `engine migrate` if types changed)"
                );
            }
            if reload_needed {
                println!("Templates or content changed; reloading.");
                reload();
            }
        }
    });
    Ok(watcher)
}
