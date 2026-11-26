//! The route table: every URL a site has, with its template and render mode.
//!
//! Routes come from content files (see [`crate::content`]), section
//! pagination (`/blog/page/2/`) and explicit `[[routes]]` in `site.toml`.
//! An explicit route whose path matches a content page overrides that page's
//! template, mode and title; otherwise it is a template-only page.
//!
//! Pattern routes (`[[routes]]` with `type` or `taxonomy`, e.g.
//! `/articles/{slug}/`) are not expanded here: the `content` feature expands
//! them into [`RouteSource::Dynamic`] routes through a
//! [`crate::builder::SiteExtension`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::{RenderMode, SiteConfig};
use crate::content::{Content, PageKind};
use crate::features;
use crate::tags::CacheTag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteSource {
    /// Index into `Content::pages`.
    Content(usize),
    /// Index into `SiteConfig::routes`.
    Explicit(usize),
    /// One page of a pattern route (index into `SiteConfig::routes`), with the
    /// `page` value templates see and the tags of the data it was made from.
    Dynamic {
        route: usize,
        page: minijinja::Value,
        tags: Vec<CacheTag>,
        /// What templates see as `listing` (paginated database lists).
        listing: Option<minijinja::Value>,
    },
}

/// Listing information for a section page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// Folder of the section (`blog`).
    pub section: String,
    /// 1-based page number.
    pub number: usize,
    /// Total listing pages.
    pub total: usize,
    /// Items per page (0 = all on one page).
    pub per_page: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Root-relative URL: `/`, `/about/`, `/feed.xml`.
    pub path: String,
    /// File under the output folder: `index.html`, `about/index.html`.
    pub output: PathBuf,
    pub template: String,
    pub mode: RenderMode,
    pub source: RouteSource,
    pub title: Option<String>,
    pub description: Option<String>,
    pub listing: Option<Listing>,
}

/// Normalise a route path: directory-style paths end in `/`; a last segment
/// with an extension (`/feed.xml`) is a file.
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let last = trimmed.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    }
}

/// Reject paths that could escape the output folder or confuse servers.
pub fn check_route_path(path: &str) -> Result<()> {
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." || seg.starts_with('.') {
            bail!("path segment `{seg}` is not allowed");
        }
        if !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '*'))
        {
            bail!("path segment `{seg}` may only use letters, digits, `-`, `_`, `.` and `~`");
        }
    }
    Ok(())
}

/// The `{name}` parameter of a pattern segment, if it is one.
fn param_name(seg: &str) -> Option<&str> {
    seg.strip_prefix('{')?.strip_suffix('}')
}

/// Check a pattern route path: concrete segments as in [`check_route_path`]
/// plus exactly one whole-segment parameter whose name is in `allowed`.
pub fn check_route_pattern(path: &str, allowed: &[&str]) -> Result<()> {
    let mut params = 0;
    let mut concrete = Vec::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        match param_name(seg) {
            Some(name) if allowed.contains(&name) => params += 1,
            Some(name) => bail!(
                "`{{{name}}}` is not a parameter here (use {})",
                allowed
                    .iter()
                    .map(|a| format!("`{{{a}}}`"))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            None => concrete.push(seg),
        }
    }
    if params != 1 {
        bail!(
            "the path needs exactly one parameter segment such as `/{}/`",
            allowed
                .iter()
                .map(|a| format!("{{{a}}}"))
                .next()
                .unwrap_or_default()
        );
    }
    check_route_path(&format!("/{}", concrete.join("/")))
}

/// Match a normalised concrete path against a normalised pattern. Returns the
/// parameter values.
pub fn match_pattern(pattern: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let pat: Vec<&str> = pattern.split('/').collect();
    let got: Vec<&str> = path.split('/').collect();
    if pat.len() != got.len() {
        return None;
    }
    let mut params = BTreeMap::new();
    for (p, g) in pat.iter().zip(&got) {
        match param_name(p) {
            Some(name) if !g.is_empty() => {
                params.insert(name.to_string(), (*g).to_string());
            }
            Some(_) => return None,
            None if p == g => {}
            None => return None,
        }
    }
    Some(params)
}

/// Fill a pattern's parameter with a value (already a valid URL segment).
pub fn fill_pattern(pattern: &str, name: &str, value: &str) -> String {
    pattern.replace(&format!("{{{name}}}"), value)
}

/// Output file for a normalised path.
pub fn output_file(path: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    if path.ends_with('/') {
        p.push("index.html");
    }
    p
}

fn listing_path(base: &str, n: usize) -> String {
    if n <= 1 {
        base.to_string()
    } else {
        format!("{base}page/{n}/")
    }
}

/// URL of listing page `n` of a section whose first page is at `base`.
pub fn page_url(base: &str, n: usize) -> String {
    listing_path(base, n)
}

/// The page number of `path` in a list whose first page is `base`
/// (`/a/` is 1, `/a/page/2/` is 2). `page/1/` is not a page (one URL each).
pub fn match_list_page(base: &str, path: &str) -> Option<usize> {
    if path == base {
        return Some(1);
    }
    let n: usize = path
        .strip_prefix(base)?
        .strip_prefix("page/")?
        .strip_suffix('/')?
        .parse()
        .ok()?;
    (n >= 2 && listing_path(base, n) == path).then_some(n)
}

/// Build the route table. `template_exists` tells whether a template name
/// resolves (site or built-in), to pick `home.html` for the home page.
pub fn build_routes(
    cfg: &SiteConfig,
    content: &Content,
    template_exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<Route>> {
    let mut routes: Vec<Route> = Vec::new();
    let explicit: BTreeMap<String, usize> = cfg
        .routes
        .iter()
        .enumerate()
        .map(|(i, r)| (normalize_path(&r.path), i))
        .collect();

    for (idx, page) in content.pages.iter().enumerate() {
        let over = explicit.get(&page.url).map(|i| &cfg.routes[*i]);
        let mode = cfg.effective_mode(over.and_then(|r| r.mode).or(page.mode));
        features::ensure_mode(&format!("{}", page.source.display()), mode)?;

        let default_template = match page.kind {
            PageKind::Section if page.url == "/" && template_exists("home.html") => {
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
        let title = over.and_then(|r| r.title.clone());
        let description = over.and_then(|r| r.description.clone());

        if page.kind == PageKind::Section {
            let folder = page.id_folder().to_string();
            let count = content.section_pages(&folder).len();
            let per_page = page.front.paginate.unwrap_or(0);
            let total = if per_page == 0 {
                1
            } else {
                count.div_ceil(per_page).max(1)
            };
            for n in 1..=total {
                let path = listing_path(&page.url, n);
                routes.push(Route {
                    output: output_file(&path),
                    path,
                    template: template.clone(),
                    mode,
                    source: RouteSource::Content(idx),
                    title: title.clone(),
                    description: description.clone(),
                    listing: Some(Listing {
                        section: folder.clone(),
                        number: n,
                        total,
                        per_page,
                    }),
                });
            }
        } else {
            routes.push(Route {
                path: page.url.clone(),
                output: output_file(&page.url),
                template,
                mode,
                source: RouteSource::Content(idx),
                title,
                description,
                listing: None,
            });
        }
    }

    for (path, i) in &explicit {
        let r = &cfg.routes[*i];
        if r.is_dynamic() || r.is_wildcard() || content.by_url(path).is_some() {
            continue;
        }
        let Some(template) = r.template.clone() else {
            bail!(
                "route `{}` has no template and no content file at that path \
                 (add `template = \"...\"` or a file under content/)",
                r.path
            );
        };
        routes.push(Route {
            path: path.clone(),
            output: output_file(path),
            template,
            mode: cfg.effective_mode(r.mode),
            source: RouteSource::Explicit(*i),
            title: r.title.clone(),
            description: r.description.clone(),
            listing: None,
        });
    }

    check_collisions(&mut routes)?;
    Ok(routes)
}

/// Fail if two routes write the same file; sort by path.
pub fn check_collisions(routes: &mut [Route]) -> Result<()> {
    let mut seen: BTreeMap<&PathBuf, &str> = BTreeMap::new();
    for r in routes.iter() {
        if let Some(other) = seen.insert(&r.output, &r.path) {
            bail!(
                "routes `{other}` and `{}` both write {}",
                r.path,
                r.output.display()
            );
        }
    }
    routes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(files: &[(&str, &str)], toml: &str) -> (tempfile::TempDir, SiteConfig, Content) {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let f = dir.path().join("content").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        let cfg = SiteConfig::parse(toml).unwrap();
        cfg.validate().unwrap();
        let content = Content::load(dir.path(), false).unwrap();
        (dir, cfg, content)
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/about"), "/about/");
        assert_eq!(normalize_path("/about/"), "/about/");
        assert_eq!(normalize_path("/feed.xml"), "/feed.xml");
        assert_eq!(output_file("/"), PathBuf::from("index.html"));
        assert_eq!(output_file("/a/b/"), PathBuf::from("a/b/index.html"));
        assert_eq!(output_file("/feed.xml"), PathBuf::from("feed.xml"));
        assert!(check_route_path("/a/../b").is_err());
        assert!(check_route_path("/.git/config").is_err());
        assert!(check_route_path("/a b").is_err());
        assert!(check_route_path("/docs/v1.0/").is_ok());
    }

    #[test]
    fn content_and_explicit_routes() {
        let (_d, cfg, content) = site(
            &[
                ("_index.md", "# Home"),
                ("about.md", "About"),
                ("blog/_index.md", "+++\npaginate = 2\n+++\n"),
                ("blog/a.md", "+++\ndate = 2026-01-01\n+++\n"),
                ("blog/b.md", "+++\ndate = 2026-01-02\n+++\n"),
                ("blog/c.md", "+++\ndate = 2026-01-03\n+++\n"),
            ],
            "[site]\nname='x'\n[[routes]]\npath='/about'\ntemplate='about.html'\ntitle='Us'\n\
             [[routes]]\npath='/contact'\ntemplate='contact.html'\n",
        );
        let routes = build_routes(&cfg, &content, &|t| t == "home.html").unwrap();
        let paths: Vec<_> = routes.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/",
                "/about/",
                "/blog/",
                "/blog/a/",
                "/blog/b/",
                "/blog/c/",
                "/blog/page/2/",
                "/contact/"
            ]
        );
        let get = |p: &str| routes.iter().find(|r| r.path == p).unwrap();
        assert_eq!(get("/").template, "home.html");
        assert_eq!(get("/about/").template, "about.html");
        assert_eq!(get("/about/").title.as_deref(), Some("Us"));
        assert_eq!(get("/blog/").template, "section.html");
        assert_eq!(get("/blog/a/").template, "page.html");
        assert_eq!(get("/blog/page/2/").listing.as_ref().unwrap().number, 2);
        assert_eq!(get("/blog/page/2/").listing.as_ref().unwrap().total, 2);
        assert_eq!(get("/contact/").source, RouteSource::Explicit(1));
        assert_eq!(get("/contact/").output, PathBuf::from("contact/index.html"));
        assert!(routes.iter().all(|r| r.mode == RenderMode::Build));
    }

    #[test]
    fn patterns_match_and_fill() {
        assert!(check_route_pattern("/articles/{slug}/", &["slug", "id"]).is_ok());
        assert!(check_route_pattern("/{term}/", &["term"]).is_ok());
        assert!(check_route_pattern("/articles/", &["slug"]).is_err());
        assert!(check_route_pattern("/a/{slug}/{id}/", &["slug", "id"]).is_err());
        assert!(check_route_pattern("/a/{x}/", &["slug"]).is_err());
        assert!(check_route_pattern("/../{slug}/", &["slug"]).is_err());
        let m = match_pattern("/articles/{slug}/", "/articles/hello/").unwrap();
        assert_eq!(m["slug"], "hello");
        assert!(match_pattern("/articles/{slug}/", "/articles/").is_none());
        assert!(match_pattern("/articles/{slug}/", "/articles/a/b/").is_none());
        assert!(match_pattern("/articles/{slug}/", "/news/a/").is_none());
        assert_eq!(
            fill_pattern("/articles/{slug}/", "slug", "x-y"),
            "/articles/x-y/"
        );
    }

    #[test]
    fn explicit_route_without_template_or_content_fails() {
        let (_d, cfg, content) = site(&[], "[site]\nname='x'\n[[routes]]\npath='/x'\n");
        let err = build_routes(&cfg, &content, &|_| false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no template"), "{err}");
    }

    #[test]
    fn page_mode_in_front_matter_is_checked() {
        let (_d, cfg, content) = site(
            &[("app.md", "+++\nmode = 'request'\n+++\n")],
            "[site]\nname='x'\n",
        );
        let res = build_routes(&cfg, &content, &|_| false);
        if cfg!(feature = "request") {
            assert_eq!(res.unwrap()[0].mode, RenderMode::Request);
        } else {
            let err = res.unwrap_err().to_string();
            assert!(err.contains("not in this build"), "{err}");
        }
    }

    #[test]
    fn colliding_outputs_fail() {
        let (_d, cfg, content) = site(
            &[
                ("blog/_index.md", "+++\npaginate=1\n+++\n"),
                ("blog/a.md", ""),
                ("blog/b.md", ""),
                ("blog/page.md", ""),
            ],
            "[site]\nname='x'\n[[routes]]\npath='/blog/page/2'\ntemplate='x.html'\n",
        );
        assert!(build_routes(&cfg, &content, &|_| false).is_err());
    }
}
