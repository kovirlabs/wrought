//! `engine routes`: every URL the site answers, with where it comes from.
//!
//! Read from the same places the engine assembles them: the site's pages
//! from `site.toml` `[[routes]]` and `content/` (as `engine build` makes its
//! route table), the cache tags each page recorded from the last build's
//! manifest (`.engine-manifest.json`, written by `engine build`), the site
//! crate's handlers from its registrations ([`crate::App::route`]), form
//! endpoints from `forms/*.toml`, and the engine's own endpoints for the
//! features of this build. Nothing is rendered and no database is needed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use engine_core::builder::{self, BuildOptions};
use engine_core::config::{RenderMode, RouteConfig, SiteConfig};
use engine_core::content::{Content, PageKind};
use engine_core::features;
use engine_core::routes::{match_pattern, normalize_path};
use serde::Serialize;

/// One line of `engine routes`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct Entry {
    /// `page`, `pattern` (one page per item or term), `list` (a paginated
    /// list), `wildcard`, `handler` (registered by the site crate), `form`
    /// or `engine`.
    pub kind: &'static str,
    /// `GET`, `POST`...; `*` for any method.
    pub methods: Vec<&'static str>,
    pub path: String,
    /// The render mode of a site page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Where it is defined: `content/about.md`, `site.toml [[routes]] #2`,
    /// `forms/contact.toml`, `app (src)`, `engine (auth)`.
    pub source: String,
    /// The cache tags its pages recorded (last build), or those a pattern
    /// route's pages carry (`i:<type>:*`).
    pub tags: Vec<String>,
    /// Anything else worth knowing (a feature this build lacks, a
    /// condition).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Listing {
    pub routes: Vec<Entry>,
    pub notes: Vec<String>,
}

/// `engine routes [--json]`.
pub(crate) fn run(site: &Path, json: bool) -> Result<()> {
    let l = list(site)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&l)?);
    } else {
        print!("{}", table(&l));
    }
    Ok(())
}

pub(crate) fn list(site: &Path) -> Result<Listing> {
    let (cfg, _) = builder::load_config(site)?;
    let mut notes = Vec::new();
    let recorded = recorded_tags(site, &cfg, &mut notes);
    let mut routes = site_pages(site, &cfg, &recorded)?;
    routes.extend(app_handlers(site, &mut notes));
    routes.extend(forms(site)?);
    routes.extend(engine_endpoints());
    Ok(Listing { routes, notes })
}

/// Path to tags, from the last build's manifest.
fn recorded_tags(
    site: &Path,
    cfg: &SiteConfig,
    notes: &mut Vec<String>,
) -> BTreeMap<String, Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        routes: Vec<Built>,
    }
    #[derive(serde::Deserialize)]
    struct Built {
        path: String,
        #[serde(default)]
        tags: Vec<String>,
    }
    let file = builder::output_dir(site, cfg, &BuildOptions::default()).join(builder::MANIFEST);
    let parsed = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
    match parsed {
        Some(m) => m.routes.into_iter().map(|r| (r.path, r.tags)).collect(),
        None => {
            notes.push(
                "No build output yet: run `engine build` to see the cache tags each page records."
                    .into(),
            );
            BTreeMap::new()
        }
    }
}

fn template_exists(site: &Path, name: &str) -> bool {
    engine_core::templates::resolve_in(Some(&site.join("templates")), &[], name).is_some()
}

/// A mode, with a note when this build cannot render it.
fn mode_note(mode: RenderMode) -> Option<String> {
    let f = mode.required_feature()?;
    (!features::has_feature(f)).then(|| format!("`{}` mode needs the `{f}` feature", mode.as_str()))
}

/// Files and request pages alike answer `GET` (and `HEAD`).
fn page_methods() -> Vec<&'static str> {
    vec!["GET"]
}

/// The site's pages: content files and `[[routes]]`, as the build's route
/// table has them (pattern and list routes as their pattern).
fn site_pages(
    site: &Path,
    cfg: &SiteConfig,
    recorded: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<Entry>> {
    let content = Content::load(site, false)?;
    let explicit: BTreeMap<String, (usize, &RouteConfig)> = cfg
        .routes
        .iter()
        .enumerate()
        .map(|(i, r)| (normalize_path(&r.path), (i, r)))
        .collect();
    let mut out = Vec::new();
    for page in &content.pages {
        let over = explicit.get(&page.url).map(|(_, r)| *r);
        let mode = cfg.effective_mode(over.and_then(|r| r.mode).or(page.mode));
        let default_template = match page.kind {
            PageKind::Section if page.url == "/" && template_exists(site, "home.html") => {
                "home.html".to_string()
            }
            PageKind::Section => "section.html".to_string(),
            PageKind::Page => content
                .section_index(&page.section)
                .and_then(|s| s.front.page_template.clone())
                .unwrap_or_else(|| "page.html".to_string()),
        };
        let template = over
            .and_then(|r| r.template.clone())
            .or_else(|| page.template.clone())
            .unwrap_or(default_template);
        let mut source = page.source.display().to_string();
        if let Some((i, _)) = explicit.get(&page.url) {
            source.push_str(&format!(" + site.toml [[routes]] #{}", i + 1));
        }
        let mut tags = recorded.get(&page.url).cloned().unwrap_or_default();
        if page.kind == PageKind::Section {
            // Listing pages after the first: /blog/page/2/...
            let prefix = format!("{}page/", page.url);
            let extra: Vec<&String> = recorded.keys().filter(|p| p.starts_with(&prefix)).collect();
            if !extra.is_empty() {
                tags = merge(
                    tags,
                    extra
                        .iter()
                        .flat_map(|p| recorded[p.as_str()].iter().cloned()),
                );
            }
        }
        out.push(Entry {
            kind: "page",
            methods: page_methods(),
            path: page.url.clone(),
            mode: Some(mode.as_str()),
            template: Some(template),
            source,
            tags,
            note: mode_note(mode),
        });
    }
    for (path, (i, r)) in &explicit {
        if content.by_url(path).is_some() {
            continue;
        }
        let mode = cfg.effective_mode(r.mode);
        let source = format!("site.toml [[routes]] #{}", i + 1);
        let (kind, tags, mut note) = if r.is_pattern() {
            // Every concrete page the last build wrote for this pattern.
            let pattern = normalize_path(&r.path);
            let built = recorded
                .iter()
                .filter(|(p, _)| match_pattern(&pattern, p).is_some())
                .flat_map(|(_, t)| t.iter().map(|t| generalise(t)));
            let declared = match (&r.content_type, &r.taxonomy) {
                (Some(t), _) => vec![format!("i:{t}:*")],
                (None, Some(v)) => vec![format!("c:{v}")],
                (None, None) => Vec::new(),
            };
            let what = match (&r.content_type, &r.taxonomy) {
                (Some(t), _) => format!("one page per published `{t}`"),
                (None, Some(v)) => format!("one page per `{v}` term"),
                (None, None) => String::new(),
            };
            ("pattern", merge(declared, built), Some(what))
        } else if let Some(list) = &r.list {
            let prefix = format!("{}page/", normalize_path(&r.path));
            let built = recorded
                .iter()
                .filter(|(p, _)| **p == normalize_path(&r.path) || p.starts_with(&prefix))
                .flat_map(|(_, t)| t.iter().cloned());
            (
                "list",
                merge(vec![format!("c:{}", list.content_type)], built),
                Some(format!("paginated list of `{}`", list.content_type)),
            )
        } else if r.is_wildcard() {
            ("wildcard", Vec::new(), None)
        } else {
            (
                "page",
                recorded.get(path).cloned().unwrap_or_default(),
                None,
            )
        };
        if r.requires_auth() {
            note = Some(join_note(note, "signed-in visitors only".into()));
        }
        if let Some(m) = mode_note(mode) {
            note = Some(join_note(note, m));
        }
        if r.is_dynamic() && !features::has_feature("content") {
            note = Some(join_note(note, "needs the `content` feature".into()));
        }
        out.push(Entry {
            kind,
            methods: page_methods(),
            path: if r.is_dynamic() || r.is_wildcard() {
                r.path.clone()
            } else {
                path.clone()
            },
            mode: Some(mode.as_str()),
            template: r.template.clone(),
            source,
            tags,
            note,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn join_note(a: Option<String>, b: String) -> String {
    match a {
        Some(a) if !a.is_empty() => format!("{a}; {b}"),
        _ => b,
    }
}

/// `i:note:3` as `i:note:*` (the pages of a pattern route).
fn generalise(tag: &str) -> String {
    match tag.strip_prefix("i:").and_then(|r| r.split_once(':')) {
        Some((kind, _)) => format!("i:{kind}:*"),
        None => tag.to_string(),
    }
}

/// Both lists' tags, sorted, each once.
fn merge(a: Vec<String>, b: impl Iterator<Item = String>) -> Vec<String> {
    a.into_iter()
        .chain(b)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The site crate's handlers ([`crate::App::route`]).
fn app_handlers(site: &Path, notes: &mut Vec<String>) -> Vec<Entry> {
    #[cfg(any(
        feature = "content",
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media",
        feature = "realtime"
    ))]
    {
        let routes = crate::app::routes();
        if !routes.is_empty() {
            return routes
                .into_iter()
                .map(|(path, methods)| Entry {
                    kind: "handler",
                    methods,
                    path,
                    mode: None,
                    template: None,
                    source: "app".into(),
                    tags: Vec::new(),
                    note: None,
                })
                .collect();
        }
    }
    // A site crate run through the stock binary: its handlers are only
    // known to its own.
    let stock = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_stem().map(|s| s == "engine"))
        .unwrap_or(false);
    if stock && crate::generate::is_app_crate(site) {
        notes.push(
            "This site is an app crate: its handlers are listed by its own binary \
             (`cargo run -- routes`)."
                .into(),
        );
    }
    Vec::new()
}

/// `POST /_engine/forms/<name>/` for each form file (and the built-in
/// forms, with the `forms` feature).
fn forms(site: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    #[cfg(feature = "forms")]
    {
        for name in engine_forms::load_files(site)?.keys() {
            let file = format!("forms/{name}.toml");
            let builtin = !site.join(&file).is_file();
            out.push(Entry {
                kind: "form",
                methods: vec!["POST"],
                path: format!("/_engine/forms/{name}/"),
                mode: None,
                template: None,
                source: if builtin {
                    "engine (built-in form)".into()
                } else {
                    file
                },
                tags: Vec::new(),
                note: None,
            });
        }
        out.push(Entry {
            kind: "form",
            methods: vec!["POST"],
            path: "/_engine/forms/{name}/".into(),
            mode: None,
            template: None,
            source: "engine (forms)".into(),
            tags: Vec::new(),
            note: Some("forms made in the admin".into()),
        });
    }
    #[cfg(not(feature = "forms"))]
    {
        let dir = site.join("forms");
        if dir.is_dir() {
            let mut names = Vec::new();
            for e in anyhow::Context::with_context(std::fs::read_dir(&dir), || {
                format!("reading {}", dir.display())
            })? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "toml")
                    && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                {
                    names.push(stem.to_string());
                }
            }
            names.sort();
            for name in names {
                out.push(Entry {
                    kind: "form",
                    methods: vec!["POST"],
                    path: format!("/_engine/forms/{name}/"),
                    mode: None,
                    template: None,
                    source: format!("forms/{name}.toml"),
                    tags: Vec::new(),
                    note: Some("needs the `forms` feature".into()),
                });
            }
        }
    }
    Ok(out)
}

/// The engine's own endpoints in this build: (feature, methods, path,
/// note). Kept next to the routers that mount them (engine-auth's,
/// engine-admin's... `web::router`); the admin is one line.
const ENGINE: &[(&str, &[&str], &str, &str)] = &[
    ("db-postgres", &["GET"], "/_engine/health", "`serve`"),
    (
        "realtime",
        &["GET"],
        "/_engine/live",
        "live-region event streams, public tags",
    ),
    ("auth", &["GET", "POST"], "/account/login/", ""),
    ("auth", &["GET", "POST"], "/account/totp/", ""),
    ("auth", &["POST"], "/account/logout/", ""),
    ("auth", &["GET", "POST"], "/account/register/", ""),
    ("auth", &["GET"], "/account/verify/", ""),
    ("auth", &["GET", "POST"], "/account/reset/", ""),
    ("auth", &["GET", "POST"], "/account/reset/confirm/", ""),
    ("auth", &["GET", "POST"], "/account/profile/", ""),
    (
        "auth",
        &["GET"],
        "/_engine/ui/{file}",
        "account pages' assets",
    ),
    (
        "oidc",
        &["POST"],
        "/account/oidc/{name}/start/",
        "with [[auth.oidc]] providers",
    ),
    (
        "oidc",
        &["POST"],
        "/account/oidc/{name}/link/",
        "with [[auth.oidc]] providers",
    ),
    (
        "oidc",
        &["GET"],
        "/account/oidc/{name}/callback/",
        "with [[auth.oidc]] providers",
    ),
    (
        "oidc",
        &["POST"],
        "/account/oidc/unlink/",
        "with [[auth.oidc]] providers",
    ),
    (
        "admin",
        &["GET", "POST", "PUT"],
        "/admin/",
        "everything under /admin/, for staff",
    ),
    ("api", &["GET"], "/api/v1/", ""),
    ("api", &["GET", "POST"], "/api/v1/content/{ty}/", ""),
    (
        "api",
        &["GET", "PATCH", "DELETE"],
        "/api/v1/content/{ty}/{key}",
        "",
    ),
    ("media", &["GET"], "/media/{id}/{file}", ""),
    ("payments", &["POST"], "/_engine/shop/cart/", "with [shop]"),
    (
        "payments",
        &["POST"],
        "/_engine/shop/checkout/",
        "with [shop]",
    ),
    (
        "payments",
        &["POST"],
        "/_engine/payments/webhook/{provider}/",
        "with [shop]",
    ),
    (
        "static",
        &["GET"],
        "/_engine/reload",
        "`engine dev` only: live reload",
    ),
];

fn engine_endpoints() -> Vec<Entry> {
    ENGINE
        .iter()
        .filter(|(f, ..)| *f == "static" || features::has_feature(f))
        .map(|(f, methods, path, note)| Entry {
            kind: "engine",
            methods: methods.to_vec(),
            path: (*path).to_string(),
            mode: None,
            template: None,
            source: if *f == "static" {
                "engine".into()
            } else {
                format!("engine ({f})")
            },
            tags: Vec::new(),
            note: (!note.is_empty()).then(|| (*note).to_string()),
        })
        .collect()
}

/// The listing as aligned columns.
fn table(l: &Listing) -> String {
    let head = [
        "METHOD", "PATH", "KIND", "MODE", "TEMPLATE", "SOURCE", "TAGS",
    ];
    let rows: Vec<[String; 7]> = l
        .routes
        .iter()
        .map(|e| {
            let mut tags = e.tags.join(" ");
            if let Some(n) = &e.note {
                if !tags.is_empty() {
                    tags.push(' ');
                }
                tags.push_str(&format!("({n})"));
            }
            [
                e.methods.join(","),
                e.path.clone(),
                e.kind.to_string(),
                e.mode.unwrap_or("-").to_string(),
                e.template.clone().unwrap_or_else(|| "-".into()),
                e.source.clone(),
                tags,
            ]
        })
        .collect();
    let mut width = head.map(str::len);
    for r in &rows {
        for (w, c) in width.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let line = |cells: &[&str]| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(&format!("{c:<w$}  ", w = width[i]));
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut out = line(&head);
    for r in &rows {
        out.push_str(&line(&r.iter().map(String::as_str).collect::<Vec<_>>()));
    }
    if !l.notes.is_empty() {
        out.push('\n');
    }
    for n in &l.notes {
        out.push_str(&format!("note: {n}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_tags_are_generalised() {
        assert_eq!(generalise("i:note:3"), "i:note:*");
        assert_eq!(generalise("c:note"), "c:note");
        assert_eq!(
            merge(
                vec!["i:note:*".into()],
                ["i:note:*".to_string(), "c:note".into()].into_iter()
            ),
            ["c:note", "i:note:*"]
        );
    }

    #[test]
    fn engine_endpoints_are_under_reserved_paths() {
        for (f, _, path, _) in ENGINE {
            let engine_owned = crate::app::reserved_prefix(path).is_some();
            assert!(engine_owned, "{path} ({f}) is not under an engine prefix");
        }
    }
}
