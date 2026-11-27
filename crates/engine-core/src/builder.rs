//! Build mode: render every `build` route to the output folder.
//!
//! The build renders into a staging folder next to the output and swaps it
//! in at the end, so a failed build leaves the previous output untouched. An
//! existing output folder is only replaced if an earlier `engine build` made
//! it (it holds `.engine-manifest.json`) or it is empty.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use minijinja::Value;
use minijinja::context;
use serde::Serialize;

use crate::config::{RenderMode, SiteConfig};
use crate::content::Content;
use crate::routes::{self, Route, RouteSource};
use crate::tags::{self, CacheTag};
use crate::templates::{self, page_value};
use crate::ui::UiProviders;

/// Marker and tag index written into every build output.
pub const MANIFEST: &str = ".engine-manifest.json";

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Output folder; defaults to `[site] output` inside the site folder.
    pub output: Option<PathBuf>,
    /// Include pages with `draft = true`.
    pub drafts: bool,
    /// Render threads; `None` = `ENGINE_BUILD_JOBS`, else one per CPU this
    /// process may use.
    pub jobs: Option<usize>,
}

/// Environment variable: number of render threads for `engine build`.
pub const JOBS_ENV: &str = "ENGINE_BUILD_JOBS";
/// Environment variable: `1` pins each render thread to its own CPU (see
/// [`render_parallel`]).
pub const PIN_ENV: &str = "ENGINE_BUILD_PIN_CPUS";

/// Environment variable: `0` makes `engine build` write no pre-compressed
/// copies. A measuring switch (what the copies cost a build), not a site
/// setting: the next publish of a page writes its copies again.
pub const PRECOMPRESS_ENV: &str = "ENGINE_BUILD_PRECOMPRESS";

fn build_precompresses() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var(PRECOMPRESS_ENV).map_or(true, |v| v.trim() != "0"))
}

/// How many threads a build renders with.
pub fn build_jobs(opts: &BuildOptions) -> usize {
    opts.jobs
        .or_else(|| std::env::var(JOBS_ENV).ok()?.trim().parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .clamp(1, 64)
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderedRoute {
    pub path: String,
    pub output: PathBuf,
    pub template: String,
    pub mode: RenderMode,
    /// Cache tags the render recorded.
    pub tags: Vec<String>,
    /// SHA-256 (hex) of the bytes written, for the publish tag index (so it
    /// does not read every page back). Not in the manifest.
    #[serde(skip)]
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct BuildReport {
    pub output: PathBuf,
    pub routes: Vec<RenderedRoute>,
    pub static_files: usize,
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

#[derive(Serialize)]
struct Manifest<'a> {
    engine: &'static str,
    routes: &'a [RenderedRoute],
}

/// Hooks a feature crate uses to add data-backed routes, template
/// functions and built-in templates (components) without core depending on
/// it: `content` (pattern routes, `list_items()`...), `auth` (account
/// components), `forms` (`form()` and the form components).
///
/// Calls happen inside a synchronous render, on a thread that may block (the
/// publish pipeline renders in `spawn_blocking`).
pub trait SiteExtension: Send + Sync {
    /// Add template functions, filters and globals.
    fn extend_env(&self, env: &mut minijinja::Environment<'static>);

    /// Expand the pattern route `cfg.routes[index]` (one with `type` or
    /// `taxonomy`) into every page it currently has.
    fn expand(&self, cfg: &SiteConfig, index: usize) -> Result<Vec<DynamicPage>> {
        let _ = (cfg, index);
        Ok(Vec::new())
    }

    /// Resolve one concrete page of the pattern route `cfg.routes[index]`
    /// from its path parameters, or `None` if nothing (published) matches.
    fn resolve(
        &self,
        cfg: &SiteConfig,
        index: usize,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<DynamicPage>> {
        let _ = (cfg, index, params);
        Ok(None)
    }

    /// Whether this extension makes the pages of pattern and list routes
    /// (the `content` feature).
    fn handles_dynamic_routes(&self) -> bool {
        false
    }

    /// Built-in templates (e.g. `components/login_form.html`) searched after
    /// the site's `templates/` and before core's own.
    fn template_dirs(&self) -> Vec<&'static include_dir::Dir<'static>> {
        Vec::new()
    }

    /// A whole-site render starts (`engine build`, a full rebuild): data
    /// may be bulk-loaded once instead of queried per page, until
    /// [`SiteExtension::end_bulk`].
    fn begin_bulk(&self) -> Result<()> {
        Ok(())
    }

    /// The whole-site render ended (also after a failure).
    fn end_bulk(&self) {}
}

/// Several extensions as one (content plus the components of `auth`,
/// `forms`...). Pattern routes come from the first that resolves them.
#[derive(Clone, Default)]
pub struct Extensions(pub Vec<Arc<dyn SiteExtension>>);

impl Extensions {
    /// `None` when empty.
    pub fn into_option(self) -> Option<Arc<dyn SiteExtension>> {
        match self.0.len() {
            0 => None,
            1 => self.0.into_iter().next(),
            _ => Some(Arc::new(self)),
        }
    }
}

impl SiteExtension for Extensions {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        for e in &self.0 {
            e.extend_env(env);
        }
    }

    fn expand(&self, cfg: &SiteConfig, index: usize) -> Result<Vec<DynamicPage>> {
        let mut all = Vec::new();
        for e in self.0.iter().filter(|e| e.handles_dynamic_routes()) {
            all.extend(e.expand(cfg, index)?);
        }
        Ok(all)
    }

    fn resolve(
        &self,
        cfg: &SiteConfig,
        index: usize,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<DynamicPage>> {
        for e in self.0.iter().filter(|e| e.handles_dynamic_routes()) {
            if let Some(p) = e.resolve(cfg, index, params)? {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    fn handles_dynamic_routes(&self) -> bool {
        self.0.iter().any(|e| e.handles_dynamic_routes())
    }

    fn template_dirs(&self) -> Vec<&'static include_dir::Dir<'static>> {
        self.0.iter().flat_map(|e| e.template_dirs()).collect()
    }

    fn begin_bulk(&self) -> Result<()> {
        for e in &self.0 {
            e.begin_bulk()?;
        }
        Ok(())
    }

    fn end_bulk(&self) {
        for e in &self.0 {
            e.end_bulk();
        }
    }
}

/// Ends a bulk render when dropped (see [`Renderer::bulk`]).
pub struct BulkGuard<'a>(Option<&'a Arc<dyn SiteExtension>>);

impl Drop for BulkGuard<'_> {
    fn drop(&mut self) {
        if let Some(e) = self.0 {
            e.end_bulk();
        }
    }
}

/// One page of a pattern route, as an extension produces it.
#[derive(Debug, Clone)]
pub struct DynamicPage {
    /// Normalised URL path, e.g. `/articles/hello/`.
    pub path: String,
    /// What templates see as `page`. A minijinja value, so rendered HTML
    /// (sanitised Markdown fields) keeps its *safe* mark and needs no
    /// `| safe` in templates.
    pub page: Value,
    /// Tags of the data the page was made from (e.g. `i:article:3`).
    pub tags: Vec<CacheTag>,
    /// What templates see as `listing` (one page of a paginated list).
    pub listing: Option<Value>,
}

/// Load and validate a site's `site.toml`.
pub fn load_config(site_dir: &Path) -> Result<(SiteConfig, Vec<String>)> {
    let path = site_dir.join("site.toml");
    if !path.is_file() {
        bail!(
            "no site.toml in {} (create a site with `engine new <name>`, or pass --site)",
            site_dir.display()
        );
    }
    let cfg = SiteConfig::load(&path)?;
    let warnings = cfg.validate()?;
    Ok((cfg, warnings))
}

/// The output folder a build of `site_dir` writes to.
pub fn output_dir(site_dir: &Path, cfg: &SiteConfig, opts: &BuildOptions) -> PathBuf {
    opts.output
        .clone()
        .unwrap_or_else(|| site_dir.join(&cfg.site.output))
}

/// Everything needed to render any route of a site: config, Markdown
/// content, the template environment and the route table. `engine build`
/// uses it once; the publish pipeline keeps one alive and renders single
/// routes on demand.
pub struct Renderer {
    catalogue: Arc<crate::components::Catalogue>,
    cfg: SiteConfig,
    warnings: Vec<String>,
    content: Arc<Content>,
    env: minijinja::Environment<'static>,
    globals: Globals,
    ui: UiProviders,
    site_templates: Option<PathBuf>,
    template_dirs: Vec<&'static include_dir::Dir<'static>>,
    ext: Option<Arc<dyn SiteExtension>>,
    fragments: Option<Arc<crate::fragments::FragmentCache>>,
}

impl Renderer {
    /// Load a site. `ext` adds pattern routes and template functions.
    pub fn load(
        site_dir: &Path,
        drafts: bool,
        ext: Option<Arc<dyn SiteExtension>>,
    ) -> Result<Self> {
        let (cfg, mut warnings) = load_config(site_dir)?;
        warnings.extend(templates::lint_site_templates(&site_dir.join("templates")));
        if !ext.as_ref().is_some_and(|e| e.handles_dynamic_routes())
            && let Some(r) = cfg.routes.iter().find(|r| r.is_dynamic())
        {
            bail!(
                "route `{}` needs the site's database (the `content` feature), which this \
                 command did not connect",
                r.path
            );
        }
        let content = Arc::new(Content::load(site_dir, drafts)?);
        let ui = UiProviders::from_config(&cfg.ui)?;
        let tdir = site_dir.join("templates");
        let site_templates = tdir.is_dir().then_some(tdir);
        #[allow(unused_mut)]
        let mut template_dirs: Vec<&'static include_dir::Dir<'static>> =
            ext.as_ref().map(|e| e.template_dirs()).unwrap_or_default();
        #[cfg(feature = "request")]
        template_dirs.push(&templates::BUILTIN_APP);
        let catalogue = Arc::new(
            crate::components::Catalogue::load(&template_dirs, site_templates.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
        let mut env = templates::environment(
            site_templates.clone(),
            content.clone(),
            cfg.site.base_url.clone(),
            template_dirs.clone(),
            catalogue.clone(),
        );
        #[cfg(feature = "request")]
        let fragments = cfg
            .cache
            .fragments
            .then(|| Arc::new(crate::fragments::FragmentCache::new(&cfg.cache)));
        #[cfg(not(feature = "request"))]
        let fragments = None;
        env.add_function("fragment", crate::fragments::fragment_fn(fragments.clone()));
        #[cfg(feature = "request")]
        crate::appui::register(&mut env);
        if let Some(ext) = &ext {
            ext.extend_env(&mut env);
        }
        let globals = Globals::new(&cfg, &ui);
        Ok(Renderer {
            catalogue,
            cfg,
            warnings,
            content,
            env,
            globals,
            ui,
            site_templates,
            template_dirs,
            ext,
            fragments,
        })
    }

    /// Start a whole-site render: extensions may bulk-load their data until
    /// the guard is dropped.
    pub fn bulk(&self) -> Result<BulkGuard<'_>> {
        if let Some(e) = &self.ext {
            e.begin_bulk()?;
        }
        Ok(BulkGuard(self.ext.as_ref()))
    }

    /// The fragment cache of `request` renders (none in builds without
    /// `request`, or with `[cache] fragments = false`).
    pub fn fragments(&self) -> Option<&Arc<crate::fragments::FragmentCache>> {
        self.fragments.as_ref()
    }

    /// Render a template that is not a route (an engine page in the site's
    /// layout), with the site globals plus `ctx`.
    pub fn render_template(&self, name: &str, ctx: &Value) -> Result<(String, Vec<CacheTag>)> {
        let g = &self.globals;
        let (out, tags) = tags::collect(|| {
            self.env.get_template(name).and_then(|t| {
                t.render(merge(
                    context! { site => g.site.clone(), ui => g.ui.clone(), engine => g.engine.clone() },
                    ctx.clone(),
                ))
            })
        });
        Ok((
            crate::components::dedup_assets(out.map_err(template_error)?),
            tags.into_vec(),
        ))
    }

    /// Render one page of a pattern route that the extension did not
    /// resolve by path (e.g. a draft for preview), as a `request` render.
    pub fn render_dynamic(
        &self,
        index: usize,
        page: DynamicPage,
        extra: &Value,
    ) -> Result<(String, Vec<CacheTag>)> {
        let mut route = self.dynamic_route(index, page)?;
        route.mode = RenderMode::Request;
        self.render_with(&route, extra)
    }

    pub fn config(&self) -> &SiteConfig {
        &self.cfg
    }

    /// Config warnings from loading.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Routes from Markdown files and plain `[[routes]]` (no database).
    pub fn static_routes(&self) -> Result<Vec<Route>> {
        let site_templates = self.site_templates.clone();
        let dirs = self.template_dirs.clone();
        routes::build_routes(&self.cfg, &self.content, &|name| {
            templates::resolve_in(site_templates.as_deref(), &dirs, name).is_some()
        })
    }

    /// Every route, with pattern routes expanded by the extension.
    pub fn routes(&self) -> Result<Vec<Route>> {
        let mut all = self.static_routes()?;
        if let Some(ext) = &self.ext {
            for (i, rc) in self.cfg.routes.iter().enumerate() {
                if !rc.is_dynamic() {
                    continue;
                }
                for page in ext.expand(&self.cfg, i)? {
                    all.push(self.dynamic_route(i, page)?);
                }
            }
        }
        routes::check_collisions(&mut all)?;
        Ok(all)
    }

    fn dynamic_route(&self, index: usize, page: DynamicPage) -> Result<Route> {
        let rc = &self.cfg.routes[index];
        let path = routes::normalize_path(&page.path);
        routes::check_route_path(&path)
            .with_context(|| format!("page `{path}` of route `{}`", rc.path))?;
        Ok(Route {
            output: routes::output_file(&path),
            path,
            template: rc.template.clone().unwrap_or_default(),
            mode: self.cfg.effective_mode(rc.mode),
            source: RouteSource::Dynamic {
                route: index,
                page: page.page,
                tags: page.tags,
                listing: page.listing,
            },
            title: rc.title.clone(),
            description: rc.description.clone(),
            listing: None,
        })
    }

    /// Find the route for one URL path without expanding every pattern
    /// route: static routes first, then pattern routes by their parameters.
    pub fn resolve_path(&self, path: &str) -> Result<Option<Route>> {
        let path = routes::normalize_path(path);
        if routes::check_route_path(&path).is_err() {
            return Ok(None);
        }
        if let Some(r) = self.static_routes()?.into_iter().find(|r| r.path == path) {
            return Ok(Some(r));
        }
        let Some(ext) = &self.ext else {
            return Ok(None);
        };
        for (i, rc) in self.cfg.routes.iter().enumerate() {
            if !rc.is_dynamic() {
                continue;
            }
            let pattern = routes::normalize_path(&rc.path);
            let params = if rc.list.is_some() {
                match routes::match_list_page(&pattern, &path) {
                    Some(n) => BTreeMap::from([("page".to_string(), n.to_string())]),
                    None => continue,
                }
            } else {
                match routes::match_pattern(&pattern, &path) {
                    Some(p) => p,
                    None => continue,
                }
            };
            if let Some(page) = ext.resolve(&self.cfg, i, &params)? {
                let route = self.dynamic_route(i, page)?;
                // A page only lives at its canonical path (e.g. an `{id}`
                // route reached through `/articles/007/`).
                if route.path == path {
                    return Ok(Some(route));
                }
            }
        }
        // Wildcard `request` routes (`/account/*`): longest prefix wins.
        let wild = self
            .cfg
            .routes
            .iter()
            .enumerate()
            .filter(|(_, rc)| rc.template.is_some())
            .filter_map(|(i, rc)| Some((i, rc, rc.wildcard_prefix()?)))
            .filter(|(_, _, prefix)| path.starts_with(prefix))
            .max_by_key(|(_, _, prefix)| prefix.len());
        if let Some((i, rc, _)) = wild {
            return Ok(Some(Route {
                output: routes::output_file(&path),
                path,
                template: rc.template.clone().unwrap_or_default(),
                mode: self.cfg.effective_mode(rc.mode),
                source: RouteSource::Explicit(i),
                title: rc.title.clone(),
                description: rc.description.clone(),
                listing: None,
            }));
        }
        Ok(None)
    }

    /// The `[[routes]]` entry a route came from, if any.
    pub fn route_config(&self, route: &Route) -> Option<&crate::config::RouteConfig> {
        match &route.source {
            RouteSource::Explicit(i) | RouteSource::Dynamic { route: i, .. } => {
                self.cfg.routes.get(*i)
            }
            RouteSource::Content(_) => self
                .cfg
                .routes
                .iter()
                .find(|r| routes::normalize_path(&r.path) == route.path),
        }
    }

    /// Render one route. Returns the HTML and the cache tags it recorded.
    pub fn render(&self, route: &Route) -> Result<(String, Vec<CacheTag>)> {
        render_route(
            &self.env,
            &self.globals,
            &self.content,
            &self.cfg,
            route,
            None,
        )
    }

    /// Render one route for a single request (`request` mode), with extra
    /// context values merged in (e.g. `user`, `request`). The result is
    /// never written to disk.
    pub fn render_with(&self, route: &Route, extra: &Value) -> Result<(String, Vec<CacheTag>)> {
        render_route(
            &self.env,
            &self.globals,
            &self.content,
            &self.cfg,
            route,
            Some(extra),
        )
    }

    /// The site's 404 page.
    pub fn render_404(&self) -> Result<String> {
        let g = &self.globals;
        self.env
            .get_template("404.html")
            .and_then(|t| {
                t.render(context! {
                    site => g.site.clone(),
                    ui => g.ui.clone(),
                    engine => g.engine.clone(),
                    page => context! { title => "Page not found", url => "/404.html" },
                })
            })
            .map(crate::components::dedup_assets)
            .map_err(template_error)
    }

    /// Engine-served assets (TagBrush, TagAction, theme CSS, licences, and
    /// the scripts and stylesheets of the compiled components).
    pub fn ui_assets(&self) -> Vec<(String, Vec<u8>)> {
        let mut v = self.ui.assets();
        v.extend(
            self.catalogue
                .assets()
                .iter()
                .map(|a| (a.path.clone(), a.body.to_vec())),
        );
        v
    }

    /// The component catalogue of this build and site.
    pub fn catalogue(&self) -> &crate::components::Catalogue {
        &self.catalogue
    }
}

/// Build a site.
pub fn build_site(site_dir: &Path, opts: &BuildOptions) -> Result<BuildReport> {
    build_site_with(site_dir, opts, None)
}

/// Build a site with an extension (pattern routes from the database).
pub fn build_site_with(
    site_dir: &Path,
    opts: &BuildOptions,
    ext: Option<Arc<dyn SiteExtension>>,
) -> Result<BuildReport> {
    use crate::timings::{phase, time};
    let started = Instant::now();
    let renderer = time("load templates", || {
        Renderer::load(site_dir, opts.drafts, ext)
    })?;
    let cfg = renderer.config().clone();
    let warnings = renderer.warnings().to_vec();
    let output = output_dir(site_dir, &cfg, opts);
    let bulk = time("preload content", || renderer.bulk())?;
    let route_table = time("route table", || renderer.routes())?;

    let staging = staging_dir(&output)?;
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("removing stale {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;

    let result = (|| -> Result<(Vec<RenderedRoute>, usize)> {
        let p = phase("static files");
        let static_files = copy_static(&site_dir.join("static"), &staging, &output)?;
        for (path, body) in renderer.ui_assets() {
            write_published(
                &staging,
                &output,
                Path::new(path.trim_start_matches('/')),
                &body,
            )?;
        }
        drop(p);

        // `request` routes render per request and are never written.
        let todo: Vec<&Route> = route_table
            .iter()
            .filter(|r| r.mode != RenderMode::Request)
            .collect();
        let rendered = time("render and write pages", || {
            render_parallel(&renderer, &todo, &staging, &output, build_jobs(opts))
        })?;

        let _p = phase("404, sitemap, manifest");
        // 404 page (not a route: no sitemap entry, no tags kept).
        let not_found = renderer.render_404()?;
        write_published(
            &staging,
            &output,
            Path::new("404.html"),
            not_found.as_bytes(),
        )?;

        if !cfg.site.base_url.is_empty() {
            write_published(
                &staging,
                &output,
                Path::new("sitemap.xml"),
                sitemap(&cfg.site.base_url, &rendered).as_bytes(),
            )?;
        }

        let manifest = serde_json::to_vec_pretty(&Manifest {
            engine: env!("CARGO_PKG_VERSION"),
            routes: &rendered,
        })?;
        std::fs::write(staging.join(MANIFEST), manifest)?;
        Ok((rendered, static_files))
    })();

    time("release preload", || drop(bulk));
    let (rendered, static_files) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    time("swap into place", || swap_into_place(&staging, &output))?;

    Ok(BuildReport {
        output,
        routes: rendered,
        static_files,
        warnings,
        elapsed: started.elapsed(),
    })
}

/// Render `routes` into `staging` on `jobs` threads and return them in
/// route order (the manifest and sitemap stay deterministic).
///
/// Workers take the next route from a shared counter, so slow pages do not
/// hold up a fixed share. Each render is independent: templates are shared
/// read-only, cache tags are collected per thread, and data-backed template
/// functions block on the runtime handle they hold (never on the worker's
/// own runtime: these are plain threads). The first failure stops the
/// others from taking more work.
///
/// On hosts that boot with `isolcpus`, the kernel never moves threads
/// between isolated CPUs, so every worker would share the CPU the process
/// started on. `ENGINE_BUILD_PIN_CPUS=1` pins worker *n* to the *n*-th CPU
/// the process may use (Linux only); elsewhere it is not needed.
fn render_parallel(
    renderer: &Renderer,
    routes: &[&Route],
    staging: &Path,
    previous: &Path,
    jobs: usize,
) -> Result<Vec<RenderedRoute>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let render_one = |route: &Route| -> Result<RenderedRoute> {
        let (html, tags) = renderer
            .render(route)
            .with_context(|| format!("rendering {}", route.path))?;
        write_published(staging, previous, &route.output, html.as_bytes())?;
        Ok(RenderedRoute {
            path: route.path.clone(),
            output: route.output.clone(),
            template: route.template.clone(),
            mode: route.mode,
            tags: tags.into_iter().map(|t| t.to_string()).collect(),
            sha256: sha256_hex(html.as_bytes()),
        })
    };
    let jobs = jobs.min(routes.len()).max(1);
    if jobs == 1 {
        return routes.iter().map(|r| render_one(r)).collect();
    }
    let pin = std::env::var(PIN_ENV).is_ok_and(|v| v.trim() == "1");
    let cpus = if pin {
        affinity::allowed_cpus()
    } else {
        Vec::new()
    };
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let mut results: Vec<(usize, Result<RenderedRoute>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs)
            .map(|w| {
                let (next, failed, render_one, cpus) = (&next, &failed, &render_one, &cpus);
                std::thread::Builder::new()
                    .name(format!("engine-build-{w}"))
                    .spawn_scoped(scope, move || {
                        if let Some(cpu) = cpus.get(w % cpus.len().max(1)) {
                            affinity::pin_current_thread(*cpu);
                        }
                        let mut out = Vec::new();
                        while !failed.load(Ordering::Relaxed) {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(route) = routes.get(i) else { break };
                            let r = render_one(route);
                            if r.is_err() {
                                failed.store(true, Ordering::Relaxed);
                            }
                            out.push((i, r));
                        }
                        out
                    })
                    .expect("spawning a build thread")
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|h| h.join().expect("a build thread panicked"))
            .collect()
    });
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// CPU pinning for build threads (see [`render_parallel`]).
#[cfg(target_os = "linux")]
mod affinity {
    /// The CPUs this process may run on, in order.
    pub fn allowed_cpus() -> Vec<usize> {
        // SAFETY: `set` is a plain bitmask owned by this frame; the kernel
        // writes at most `size_of::<cpu_set_t>()` bytes into it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
                return Vec::new();
            }
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&c| libc::CPU_ISSET(c, &set))
                .collect()
        }
    }

    /// Pin the calling thread to `cpu` (best effort).
    pub fn pin_current_thread(cpu: usize) {
        // SAFETY: as above; pid 0 is the calling thread.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut set);
            let _ = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod affinity {
    pub fn allowed_cpus() -> Vec<usize> {
        Vec::new()
    }
    pub fn pin_current_thread(_: usize) {}
}

struct Globals {
    site: Value,
    ui: Value,
    engine: Value,
}

impl Globals {
    fn new(cfg: &SiteConfig, ui: &UiProviders) -> Self {
        Globals {
            site: context! {
                name => cfg.site.name,
                base_url => cfg.site.base_url,
                description => cfg.site.description,
                author => cfg.site.author,
                language => cfg.site.language,
                nav => cfg.site.nav,
                extra => cfg.site.extra,
            },
            ui: context! {
                css => ui.css.href(),
                js => ui.behaviour.href(),
                theme => ui.theme_css.as_ref().map(|_| crate::ui::THEME_CSS_PATH),
            },
            engine: context! {
                version => env!("CARGO_PKG_VERSION"),
                preset => crate::features::PRESET,
            },
        }
    }
}

fn template_error(e: minijinja::Error) -> anyhow::Error {
    // minijinja's alternate format includes the template name, line and a
    // source excerpt.
    anyhow::anyhow!("{e:#}")
}

fn render_route(
    env: &minijinja::Environment<'static>,
    g: &Globals,
    content: &Content,
    cfg: &SiteConfig,
    route: &Route,
    extra: Option<&Value>,
) -> Result<(String, Vec<CacheTag>)> {
    let (result, tags) = tags::collect(|| -> Result<String> {
        // Every page reads site settings and navigation.
        tags::record(CacheTag::singleton("site"));
        let home_crumb = || {
            content
                .section_index("")
                .map(|h| vec![context! { title => h.title, url => h.url }])
                .unwrap_or_default()
        };

        let (page, breadcrumbs) = match &route.source {
            RouteSource::Content(i) => {
                let p = &content.pages[*i];
                tags::record(p.tag());
                let mut v = page_value(p);
                if let Some(t) = &route.title {
                    v = merge(v, context! { title => t });
                }
                if let Some(d) = &route.description {
                    v = merge(v, context! { description => d });
                }
                let crumbs: Vec<Value> = content
                    .ancestors(p)
                    .into_iter()
                    .map(|a| context! { title => a.title, url => a.url })
                    .collect();
                (v, crumbs)
            }
            RouteSource::Explicit(_) => (
                context! {
                    title => route.title.clone().unwrap_or_default(),
                    description => route.description,
                    url => route.path,
                },
                home_crumb(),
            ),
            RouteSource::Dynamic {
                route: i,
                page,
                tags,
                ..
            } => {
                for t in tags {
                    tags::record(t.clone());
                }
                let mut v = page.clone();
                v = merge(v, context! { url => route.path });
                if let Some(t) = &route.title {
                    v = merge(v, context! { title => t });
                }
                // Breadcrumbs: home, then the route's own listing page if
                // the site has one (`/articles/` for `/articles/{slug}/`).
                let mut crumbs = home_crumb();
                let parent =
                    routes::normalize_path(cfg.routes[*i].path.split('{').next().unwrap_or("/"));
                if parent != "/"
                    && let Some(r) = cfg
                        .routes
                        .iter()
                        .find(|r| !r.is_pattern() && routes::normalize_path(&r.path) == parent)
                {
                    crumbs.push(context! {
                        title => r.title.clone().unwrap_or_else(|| parent.clone()),
                        url => parent,
                    });
                } else if let Some(p) = content.by_url(&parent).filter(|_| parent != "/") {
                    crumbs.push(context! { title => p.title, url => p.url });
                }
                (v, crumbs)
            }
        };
        let listing = route.listing.as_ref().map(|l| {
            tags::record(CacheTag::collection(
                l.section
                    .split('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("page"),
            ));
            let all = content.section_pages(&l.section);
            let base = content
                .section_index(&l.section)
                .map(|s| s.url.clone())
                .unwrap_or_else(|| "/".into());
            let items: Vec<Value> = if l.per_page == 0 {
                all.iter().map(|p| page_value(p)).collect()
            } else {
                all.iter()
                    .skip((l.number - 1) * l.per_page)
                    .take(l.per_page)
                    .map(|p| page_value(p))
                    .collect()
            };
            let urls: Vec<Value> = (1..=l.total)
                .map(|n| {
                    context! {
                        number => n,
                        url => routes::page_url(&base, n),
                        current => n == l.number,
                    }
                })
                .collect();
            context! {
                pages => items,
                number => l.number,
                total => l.total,
                prev_url => (l.number > 1).then(|| routes::page_url(&base, l.number - 1)),
                next_url => (l.number < l.total).then(|| routes::page_url(&base, l.number + 1)),
                urls => urls,
            }
        });

        let listing = listing.or_else(|| match &route.source {
            RouteSource::Dynamic { listing, .. } => listing.clone(),
            _ => None,
        });
        let ctx = context! {
            site => g.site.clone(),
            ui => g.ui.clone(),
            engine => g.engine.clone(),
            page => page,
            breadcrumbs => breadcrumbs,
            listing => listing,
            route => context! {
                path => route.path,
                mode => route.mode.as_str(),
                template => route.template,
            },
        };
        let ctx = match extra {
            Some(extra) => merge(ctx, extra.clone()),
            None => ctx,
        };
        env.get_template(&route.template)
            .and_then(|t| t.render(ctx))
            .map_err(template_error)
            .with_context(|| format!("rendering {} with {}", route.path, route.template))
    });
    Ok((crate::components::dedup_assets(result?), tags.into_vec()))
}

fn merge(base: Value, extra: Value) -> Value {
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    for v in [base, extra] {
        if let Ok(iter) = v.try_iter() {
            for k in iter {
                if let Ok(val) = v.get_item(&k) {
                    m.insert(k.to_string(), val);
                }
            }
        }
    }
    Value::from(m)
}

fn sitemap(base_url: &str, routes: &[RenderedRoute]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for r in routes.iter().filter(|r| r.path.ends_with('/')) {
        // Paths are restricted to URL-safe characters by routes::check_route_path
        // and content::slug_segment, so no XML escaping is needed beyond `&`.
        s.push_str(&format!(
            "  <url><loc>{}{}</loc></url>\n",
            base_url,
            r.path.replace('&', "&amp;")
        ));
    }
    s.push_str("</urlset>\n");
    s
}

/// SHA-256 of `body`, in lower-case hex.
pub fn sha256_hex(body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(body))
}

fn staging_dir(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("output folder {} has no name", output.display()))?;
    Ok(output.with_file_name(format!(".{name}.staging")))
}

/// Write a published file under `staging` with its pre-compressed copies
/// ([`crate::precompress`]): copies first, then the file. When the previous
/// build in `previous` holds exactly these bytes with fresh copies, the
/// copies are hard-linked from it instead of compressed again (files are
/// only ever replaced by rename, never changed in place, so the shared
/// inode cannot change under the new build).
fn write_published(staging: &Path, previous: &Path, rel: &Path, body: &[u8]) -> Result<()> {
    use crate::precompress::{self, Encoding};
    if !build_precompresses() {
        return write_new(staging, rel, body);
    }
    let reused = (|| {
        if !precompress::compressible(rel) || body.len() < precompress::MIN_BYTES {
            return false;
        }
        let old = previous.join(rel);
        let Ok(meta) = std::fs::metadata(&old) else {
            return false;
        };
        if meta.len() != body.len() as u64 || std::fs::read(&old).ok().as_deref() != Some(body) {
            return false;
        }
        let fresh: Vec<Encoding> = Encoding::ALL
            .into_iter()
            .filter(|e| precompress::fresh(&meta, &e.sidecar(&old)).is_some())
            .collect();
        if fresh.is_empty() {
            return false;
        }
        let dest = staging.join(rel);
        if let Some(parent) = dest.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return false;
        }
        fresh.iter().all(|e| {
            std::fs::hard_link(e.sidecar(&old), e.sidecar(&dest)).is_ok()
                || std::fs::copy(e.sidecar(&old), e.sidecar(&dest)).is_ok()
        })
    })();
    if reused {
        return write_new(staging, rel, body);
    }
    for (e, copy) in precompress::encode_all(rel, body) {
        if let Some(c) = copy {
            write_new(staging, &e.sidecar(rel), &c)?;
        }
    }
    write_new(staging, rel, body)
}

/// Write a file under `root`, refusing to overwrite (catches collisions
/// between `static/` files, engine assets and rendered pages).
fn write_new(root: &Path, rel: &Path, body: &[u8]) -> Result<()> {
    let path = root.join(rel);
    if path.exists() {
        bail!(
            "{} is produced twice (a file in static/ collides with a page or engine asset?)",
            rel.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

/// Copy `static/` into the output. Symlinks and dotfiles are skipped.
fn copy_static(src: &Path, dest: &Path, previous: &Path) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut n = 0;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            let rel = entry.path().strip_prefix(src)?.to_path_buf();
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                if rel.starts_with("_engine") {
                    bail!("static/_engine/ is reserved for files the engine serves");
                }
                if crate::precompress::is_sidecar(&rel) {
                    bail!(
                        "static/{}: names ending in .br or .gz after a text file's extension \
                         are the engine's pre-compressed copies; rename the file",
                        rel.display()
                    );
                }
                let body = std::fs::read(entry.path())?;
                write_published(dest, previous, &rel, &body)?;
                n += 1;
            }
        }
    }
    Ok(n)
}

fn swap_into_place(staging: &Path, output: &Path) -> Result<()> {
    if output.exists() {
        let ours = output.join(MANIFEST).is_file();
        let empty = output.is_dir() && std::fs::read_dir(output)?.next().is_none();
        if !(ours || empty) {
            let _ = std::fs::remove_dir_all(staging);
            bail!(
                "refusing to replace {}: it exists and was not created by `engine build` \
                 (no {MANIFEST}); remove it or choose another output folder",
                output.display()
            );
        }
        std::fs::remove_dir_all(output)
            .with_context(|| format!("removing old {}", output.display()))?;
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(staging, output)
        .with_context(|| format!("moving build into {}", output.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn builds_a_minimal_site_with_builtin_templates() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "site.toml",
            "[site]\nname='Mini'\nbase_url='https://mini.example'\n",
        );
        write(
            d.path(),
            "content/_index.md",
            "# Hello\n\nWelcome <here>.\n",
        );
        write(
            d.path(),
            "content/about.md",
            "+++\ntitle='About'\n+++\nAbout us.\n",
        );
        write(d.path(), "static/robots.txt", "User-agent: *\n");
        let report = build_site(d.path(), &BuildOptions::default()).unwrap();
        let out = d.path().join("public");
        assert_eq!(report.output, out);
        let home = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(home.contains("<html lang=\"en\">"), "{home}");
        assert!(home.contains("/_engine/tagbrush-2.0.1.min.css"));
        assert!(home.contains("/_engine/tagaction-0.2.0.min.js"));
        assert!(out.join("about/index.html").is_file());
        assert!(out.join("404.html").is_file());
        assert!(out.join("robots.txt").is_file());
        assert!(out.join("_engine/tagbrush-2.0.1.min.css").is_file());
        assert!(out.join(MANIFEST).is_file());
        let sitemap = std::fs::read_to_string(out.join("sitemap.xml")).unwrap();
        assert!(sitemap.contains("https://mini.example/about/"));
        let about = report.routes.iter().find(|r| r.path == "/about/").unwrap();
        assert_eq!(about.tags, ["i:page:about", "s:site"]);

        // Every text file big enough has its copies; small ones do not.
        for f in ["index.html", "_engine/tagbrush-2.0.1.min.css"] {
            for e in crate::precompress::Encoding::ALL {
                let side = e.sidecar(&out.join(f));
                let meta = std::fs::metadata(out.join(f)).unwrap();
                assert!(
                    crate::precompress::fresh(&meta, &side).is_some(),
                    "{} (fresh)",
                    side.display()
                );
            }
        }
        assert!(!out.join("robots.txt.br").exists(), "too small to gain");

        // Rebuild replaces our own output; unchanged files keep their
        // copies (linked from the previous build, not compressed again).
        #[cfg(unix)]
        let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap());
        #[cfg(unix)]
        let before = ino(&out.join("index.html.br"));
        build_site(d.path(), &BuildOptions::default()).unwrap();
        #[cfg(unix)]
        assert_eq!(ino(&out.join("index.html.br")), before);
        // A changed page gets new copies.
        write(
            d.path(),
            "content/_index.md",
            &format!(
                "+++\ntitle='Home'\n+++\n{}\n",
                "Changed home page. ".repeat(60)
            ),
        );
        build_site(d.path(), &BuildOptions::default()).unwrap();
        let br = std::fs::read(out.join("index.html.br")).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut brotli::Decompressor::new(&br[..], 4096), &mut text)
            .unwrap();
        assert_eq!(
            text,
            std::fs::read_to_string(out.join("index.html")).unwrap()
        );
        assert!(text.contains("Changed home page."));
    }

    #[test]
    fn refuses_to_replace_foreign_output() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "site.toml", "[site]\nname='x'\n");
        write(d.path(), "public/important.txt", "keep me");
        let err = build_site(d.path(), &BuildOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing"), "{err}");
        assert!(d.path().join("public/important.txt").is_file());
        assert!(!d.path().join(".public.staging").exists());
    }

    #[test]
    fn failed_build_keeps_previous_output() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "site.toml", "[site]\nname='x'\n");
        write(d.path(), "content/a.md", "A");
        build_site(d.path(), &BuildOptions::default()).unwrap();
        write(
            d.path(),
            "content/b.md",
            "+++\ntemplate='missing.html'\n+++\n",
        );
        let err = format!(
            "{:#}",
            build_site(d.path(), &BuildOptions::default()).unwrap_err()
        );
        assert!(err.contains("missing.html"), "{err}");
        assert!(d.path().join("public/a/index.html").is_file());
    }

    #[test]
    fn static_collisions_are_errors() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "site.toml", "[site]\nname='x'\n");
        write(d.path(), "content/a.md", "A");
        write(d.path(), "static/a/index.html", "clash");
        assert!(build_site(d.path(), &BuildOptions::default()).is_err());
    }
}
