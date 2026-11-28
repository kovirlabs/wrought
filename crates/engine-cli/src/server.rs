//! `engine serve` for a site with a database: the static server plus, per
//! compiled feature, render-on-miss and the publish listener (`publish`),
//! `request`-mode pages with the fragment cache, `/account/` (`auth`),
//! `/account/oidc/` (`oidc`), `/admin/` (`admin`), form posts (`forms`), job
//! workers (`jobs`), email (`mail`) and the shop (`payments`). Template and
//! Markdown edits are picked up while it runs.

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

/// Options of `engine serve` (and of `engine dev`, which runs the same
/// server with [`DevServe`]).
pub struct ServeOptions {
    /// Run job workers in this process (`jobs` feature).
    pub workers: bool,
    /// `engine dev`: live reload, rebuilds and the developer tools. Only
    /// `dev::run` sets it; `engine serve` passes `None`.
    pub dev: Option<DevServe>,
}

/// What `engine dev` adds to the server.
pub struct DevServe {
    pub tools: Arc<crate::devtools::DevTools>,
    /// Bumped after each rebuild (the browser reloads).
    pub version: tokio::sync::watch::Sender<u64>,
    /// The build's options (`--drafts`).
    pub build: BuildOptions,
    /// TagCheck after each rebuild.
    pub checker: Option<crate::check::Checker>,
}

/// Whether `site` needs this server (database content, or a database and a
/// feature that uses it) rather than the plain static one. An error when
/// the site crate's handlers need a database the site does not name.
pub fn wanted(site: &Path) -> Result<bool> {
    let (cfg, _) = builder::load_config(site)?;
    #[cfg(feature = "content")]
    let has_content = engine_content::site::uses_content(site, &cfg);
    #[cfg(not(feature = "content"))]
    let has_content = false;
    let _ = &cfg;
    let has_db = crate::db::db_config(site)?.is_some();
    let db_features = cfg!(any(
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media",
        feature = "realtime"
    ));
    if !(has_content || (has_db && db_features)) {
        if crate::app::needs_database() {
            bail!(
                "this site's handlers and queries (App::route, App::query) need its database: \
                 set `[database] url` in site.toml or {}",
                engine_db::config::URL_ENV
            );
        }
        return Ok(false);
    }
    Ok(true)
}

/// Serve a site that has database content, accounts, forms or jobs.
/// `Ok(false)` if it has none of them (the plain static server applies).
pub fn serve(site: &Path, addr: SocketAddr, opts: ServeOptions) -> Result<bool> {
    let site = site
        .canonicalize()
        .with_context(|| format!("site folder {}", site.display()))?;
    if !wanted(&site)? {
        return Ok(false);
    }
    let (cfg, _) = builder::load_config(&site)?;
    #[cfg(feature = "content")]
    let has_content = engine_content::site::uses_content(&site, &cfg);
    let ServeOptions { workers, dev } = opts;
    let command = if dev.is_some() {
        "engine dev"
    } else {
        "engine serve"
    };
    let drafts = dev.as_ref().is_some_and(|d| d.build.drafts);
    let output = builder::output_dir(&site, &cfg, &BuildOptions::default());
    if !output.is_dir() {
        bail!(
            "{} does not exist; run `engine build` first",
            output.display()
        );
    }
    // The hosting admin's choice (environment or /etc/hammerwright/host.toml),
    // never the site's.
    let idle_exit = engine_core::host::engine_idle_exit()?;
    // A sleeping PostgreSQL: also the hosting admin's (off by default).
    let db_sleep = engine_core::host::db_sleep()?;
    let sleep_hooks = db_sleep.as_ref().map(|s| {
        (
            engine_db::sleep::command(s.wake.clone(), s.wake_timeout, "wake"),
            engine_db::sleep::command(s.sleep.clone(), engine_db::sleep::SLEEP_TIMEOUT, "sleep"),
        )
    });
    let sleep_state = site.join(".engine/db-sleep.json");
    // Listen first: published pages are answered from here on, while the
    // database is reached (it may be asleep or down), its migrations are
    // checked and the features are wired. Nothing that needs the database
    // is answered before that is done (DECISIONS.md, "Start on demand").
    let rt = crate::runtime()?;
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
    // Posts wait on disk while the database is unavailable, and are
    // written to it when it is back; from the first moment, before the
    // database is reached, for the forms defined in the site's files
    // (DECISIONS.md, "Spool form posts during start").
    #[cfg(feature = "forms")]
    let spool = {
        let dir = std::env::var_os(engine_forms::spool::SPOOL_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| site.join(".engine/spool/forms"));
        let spool = engine_forms::spool::Spool::open(
            &dir,
            engine_core::limits::current().form_spool_bytes,
        )?;
        if !spool.is_empty() {
            println!(
                "Form spool: {} post(s) waiting in {} (written to the database once it answers)",
                spool.len(),
                dir.display()
            );
        }
        Arc::new(spool)
    };
    #[cfg(feature = "forms")]
    let offline_forms = engine_forms::Forms::offline(&site, spool.clone(), rt.handle().clone())?;
    #[cfg(feature = "forms")]
    let early_app = offline_forms.as_ref().map(|f| {
        engine_forms::web::router(
            f.clone(),
            Arc::new(crate::formhooks::OfflineHooks),
            anon,
            behind_proxy,
        )
        .fallback(|| async { serve::unavailable(serve::UNAVAILABLE_PAGE) })
    });
    #[cfg(not(feature = "forms"))]
    let early_app = None;
    let starting = rt.block_on(serve::Starting::early(
        &output,
        addr,
        crate::shutdown_signal(),
        early_app,
    ))?;
    let asleep = match (&db_sleep, &sleep_hooks) {
        (Some(s), Some((wake, _))) => Some(crate::db::AsleepStart {
            wake,
            wake_timeout: s.wake_timeout,
            state_file: &sleep_state,
            idle_exit,
        }),
        _ => None,
    };
    let Some(db) = crate::db::connect_when_reachable(&site, &rt, &starting, asleep)? else {
        rt.block_on(starting.abandon());
        return Ok(true);
    };
    let conn = crate::db::Conn {
        rt,
        db,
        site: site.clone(),
    };
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
    // The content types, whose tags anyone may follow live.
    #[cfg(all(feature = "content", feature = "realtime"))]
    let mut live_content: Option<engine_content::ContentSite> = None;
    #[cfg(feature = "content")]
    if has_content {
        let migrations = crate::db::migrations_for(&site)?;
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
        #[cfg(feature = "realtime")]
        {
            live_content = Some(app.content.clone());
        }
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
            let p = Arc::new(crate::cms::publisher(&cms, &site, &cfg, conn.rt.handle())?);
            // Assets under the names this renderer links (static/ may have
            // changed since the last build).
            p.write_assets()
                .context("writing the site's assets to the output folder")?;
            publisher = Some(p);
        }
    }

    let own = Arc::new(RwLock::new(Arc::new(Renderer::load(
        &site,
        drafts,
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
        idle_exit,
        // `engine dev` only: live reload and the developer tools' layer.
        live_reload: dev.as_ref().map(|d| d.version.subscribe()),
        outer: dev.as_ref().map(|d| d.tools.layer()),
        ..Default::default()
    };
    #[cfg(feature = "forms")]
    #[allow(unused_mut)]
    let mut idle_spool: Option<Arc<engine_forms::spool::Spool>> = None;
    #[allow(unused_mut, unused_assignments)]
    let mut app_router: Option<axum::Router> = None;

    #[cfg(feature = "publish")]
    if let Some(p) = publisher.as_ref().filter(|p| p.renders_on_miss()) {
        let p = p.clone();
        config.on_miss = Some(Arc::new(move |path: String| {
            let p = p.clone();
            Box::pin(async move {
                let shown = path.clone();
                match tokio::task::spawn_blocking(engine_core::dev::carry(move || {
                    p.render_missing(&path)
                }))
                .await
                {
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

    // Jobs and mail.
    #[cfg(feature = "jobs")]
    let jobs_parts = Some(crate::jobs::parts(&site, &cfg, &conn.db, &public_url)?);
    // `engine dev`: mail is kept and shown, never sent.
    #[cfg(feature = "mail")]
    if let (Some(d), Some(p)) = (&dev, &jobs_parts) {
        d.tools.capture_mail(&p.mailer);
        println!("Mail is captured, not sent: http://{addr}/_engine/dev/mail/");
    }
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
    // Live regions: one hub per process, sized by the machine
    // (DECISIONS.md, "Real-time"); open streams keep the site awake only
    // when the hosting admin says so.
    #[cfg(feature = "realtime")]
    let live = engine_realtime::Hub::from_limits();
    #[cfg(feature = "realtime")]
    let live_keeps_awake = engine_core::host::live_keeps_awake()?;
    // Each stream is an open file: take the hard limit, as servers do.
    #[cfg(feature = "realtime")]
    if let Some(n) = engine_core::resources::raise_open_files() {
        let want = live.stats().limit as u64 + 256;
        if n < want {
            eprintln!(
                "note: the open-file limit ({n}) is below the live-region subscriber limit plus \
                 room for requests ({want}): raise it (LimitNOFILE=) or set [limits] \
                 live_subscribers"
            );
        }
    }
    // What the site crate's handlers and hooks get (App::route...).
    let app_cx = crate::app::Context::new(crate::app::ContextParts {
        db: conn.db.clone(),
        site: site.clone(),
        public_url: public_url.clone(),
        current: current.clone(),
        #[cfg(feature = "auth")]
        auth: auth.clone(),
        #[cfg(feature = "mail")]
        mailer: mailer.clone(),
        #[cfg(feature = "realtime")]
        live: live.clone(),
    });
    #[cfg(any(feature = "auth", feature = "forms"))]
    let rm = crate::request::RequestMode {
        current: current.clone(),
        csrf: anon,
        db: conn.db.clone(),
        renders: Arc::new(tokio::sync::Semaphore::new(
            conn.db.max_connections().max(1) as usize,
        )),
        published: output.clone(),
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
        #[cfg(feature = "oidc")]
        if !auth.oidc_providers().is_empty() {
            let oidc = engine_auth::oidc::Oidc::new(auth.oidc_providers())?;
            app = app.merge(engine_auth::oidc::router(web.clone(), oidc));
            let names: Vec<&str> = auth
                .oidc_providers()
                .iter()
                .map(|p| p.name.as_str())
                .collect();
            println!("Single sign-on with: {}", names.join(", "));
        }
        #[cfg(feature = "admin")]
        if let Some((store, content)) = admin_parts {
            #[allow(unused_mut)]
            let mut admin = engine_admin::Admin::new(web, store, content.clone()).with_design(
                engine_admin::design::Design {
                    components: crate::components::catalogue(&site)?.to_json(),
                    tokens: crate::components::tokens(&site)?,
                },
            );
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
            #[cfg(any(feature = "workflow", feature = "visual-editor"))]
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
            if !workers {
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
            app: app_cx.clone(),
            #[cfg(feature = "mail")]
            mailer: mailer.clone(),
            public_url: public_url.clone(),
        });
        // The same spool (and outage limit) as during the start.
        idle_spool = Some(spool.clone());
        forms.set_spool(spool.clone());
        if let Some(early) = &offline_forms {
            forms.share_outage_limit(early);
        }
        // So that the next start can take posts before the database.
        if let Err(e) = forms.remember_stamp_key() {
            eprintln!("forms: keeping the stamp key in the spool's folder: {e:#}");
        }
        conn.rt.spawn(engine_forms::web::replay_forever(
            forms.clone(),
            hooks.clone(),
        ));
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
    // Health: anyone gets the status only (`ok` or `degraded`, 200 or
    // 503), with no error text. The detail (the database's last error,
    // outages, probes, the form spool) is for a caller on this machine that
    // did not come through a proxy, or one with ENGINE_HEALTH_TOKEN as a
    // bearer token. `?check` probes the database, at most once per
    // HEALTH_PROBE_EVERY whoever asks; otherwise the last known state is
    // reported without a query.
    {
        let db = conn.db.clone();
        #[cfg(feature = "forms")]
        let spool = extras.forms.as_ref().and_then(|f| f.spool().cloned());
        #[cfg(feature = "realtime")]
        let live = live.clone();
        let token = std::env::var(HEALTH_TOKEN_ENV)
            .ok()
            .filter(|t| t.len() >= 16);
        let r = axum::Router::new().route(
            "/_engine/health",
            axum::routing::get(
                move |q: axum::extract::RawQuery,
                      peer: axum::extract::ConnectInfo<SocketAddr>,
                      headers: axum::http::HeaderMap| {
                    let db = db.clone();
                    #[cfg(feature = "forms")]
                    let spool = spool.clone();
                    #[cfg(feature = "realtime")]
                    let live = live.clone();
                    let token = token.clone();
                    async move {
                        if q.0
                            .as_deref()
                            .is_some_and(|q| q.split('&').any(|p| p == "check"))
                        {
                            db.health().check_recent(HEALTH_PROBE_EVERY).await;
                        }
                        let st = db.health().status();
                        // Asleep is a normal state, not a failure (and this
                        // never wakes it).
                        let ok = st.up || st.asleep;
                        #[allow(unused_mut)]
                        let mut body = serde_json::json!({
                            "status": if ok { "ok" } else { "degraded" },
                        });
                        if health_detail(peer.0, &headers, token.as_deref()) {
                            body["database"] = serde_json::json!({
                                "up": st.up,
                                "down_for_s": st.down_for.map(|d| d.as_secs_f64()),
                                "outages": st.outages,
                                "probes": st.probes,
                                "last_error": st.last_error,
                            });
                            let sl = db.health().sleep_status();
                            if sl.on {
                                body["database"]["sleep"] = serde_json::json!({
                                    "asleep": sl.asleep, "sleeps": sl.sleeps, "wakes": sl.wakes,
                                    "wake_failures": sl.wake_failures,
                                    "last_wake_ms": sl.last_wake_ms,
                                    "last_sleep_error": sl.last_sleep_error,
                                });
                            }
                            #[cfg(feature = "realtime")]
                            {
                                body["live"] = serde_json::json!(live.stats());
                            }
                            #[cfg(feature = "forms")]
                            if let Some(s) = &spool {
                                body["form_spool"] = serde_json::json!({
                                    "posts": s.len(), "bytes": s.used(),
                                    "limit_bytes": s.limit(), "quarantined": s.quarantined(),
                                });
                            }
                        }
                        let code = if ok {
                            axum::http::StatusCode::OK
                        } else {
                            axum::http::StatusCode::SERVICE_UNAVAILABLE
                        };
                        (
                            code,
                            [
                                (axum::http::header::CACHE_CONTROL, "no-store"),
                                (axum::http::header::CONTENT_TYPE, "application/json"),
                            ],
                            body.to_string(),
                        )
                    }
                },
            ),
        );
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
    }
    // Live regions' event streams: public tags only (the content types'
    // and those the site crate declared public).
    #[cfg(feature = "realtime")]
    {
        let declared = crate::app::live_public();
        #[cfg(feature = "content")]
        let content = live_content.clone();
        let public = engine_realtime::Public::new(move |t: &engine_core::tags::CacheTag| {
            if declared
                .iter()
                .any(|p| engine_realtime::pattern_matches(p, t))
            {
                return true;
            }
            #[cfg(feature = "content")]
            if let Some(c) = &content {
                use engine_core::tags::CacheTag;
                let reg = c.registry();
                return match t {
                    CacheTag::Item { kind, .. } | CacheTag::Collection { kind } => {
                        reg.get(kind).is_some_and(|ty| !ty.singleton) || reg.is_vocabulary(kind)
                    }
                    CacheTag::Singleton { name } => reg.get(name).is_some_and(|ty| ty.singleton),
                };
            }
            let _ = t;
            false
        });
        if let Some(d) = &dev {
            d.tools.set_live_public(public.clone());
        }
        let r = engine_realtime::router(live.clone(), public);
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
        // Changes another process published: its fragments are dropped
        // here too, so a region fetching again gets the new content.
        let cur = current.clone();
        live.set_evict(Arc::new(move |tags: &[String]| {
            if let Some(c) = cur().fragments() {
                c.evict(tags);
            }
        }));
        let l = live.clone();
        config.on_shutdown = Some(Arc::new(move || l.close(engine_core::host::idle_exited())));
    }
    // The site crate's handlers, beside the engine's routes (resolved now:
    // nothing is looked up per request that axum's router does not).
    if let Some(r) = crate::app::router(&app_cx) {
        println!(
            "Site handlers: {}",
            crate::app::registry().server.routes.len()
        );
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
    }
    // `engine dev`'s pages (requests, queries, mail).
    if let Some(d) = &dev {
        let r = d.tools.router();
        app_router = Some(match app_router {
            Some(a) => a.merge(r),
            None => r,
        });
    }
    // Every handler answers 503 itself when its error's chain says the
    // database is unavailable; this catches one that did not while the
    // database is known to be down.
    let health = conn.db.health().clone();
    config.app = app_router.map(|a| {
        a.layer(axum::middleware::map_response(
            move |mut r: axum::response::Response| {
                let health = health.clone();
                async move {
                    if r.status() == axum::http::StatusCode::INTERNAL_SERVER_ERROR
                        && health.is_down()
                    {
                        *r.status_mut() = axum::http::StatusCode::SERVICE_UNAVAILABLE;
                        let h = r.headers_mut();
                        h.insert(
                            axum::http::header::RETRY_AFTER,
                            axum::http::HeaderValue::from_static("30"),
                        );
                        h.insert(
                            axum::http::header::CACHE_CONTROL,
                            axum::http::HeaderValue::from_static("no-store"),
                        );
                    }
                    r
                }
            },
        ))
    });

    // Pick up template and Markdown edits.
    #[cfg(feature = "publish")]
    let reload_publisher = publisher.clone();
    let reload_own = own.clone();
    let reload_ext = ext.clone();
    let reload_site = site.clone();
    // `engine dev`: after a reload, the browser reloads (and TagCheck runs).
    let after = dev.as_ref().map(|d| {
        (
            d.version.clone(),
            d.build.clone(),
            d.checker.clone(),
            output.clone(),
        )
    });
    let _watcher = watch_sources(&site, &output, command, move || {
        let fresh = match Renderer::load(&reload_site, drafts, reload_ext.clone()) {
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
        let done = |ok: bool| {
            if let (true, Some((version, _, checker, out))) = (ok, &after) {
                version.send_modify(|v| *v += 1);
                if let Some(c) = checker {
                    match crate::check::dev_summary(c, out) {
                        Ok(s) => println!("{s}"),
                        Err(e) => eprintln!("TagCheck failed: {e:#}"),
                    }
                }
            }
        };
        #[cfg(feature = "publish")]
        if let Some(p) = &reload_publisher {
            match p.reload(fresh) {
                Ok(r) => {
                    println!("{}", r.summary());
                    done(true);
                }
                Err(e) => eprintln!("re-rendering after a template change failed: {e:#}"),
            }
            return;
        }
        *reload_own.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(fresh);
        println!("Templates reloaded.");
        // `engine dev` without a publisher: the build's pages are written
        // again too (`engine serve` leaves them to `engine build`).
        if let Some((_, opts, ..)) = &after {
            match builder::build_site_with(&reload_site, opts, reload_ext.clone()) {
                Ok(r) => {
                    crate::print_report(&r, false);
                    done(true);
                }
                Err(e) => eprintln!("build failed: {e:#}"),
            }
        }
    })?;

    // Not idle while a job runs, or while the form spool holds posts that
    // could be written now (the database is up); posts waiting for a down
    // database do not keep it awake: the next start replays them.
    {
        #[cfg(feature = "forms")]
        let health = conn.db.health().clone();
        #[cfg(feature = "realtime")]
        let live = live_keeps_awake.then(|| live.clone());
        config.busy = Some(Arc::new(move || {
            #[cfg(feature = "jobs")]
            if engine_jobs::running() > 0 {
                return true;
            }
            #[cfg(feature = "realtime")]
            if live.as_ref().is_some_and(|l| l.subscribers() > 0) {
                return true;
            }
            #[cfg(feature = "forms")]
            if idle_spool.as_ref().is_some_and(|s| !s.is_empty()) && !health.is_down() {
                return true;
            }
            false
        }));
    }
    // The sleeping database's watchdog (the hosting admin's setting).
    #[allow(unused_mut)]
    let mut sleeper: Option<Arc<engine_db::sleep::Sleeper>> = None;
    if let (Some(s), Some((wake, sleep))) = (&db_sleep, sleep_hooks) {
        #[cfg(feature = "forms")]
        let spool = extras.forms.as_ref().and_then(|f| f.spool().cloned());
        #[cfg(feature = "realtime")]
        let live = live_keeps_awake.then(|| live.clone());
        #[cfg(feature = "jobs")]
        let jobs_db = conn.db.clone();
        // Only jobs this process's workers run keep it awake or wake it.
        #[cfg(feature = "jobs")]
        let kinds: Arc<Vec<String>> = Arc::new(
            jobs_parts
                .as_ref()
                .filter(|_| workers)
                .map(|p| p.handlers.kinds())
                .unwrap_or_default(),
        );
        let next_due: engine_db::sleep::NextDue =
            Arc::new(move || -> engine_db::sleep::DueFuture {
                #[cfg(feature = "jobs")]
                {
                    let (db, kinds) = (jobs_db.clone(), kinds.clone());
                    Box::pin(async move { engine_jobs::next_due(&db, &kinds).await })
                }
                #[cfg(not(feature = "jobs"))]
                Box::pin(async { Ok(None) })
            });
        let sl = engine_db::sleep::Sleeper {
            idle: s.idle,
            wake,
            sleep,
            wake_timeout: s.wake_timeout,
            busy: Arc::new(move || {
                #[cfg(feature = "jobs")]
                if engine_jobs::running() > 0 {
                    return true;
                }
                #[cfg(feature = "forms")]
                if spool.as_ref().is_some_and(|s| !s.is_empty()) {
                    return true;
                }
                #[cfg(feature = "realtime")]
                if live.as_ref().is_some_and(|l| l.subscribers() > 0) {
                    return true;
                }
                false
            }),
            next_due,
            state_file: sleep_state.clone(),
        };
        engine_db::sleep::enable(&conn.db, &sl);
        println!(
            "Database sleep: after {} s unused (the hosting admin's setting)",
            s.idle.as_secs()
        );
        sleeper = Some(Arc::new(sl));
    }
    println!(
        "Serving {} at http://{}/ (Ctrl+C to stop)",
        output.display(),
        starting.local_addr().unwrap_or(addr)
    );
    let run_workers = workers;
    #[cfg(not(feature = "jobs"))]
    let _ = run_workers;
    let db = conn.db.clone();
    let evict_current = current.clone();
    #[cfg(feature = "publish")]
    let hooks_cx = app_cx.clone();
    let _ = &app_cx;
    #[cfg(feature = "realtime")]
    if live_keeps_awake {
        println!("Live regions: open streams keep the site awake (the hosting admin's setting)");
    }
    conn.rt.block_on(async move {
        let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        // A new database (or one restored without its index) gets its
        // search index built once; later the change feed keeps it current.
        // Checked beside serving, not before it (a cold start answers
        // sooner; an index that is already built costs one lookup).
        #[cfg(feature = "search")]
        if let Some(s) = &search {
            // On PostgreSQL the index is an unlogged table, which a crash or
            // failover of the database server empties while this server
            // keeps running: look once a minute (one indexed lookup) and
            // refill it.
            let s = s.clone();
            let db_health = db.health().clone();
            tokio::spawn(async move {
                match s.ensure_built().await {
                    Ok(Some(n)) => println!("Search index: {n} document(s)."),
                    Ok(None) => {}
                    Err(e) => eprintln!("search: building the index failed: {e:#}"),
                }
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if db_health.is_down() {
                        continue;
                    }
                    match engine_db::sleep::quietly(s.ensure_built()).await {
                        Ok(Some(n)) => {
                            println!("Search index was empty; rebuilt: {n} document(s).")
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("search: checking the index: {e:#}"),
                    }
                }
            });
        }
        #[cfg(feature = "publish")]
        let listener = if let Some(p) = publisher {
            // The site crate's publish hooks (App::on_publish).
            crate::app::spawn_publish_hooks(&p, &hooks_cx);
            let mut reports = p.subscribe();
            let cur = evict_current.clone();
            #[cfg(feature = "search")]
            let search = search.clone();
            #[cfg(feature = "realtime")]
            let (live, live_db) = (live.clone(), db.clone());
            tokio::spawn(async move {
                while let Ok(r) = reports.recv().await {
                    #[allow(unused_mut)]
                    let mut changed = r.tags.clone();
                    // The search index follows the same change feed.
                    #[cfg(feature = "search")]
                    if let Some(s) = &search {
                        let mut reindexed = false;
                        for t in &r.tags {
                            let mut parts = t.splitn(3, ':');
                            if let (Some("i"), Some(ty), Some(id)) =
                                (parts.next(), parts.next(), parts.next())
                                && let Ok(id) = id.parse::<i64>()
                            {
                                reindexed = true;
                                if let Err(e) = s.reindex_item(ty, id).await {
                                    eprintln!("search: indexing {ty} #{id}: {e:#}");
                                }
                            }
                        }
                        // Results pages recorded `c:search`: any reindexed
                        // item may change them (after the index has it).
                        if reindexed {
                            changed.push("c:search".into());
                        }
                    }
                    // The fragment cache (and the ETags of request pages)
                    // drops what the change touched.
                    let everything = r.reason == "rebuild" || r.reason == "templates changed";
                    if let Some(c) = cur().fragments() {
                        if everything {
                            c.clear();
                        } else {
                            c.evict(&changed);
                        }
                    }
                    // Then live regions fetch again, here and in the other
                    // engine processes (which drop their fragments too).
                    #[cfg(feature = "realtime")]
                    {
                        let sent = if everything {
                            live.reset(&live_db).await
                        } else {
                            live.change(&live_db, &changed).await
                        };
                        if let Err(e) = sent {
                            eprintln!("live: telling the other engine processes: {e:#}");
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
        // The one listening connection for the other processes' events
        // (open only while someone here subscribes).
        #[cfg(feature = "realtime")]
        {
            let mut stop = stop_tx.subscribe();
            tokio::spawn(engine_realtime::listen(
                db.clone(),
                live.clone(),
                async move {
                    let _ = stop.recv().await;
                },
            ));
        }
        if let Some(sl) = &sleeper {
            let mut stop = stop_tx.subscribe();
            tokio::spawn(engine_db::sleep::watch(
                db.clone(),
                sl.clone(),
                async move {
                    let _ = stop.recv().await;
                },
            ));
        }
        let served = starting.serve(config).await;
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
        // An idle exit with sleeping on: the engine is the database's only
        // user, so it sleeps now too (checked as usual first).
        if let Some(sl) = &sleeper
            && engine_core::host::idle_exited()
        {
            match engine_db::sleep::sleep_now(&db, sl).await {
                Ok(true) | Ok(false) => {}
                Err(e) => eprintln!("database: {e:#}"),
            }
        }
        served
    })?;
    Ok(true)
}

/// The bearer token that unlocks the health endpoint's detail for callers
/// elsewhere (at least 16 characters; unset: only local callers see it).
const HEALTH_TOKEN_ENV: &str = "ENGINE_HEALTH_TOKEN";

/// `/_engine/health?check` probes the database at most this often.
const HEALTH_PROBE_EVERY: Duration = Duration::from_secs(10);

/// Whether a health request may see the detail: a loopback caller that did
/// not come through a proxy (no forwarding headers; the shipped Caddy
/// configuration always adds them), or the right bearer token.
fn health_detail(peer: SocketAddr, headers: &axum::http::HeaderMap, token: Option<&str>) -> bool {
    use axum::http::header;
    if let (Some(want), Some(got)) = (
        token,
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ")),
    ) {
        // Constant time in the token's content.
        let same = want.len() == got.len()
            && want
                .bytes()
                .zip(got.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        if same {
            return true;
        }
    }
    let proxied = ["x-forwarded-for", "forwarded", "x-real-ip"]
        .iter()
        .any(|h| headers.contains_key(*h));
    peer.ip().is_loopback() && !proxied
}

/// Watch `templates/`, `content/` and `static/` and call `reload` after edits
/// (debounced). Type files and `site.toml` need a restart; say so.
fn watch_sources(
    site: &Path,
    output: &Path,
    command: &'static str,
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
                // `static/` too: its files' hashed names are in the pages.
                "templates" | "content" | "static" => Some("reload"),
                "types" | "site.toml" | "migrations" | "forms" => Some("restart"),
                // An app crate's Rust code: its binary has to be built again.
                "src" | "Cargo.toml" if site.join("Cargo.toml").is_file() => Some("rust"),
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
            let mut rust = kinds.contains(&"rust");
            while let Ok(Ok(more)) = rx.recv_timeout(Duration::from_millis(200)) {
                for p in &more.paths {
                    match kind(p) {
                        Some("restart") => restart = true,
                        Some("reload") => reload_needed = true,
                        Some("rust") => rust = true,
                        _ => {}
                    }
                }
            }
            if restart {
                eprintln!(
                    "site.toml, types/, forms/ or migrations/ changed: restart `{command}` to \
                     use them (after `engine types sync` and `engine migrate` if types changed)"
                );
            }
            if rust {
                eprintln!(
                    "the site crate's Rust code changed: it runs from its compiled binary, so \
                     stop this server and run `cargo run -- {}` again (or keep `cargo watch -x \
                     'run -- {0}'` running, which does that on every save)",
                    command.trim_start_matches("engine ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn health_detail_only_for_local_callers_or_the_token() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let remote: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let none = HeaderMap::new();
        assert!(health_detail(local, &none, None));
        assert!(!health_detail(remote, &none, None));
        // Through the front door: loopback peer, but forwarded.
        let mut fwd = HeaderMap::new();
        fwd.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        assert!(!health_detail(local, &fwd, None));
        let tok = "health token, long enough";
        let mut auth = HeaderMap::new();
        auth.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {tok}")).unwrap(),
        );
        assert!(health_detail(remote, &auth, Some(tok)));
        assert!(!health_detail(
            remote,
            &auth,
            Some("another token, long enough")
        ));
        assert!(!health_detail(remote, &auth, None));
    }
}
