//! `engine serve` for a site with a database: the static server plus, per
//! compiled feature, render-on-miss and the publish listener (`publish`),
//! `request`-mode pages and `/account/` (`auth`), and `/admin/` (`admin`).
//! Template and Markdown edits are picked up while it runs.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use engine_core::builder::{self, BuildOptions, Renderer, SiteExtension};
use engine_core::serve::{self, ServeConfig};
use notify::{RecursiveMode, Watcher};

/// The renderer currently in use (swapped when templates change).
pub type CurrentRenderer = Arc<dyn Fn() -> Arc<Renderer> + Send + Sync>;

/// Serve a site that has database content or accounts. `Ok(false)` if it
/// has neither (the plain static server applies).
pub fn serve(site: &Path, addr: SocketAddr) -> Result<bool> {
    let site = site
        .canonicalize()
        .with_context(|| format!("site folder {}", site.display()))?;
    let (cfg, _) = builder::load_config(&site)?;
    #[cfg(feature = "content")]
    let has_content = engine_content::site::uses_content(&site, &cfg);
    #[cfg(not(feature = "content"))]
    let has_content = false;
    let has_db = crate::db::db_config(&site)?.is_some();
    let has_accounts = cfg!(feature = "auth") && has_db;
    if !(has_content || has_accounts) {
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

    // Content: the template extension, the store, the publisher.
    #[allow(unused_mut)]
    let mut ext: Option<Arc<dyn SiteExtension>> = None;
    #[cfg(feature = "admin")]
    let mut admin_parts = None;
    #[cfg(feature = "publish")]
    let mut publisher: Option<Arc<engine_publish::Publisher>> = None;
    #[cfg(feature = "content")]
    if has_content {
        let migrations = crate::db::migrations_for(&site, conn.db.backend())?;
        let app = conn.rt.block_on(engine_content::ContentApp::open(
            &site,
            conn.db.clone(),
            conn.rt.handle().clone(),
            &migrations,
        ))?;
        ext = Some(app.extension());
        #[cfg(feature = "admin")]
        {
            admin_parts = Some((app.store.clone(), app.content.clone()));
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
    #[cfg_attr(not(feature = "auth"), allow(unused_variables))]
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

    #[cfg(feature = "auth")]
    {
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
        let auth = engine_auth::Auth::new(
            conn.db.clone(),
            engine_auth::AuthConfig::from_site(cfg.auth.as_ref())?,
            &cfg.site.name,
            &public_url,
            Arc::new(engine_auth::LogNotifier),
        );
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
            app = app.merge(engine_admin::router(engine_admin::Admin::new(
                web, store, content,
            )));
            println!("Admin at http://{addr}/admin/");
        }
        if conn.rt.block_on(auth.user_count())? == 0 {
            println!(
                "No accounts yet. Create the first administrator with:\n  engine user create --site {} --email you@example.org",
                site.display()
            );
        }
        config.app = Some(app);
        config.on_request = Some(crate::request::handler(current.clone(), auth));
    }

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
    let db = conn.db.clone();
    conn.rt.block_on(async move {
        #[cfg(feature = "publish")]
        let stop = if let Some(p) = publisher {
            let mut reports = p.subscribe();
            tokio::spawn(async move {
                while let Ok(r) = reports.recv().await {
                    println!("{}", r.summary());
                }
            });
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
            let listener = tokio::spawn(engine_publish::listener::listen(db, p, async {
                let _ = stop_rx.await;
            }));
            Some((stop_tx, listener))
        } else {
            None
        };
        #[cfg(not(feature = "publish"))]
        let _ = db;
        let served = serve::serve(config, addr, crate::shutdown_signal()).await;
        #[cfg(feature = "publish")]
        if let Some((stop_tx, listener)) = stop {
            let _ = stop_tx.send(());
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
                "types" | "site.toml" | "migrations" => Some("restart"),
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
                    "site.toml, types/ or migrations/ changed: restart `engine serve` to use them \
                     (after `engine types sync` and `engine migrate` if types changed)"
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
