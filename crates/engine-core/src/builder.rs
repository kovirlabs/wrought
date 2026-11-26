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
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderedRoute {
    pub path: String,
    pub output: PathBuf,
    pub template: String,
    pub mode: RenderMode,
    /// Cache tags the render recorded.
    pub tags: Vec<String>,
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

/// Build a site.
pub fn build_site(site_dir: &Path, opts: &BuildOptions) -> Result<BuildReport> {
    let started = Instant::now();
    let (cfg, warnings) = load_config(site_dir)?;
    let output = output_dir(site_dir, &cfg, opts);
    let content = Arc::new(Content::load(site_dir, opts.drafts)?);
    let ui = UiProviders::from_config(&cfg.ui)?;

    let tdir = site_dir.join("templates");
    let site_templates = tdir.is_dir().then_some(tdir);
    let env = templates::environment(
        site_templates.clone(),
        content.clone(),
        cfg.site.base_url.clone(),
    );
    let route_table = routes::build_routes(&cfg, &content, &|name| {
        templates::resolve(site_templates.as_deref(), name).is_some()
    })?;

    let staging = staging_dir(&output)?;
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("removing stale {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;

    let result = (|| -> Result<(Vec<RenderedRoute>, usize)> {
        let static_files = copy_static(&site_dir.join("static"), &staging)?;
        for (path, body) in ui.assets() {
            write_new(&staging, Path::new(path.trim_start_matches('/')), &body)?;
        }

        let globals = Globals::new(&cfg, &ui);
        let mut rendered = Vec::new();
        for route in &route_table {
            let (html, tags) = render_route(&env, &globals, &content, route)?;
            write_new(&staging, &route.output, html.as_bytes())?;
            rendered.push(RenderedRoute {
                path: route.path.clone(),
                output: route.output.clone(),
                template: route.template.clone(),
                mode: route.mode,
                tags: tags.into_iter().map(|t| t.to_string()).collect(),
            });
        }

        // 404 page (not a route: no sitemap entry, no tags kept).
        let not_found = env
            .get_template("404.html")
            .and_then(|t| {
                t.render(context! {
                    site => globals.site.clone(),
                    ui => globals.ui.clone(),
                    engine => globals.engine.clone(),
                    page => context! { title => "Page not found", url => "/404.html" },
                })
            })
            .map_err(template_error)?;
        write_new(&staging, Path::new("404.html"), not_found.as_bytes())?;

        if !cfg.site.base_url.is_empty() {
            write_new(
                &staging,
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

    let (rendered, static_files) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    swap_into_place(&staging, &output)?;

    Ok(BuildReport {
        output,
        routes: rendered,
        static_files,
        warnings,
        elapsed: started.elapsed(),
    })
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
    route: &Route,
) -> Result<(String, Vec<CacheTag>)> {
    let (result, tags) = tags::collect(|| -> Result<String> {
        // Every page reads site settings and navigation.
        tags::record(CacheTag::singleton("site"));

        let (page, breadcrumbs) = match route.source {
            RouteSource::Content(i) => {
                let p = &content.pages[i];
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
                content
                    .section_index("")
                    .map(|h| vec![context! { title => h.title, url => h.url }])
                    .unwrap_or_default(),
            ),
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
        env.get_template(&route.template)
            .and_then(|t| t.render(ctx))
            .map_err(template_error)
            .with_context(|| format!("rendering {} with {}", route.path, route.template))
    });
    Ok((result?, tags.into_vec()))
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

fn staging_dir(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("output folder {} has no name", output.display()))?;
    Ok(output.with_file_name(format!(".{name}.staging")))
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
fn copy_static(src: &Path, dest: &Path) -> Result<usize> {
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
                let body = std::fs::read(entry.path())?;
                write_new(dest, &rel, &body)?;
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

        // Rebuild replaces our own output.
        build_site(d.path(), &BuildOptions::default()).unwrap();
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
