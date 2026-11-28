//! minijinja templates.
//!
//! Names resolve against the site's `templates/` folder first, then the
//! built-in templates embedded in the binary (`crates/engine-core/builtin/`).
//! So a site overrides any built-in layout or component by adding a file with
//! the same name, e.g. `templates/components/card.html`.
//!
//! Auto-escaping is on for `.html`, `.xml` and `.txt` templates. Rendered
//! HTML (Markdown `page.content` and `page.summary`, sanitised `markdown`
//! fields from the database, component and block output) arrives as *safe*
//! values and prints as-is with `{{ page.content }}`; everything else is
//! escaped. Templates should never need `| safe`, and `engine build` warns
//! about every use of it in a site's templates.
//!
//! Functions available in every template:
//!
//! * `component(name, args=None, **kwargs)` renders `components/<name>.html`
//!   with `args` (a map, e.g. a block) and `kwargs`, plus `site`, `page`,
//!   `ui` and `engine` from the caller.
//! * `component_assets(name)` prints the `<link>`/`<script>` tags of a
//!   built-in component's assets (see [`crate::components`]).
//! * `token_assets()` prints the `<link>` of the token stylesheet, which
//!   styles `data-color`/`data-background`/`data-font`/`data-padding`
//!   attributes holding `token` field values (see
//!   [`crate::ui::TOKEN_CHOICES`]); `render_block` adds it for blocks with
//!   token fields.
//! * `component_catalogue()` lists the components of this build.
//! * `video_source(url)` understands a video address (YouTube, Vimeo, a
//!   file); see [`crate::components::video_source`].
//! * `section_pages(section, limit=None)` lists a section's pages, in the
//!   section's sort order (records `c:<type>`).
//! * `get_page(id)` looks up one page by id (`blog/hello`, `docs`) or URL
//!   (records `i:<type>:<id>`).
//! * `tag(name)` records an `s:<name>` singleton tag.
//!
//! Filters: `date` (`"2026-11-20"` → `20 November 2026`; `date("short")` →
//! `20 Nov 2026`), `absolute_url` (prefixes `site.base_url`), `markdown`
//! (Markdown from editors or visitors to safe HTML: raw HTML shown as text,
//! unsafe link schemes dropped; HTML the engine already made safe, such as
//! a block's `markdown` field, passes through unchanged).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use include_dir::{Dir, include_dir};
use minijinja::value::{Kwargs, Value};
use minijinja::{AutoEscape, Environment, Error, ErrorKind, State, UndefinedBehavior};

use crate::components::Catalogue;
use crate::content::{Content, Page};
use crate::tags::{self, CacheTag};

/// The error of a template function that failed because something it called
/// failed (a query, a store). The message says what happened; the cause is
/// kept with the error, so whoever renders can still tell why with
/// [`any_cause`], e.g. that the database is unavailable (`503`, not `500`).
/// A function must never flatten such an error into a string.
pub fn fn_error(e: impl Into<anyhow::Error>) -> Error {
    let e = e.into();
    Error::new(ErrorKind::InvalidOperation, format!("{e:#}")).with_source(Cause(e))
}

/// [`fn_error`] with the function's name in front of the message.
pub fn fn_error_in(what: &str, e: impl Into<anyhow::Error>) -> Error {
    let e = e.into();
    Error::new(ErrorKind::InvalidOperation, format!("{what}: {e:#}")).with_source(Cause(e))
}

/// A failed render as an `anyhow::Error`: its message is minijinja's long
/// form (template, line, excerpt), and the template error is kept for
/// [`any_cause`].
pub fn render_error(e: Error) -> anyhow::Error {
    anyhow::Error::new(RenderFailure(e))
}

/// Whether `f` holds for `e` or anything that caused it: its `source()`
/// chain, and through the causes kept by [`fn_error`] and [`render_error`]
/// (which are left out of `source()` so a logged chain does not print the
/// same message twice).
pub fn any_cause(
    e: &(dyn std::error::Error + 'static),
    f: &mut dyn FnMut(&(dyn std::error::Error + 'static)) -> bool,
) -> bool {
    let mut next = Some(e);
    while let Some(link) = next {
        if f(link) {
            return true;
        }
        if let Some(Cause(inner)) = link.downcast_ref::<Cause>() {
            let inner: &(dyn std::error::Error + 'static) = inner.as_ref();
            return any_cause(inner, f);
        }
        if let Some(RenderFailure(inner)) = link.downcast_ref::<RenderFailure>() {
            return any_cause(inner, f);
        }
        next = link.source();
    }
    false
}

/// What [`fn_error`] keeps. Prints nothing of its own (the template error's
/// message already says it).
#[derive(Debug)]
struct Cause(anyhow::Error);

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for Cause {}

#[derive(Debug)]
struct RenderFailure(Error);

impl std::fmt::Display for RenderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // minijinja's alternate format includes the template name, line and
        // a source excerpt.
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for RenderFailure {}

/// Built-in layouts and components, embedded at compile time.
pub static BUILTIN: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin");

/// The App UI components (data table, stat card, chart, alert, modal,
/// toast, empty state), compiled with the `request` render mode.
#[cfg(feature = "request")]
pub static BUILTIN_APP: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin-app");

/// Core's own template folders: the App UI one in builds with `request`,
/// and the built-in one.
#[cfg(feature = "request")]
pub fn core_dirs() -> Vec<&'static Dir<'static>> {
    vec![&BUILTIN_APP, &BUILTIN]
}

/// Core's own template folder (no App UI without `request`).
#[cfg(not(feature = "request"))]
pub fn core_dirs() -> Vec<&'static Dir<'static>> {
    vec![&BUILTIN]
}

/// Reject template names that are absolute or climb out of `templates/`.
pub fn validate_template_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with('/')
        && name.split('/').all(|seg| {
            !seg.is_empty()
                && !seg.starts_with('.')
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "template name `{name}` must be a relative path like `page.html` or \
             `components/card.html` (no `..`, no leading `/` or `.`)"
        ))
    }
}

/// Where a template name resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    Site(PathBuf),
    Builtin,
}

/// Resolve a template name: the site's file wins over the built-in one.
pub fn resolve(site_templates: Option<&Path>, name: &str) -> Option<TemplateSource> {
    resolve_in(site_templates, &[], name)
}

/// Like [`resolve`], with feature template folders searched between the
/// site's and core's.
pub fn resolve_in(
    site_templates: Option<&Path>,
    dirs: &[&'static Dir<'static>],
    name: &str,
) -> Option<TemplateSource> {
    builtin_file(site_templates, dirs, name).map(|(src, _)| src)
}

fn builtin_file(
    site_templates: Option<&Path>,
    dirs: &[&'static Dir<'static>],
    name: &str,
) -> Option<(TemplateSource, Option<&'static str>)> {
    validate_template_name(name).ok()?;
    if let Some(dir) = site_templates {
        let p = dir.join(name);
        if p.is_file() {
            return Some((TemplateSource::Site(p), None));
        }
    }
    dirs.iter()
        .copied()
        .chain([&BUILTIN])
        .find_map(|d| d.get_file(name))
        .map(|f| (TemplateSource::Builtin, f.contents_utf8()))
}

fn load(
    site_templates: Option<&Path>,
    dirs: &[&'static Dir<'static>],
    name: &str,
) -> Result<Option<String>, Error> {
    match builtin_file(site_templates, dirs, name) {
        Some((TemplateSource::Site(p), _)) => std::fs::read_to_string(&p).map(Some).map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("reading {}: {e}", p.display()),
            )
        }),
        Some((TemplateSource::Builtin, text)) => Ok(text.map(str::to_string)),
        None => Ok(None),
    }
}

/// A page as templates see it. `content` and `summary` are rendered HTML and
/// arrive as *safe* values, so templates print them with `{{ page.content }}`
/// and never need `| safe`; every other field is plain data and is escaped.
pub fn page_value(p: &Page) -> Value {
    let mut m = value_map(&Value::from_serialize(p));
    m.insert("content".into(), Value::from_safe_string(p.content.clone()));
    m.insert("summary".into(), Value::from_safe_string(p.summary.clone()));
    Value::from(m)
}

/// The entries of a map value (empty for anything else).
pub fn value_map(v: &Value) -> std::collections::BTreeMap<String, Value> {
    let mut m = std::collections::BTreeMap::new();
    if let Ok(keys) = v.try_iter() {
        for k in keys {
            if let Ok(val) = v.get_item(&k) {
                m.insert(k.to_string(), val);
            }
        }
    }
    m
}

/// Lint a template source: `| safe` turns auto-escaping off for a value.
/// Rendered Markdown, component and block output are already safe values,
/// so `| safe` is never needed for them; on anything else (a text field, a
/// query parameter) it is an XSS hole. Returns the 1-based lines that use it.
pub fn safe_filter_lines(source: &str) -> Vec<usize> {
    // Blank out `{# comments #}` (keeping line breaks) so prose that
    // mentions the filter is not reported.
    let mut text = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("{#") {
        text.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find("#}").map(|e| e + 2).unwrap_or(after.len());
        text.extend(after[..end].chars().filter(|c| *c == '\n'));
        rest = &after[end..];
    }
    text.push_str(rest);
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let mut rest = *l;
            while let Some(i) = rest.find('|') {
                rest = &rest[i + 1..];
                let t = rest.trim_start();
                if let Some(after) = t.strip_prefix("safe")
                    && !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                {
                    return true;
                }
            }
            false
        })
        .map(|(i, _)| i + 1)
        .collect()
}

/// `| safe` uses in a site's `templates/` folder, as warnings.
pub fn lint_site_templates(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "html" || x == "xml") {
                files.push(p);
            }
        }
    }
    files.sort();
    for f in files {
        let Ok(src) = std::fs::read_to_string(&f) else {
            continue;
        };
        for line in safe_filter_lines(&src) {
            let rel = f.strip_prefix(dir).unwrap_or(&f);
            out.push(format!(
                "templates/{}:{line}: `| safe` turns off HTML escaping. Rendered Markdown \
                 (page.content, markdown fields), component() and render_block() output are \
                 already safe and need no filter; on any other value it allows script \
                 injection",
                rel.display()
            ));
        }
    }
    out
}

fn format_date(s: &str, short: bool) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let parts: Vec<&str> = s.get(..10).unwrap_or("").split('-').collect();
    if let [y, m, d] = parts[..]
        && let (Ok(m), Ok(d)) = (m.parse::<usize>(), d.parse::<u32>())
        && (1..=12).contains(&m)
    {
        let month = MONTHS[m - 1];
        let month = if short { &month[..3] } else { month };
        return format!("{d} {month} {y}");
    }
    s.to_string()
}

/// Escape text for HTML element content and quoted attribute values.
pub fn escape_html(s: &str, out: &mut impl std::fmt::Write) -> std::fmt::Result {
    let mut last = 0;
    for (i, c) in s.char_indices() {
        let rep = match c {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            _ => continue,
        };
        out.write_str(&s[last..i])?;
        out.write_str(rep)?;
        last = i + 1;
    }
    out.write_str(&s[last..])
}

/// Output formatter: `none` and undefined print nothing; with auto-escaping
/// on, unsafe values are HTML-escaped (`/` is left alone so URLs stay
/// readable; it needs no escaping in quoted attributes or text).
fn format_value(out: &mut minijinja::Output, state: &State, value: &Value) -> Result<(), Error> {
    if value.is_none() || value.is_undefined() {
        return Ok(());
    }
    let fmt_err = |_| Error::new(ErrorKind::WriteFailure, "formatting failed");
    if !matches!(state.auto_escape(), AutoEscape::None) && !value.is_safe() {
        match value.as_str() {
            Some(s) => escape_html(s, out).map_err(fmt_err),
            None => escape_html(&value.to_string(), out).map_err(fmt_err),
        }
    } else {
        write!(out, "{value}").map_err(fmt_err)
    }
}

/// Build the template environment for one site build.
pub fn environment(
    site_templates: Option<PathBuf>,
    content: Arc<Content>,
    base_url: String,
    dirs: Vec<&'static Dir<'static>>,
    catalogue: Arc<Catalogue>,
) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html")
            || name.ends_with(".htm")
            || name.ends_with(".xml")
            || name.ends_with(".txt")
        {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.set_formatter(format_value);
    env.set_keep_trailing_newline(true);
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);

    let dir = site_templates.clone();
    env.set_loader(move |name| load(dir.as_deref(), &dirs, name));

    env.add_function("component", component);
    let cat = catalogue.clone();
    env.add_function(
        "component_assets",
        move |name: String| -> Result<Value, Error> {
            let c = cat.get(&name).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("component_assets: no component `{name}`"),
                )
            })?;
            Ok(Value::from_safe_string(c.asset_tags()))
        },
    );
    let tokens_link = catalogue
        .assets()
        .iter()
        .find(|a| {
            a.path
                .starts_with(&format!("{}tokens-", crate::components::ASSET_PREFIX))
        })
        .map(|a| format!("<link rel=\"stylesheet\" href=\"{}\">", a.path))
        .unwrap_or_default();
    env.add_function("token_assets", move || -> Value {
        Value::from_safe_string(tokens_link.clone())
    });
    let cat = catalogue;
    env.add_function("component_catalogue", move || -> Value {
        Value::from(
            cat.components()
                .iter()
                .map(|c| {
                    let mut m = value_map(&Value::from_serialize(c));
                    m.insert("example".into(), c.example_value());
                    Value::from(m)
                })
                .collect::<Vec<_>>(),
        )
    });
    env.add_function("video_source", |url: String| -> Value {
        crate::components::video_source(&url)
            .map(|v| Value::from_serialize(&v))
            .unwrap_or(Value::from(()))
    });
    // A value that is already safe HTML (a `markdown` field of a block or
    // item, rendered and sanitised by the engine) passes through: running
    // it through Markdown again would show its tags as text.
    env.add_filter("markdown", |md: Value| {
        if md.is_safe() {
            return md;
        }
        let text = if md.is_none() || md.is_undefined() {
            String::new()
        } else {
            md.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| md.to_string())
        };
        Value::from_safe_string(crate::components::safe_markdown(&text))
    });

    let c = content.clone();
    env.add_function(
        "section_pages",
        move |section: String, kwargs: Kwargs| -> Result<Value, Error> {
            let limit: Option<usize> = kwargs.get("limit")?;
            kwargs.assert_all_used()?;
            let section = section.trim_matches('/').to_string();
            let tag_type = section.split('/').next().filter(|s| !s.is_empty());
            tags::record(CacheTag::collection(tag_type.unwrap_or("page")));
            let mut list = c.section_pages(&section);
            if let Some(l) = limit {
                list.truncate(l);
            }
            Ok(Value::from(
                list.into_iter().map(page_value).collect::<Vec<_>>(),
            ))
        },
    );

    let c = content;
    env.add_function("get_page", move |id: String| -> Value {
        let found = c.get(&id).or_else(|| {
            let url = crate::routes::normalize_path(&id);
            c.by_url(&url)
        });
        match found {
            Some(p) => {
                tags::record(p.tag());
                page_value(p)
            }
            None => Value::UNDEFINED,
        }
    });

    env.add_function("tag", |name: String| {
        tags::record(CacheTag::singleton(name));
    });

    env.add_filter("date", |s: String, fmt: Option<String>| {
        format_date(&s, fmt.as_deref() == Some("short"))
    });
    env.add_filter("absolute_url", move |path: String| {
        if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else {
            format!("{base_url}{path}")
        }
    });
    env
}

/// `component(name, args=None, **kwargs)`: render `components/<name>.html`.
/// `args` is a map of arguments (a block, an example); keyword arguments
/// win over it. Names starting with `_` are partials, not components.
fn component(
    state: &State,
    name: String,
    args: Option<Value>,
    kwargs: Kwargs,
) -> Result<Value, Error> {
    if name.is_empty()
        || name.starts_with('_')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("invalid component name `{name}`"),
        ));
    }
    let tmpl = state
        .env()
        .get_template(&format!("components/{name}.html"))?;
    let mut ctx = std::collections::BTreeMap::<String, Value>::new();
    // `user`, `request` and `csrf` exist only in `request` renders (and
    // never inside shared fragments, see `fragments`).
    for key in [
        "site",
        "page",
        "ui",
        "engine",
        "route",
        "listing",
        "breadcrumbs",
        "user",
        "request",
        "csrf",
        "form_state",
    ] {
        if let Some(v) = state.lookup(key) {
            ctx.insert(key.to_string(), v);
        }
    }
    if let Some(args) = args.filter(|a| !a.is_none() && !a.is_undefined()) {
        if args.kind() != minijinja::value::ValueKind::Map {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("component(\"{name}\", args): args must be a map"),
            ));
        }
        ctx.extend(value_map(&args));
    }
    for key in kwargs.args() {
        ctx.insert(key.to_string(), kwargs.get::<Value>(key)?);
    }
    let html = tmpl.render(Value::from(ctx))?;
    Ok(Value::from_safe_string(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    fn env_with(site: Option<&Path>) -> Environment<'static> {
        environment(
            site.map(Path::to_path_buf),
            Arc::new(Content::default()),
            "https://example.org".into(),
            Vec::new(),
            Arc::new(Catalogue::load(&[], site).unwrap()),
        )
    }

    #[test]
    fn template_names_are_validated() {
        for ok in ["page.html", "components/card.html", "a-b_c.d.html"] {
            assert!(validate_template_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x.html",
            "a/../../x",
            ".hidden.html",
            "a//b",
            "a\\b",
        ] {
            assert!(validate_template_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn builtin_component_renders_and_escapes() {
        let env = env_with(None);
        let out = env
            .render_str(
                r#"{{ component("card", title="<b>Hi</b>", href="/x/") }}"#,
                context! {},
            )
            .unwrap();
        assert!(out.contains("<article>"), "{out}");
        assert!(out.contains("&lt;b&gt;Hi&lt;/b&gt;"), "{out}");
        assert!(out.contains(r#"href="/x/""#), "{out}");
    }

    #[test]
    fn site_component_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let comp = dir.path().join("components");
        std::fs::create_dir_all(&comp).unwrap();
        std::fs::write(comp.join("card.html"), "<p>custom {{ title }}</p>").unwrap();
        assert_eq!(
            resolve(Some(dir.path()), "components/card.html"),
            Some(TemplateSource::Site(comp.join("card.html")))
        );
        assert_eq!(
            resolve(Some(dir.path()), "components/footer.html"),
            Some(TemplateSource::Builtin)
        );
        let env = env_with(Some(dir.path()));
        let out = env
            .render_str(r#"{{ component("card", title="x") }}"#, context! {})
            .unwrap();
        assert_eq!(out, "<p>custom x</p>");
        // Non-overridden components still come from the binary.
        let footer = env
            .render_str(
                r#"{{ component("footer") }}"#,
                context! { site => context!{ name => "S" } },
            )
            .unwrap();
        assert!(footer.contains("<footer>"), "{footer}");
    }

    #[test]
    fn traversal_template_names_do_not_load() {
        let env = env_with(None);
        assert!(env.get_template("../Cargo.toml").is_err());
        assert!(
            env.render_str(r#"{{ component("../x") }}"#, context! {})
                .is_err()
        );
    }

    #[test]
    fn functions_record_cache_tags() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join("content/blog");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("a.md"), "+++\ntitle='A'\n+++\n").unwrap();
        let content = Arc::new(Content::load(dir.path(), false).unwrap());
        let env = environment(None, content, String::new(), Vec::new(), Arc::default());
        let (out, tags) = tags::collect(|| {
            env.render_str(
                r#"{% for p in section_pages("blog") %}{{ p.title }}{% endfor %}|{{ get_page("blog/a").url }}|{{ tag("menu") }}"#,
                context! {},
            )
            .unwrap()
        });
        assert_eq!(out, "A|/blog/a/|");
        let tags: Vec<String> = tags.iter().map(ToString::to_string).collect();
        assert_eq!(tags, ["i:blog:blog/a", "c:blog", "s:menu"]);
    }

    /// Security review of v0.2: `page_list` printed `p.summary | safe`, so a
    /// database text field named `summary` (plain text, not sanitised) was
    /// inserted raw. Rendered HTML is now a safe *value*; the template has
    /// no `| safe`, and plain strings are always escaped.
    #[test]
    fn rendered_html_is_a_safe_value_and_text_is_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join("content/blog");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(
            c.join("a.md"),
            "+++\ntitle='<i>A</i>'\n+++\nHello *there*.\n",
        )
        .unwrap();
        let content = Arc::new(Content::load(dir.path(), false).unwrap());
        let env = environment(None, content, String::new(), Vec::new(), Arc::default());
        // Markdown pages: summary and content are HTML and print as HTML.
        let out = env
            .render_str(
                r#"{{ component("page_list", pages=section_pages("blog")) }}|{{ get_page("blog/a").content }}"#,
                context! {},
            )
            .unwrap();
        assert!(out.contains("<p>Hello <em>there</em>.</p>"), "{out}");
        assert!(
            out.contains("&lt;i&gt;A&lt;/i&gt;"),
            "titles are text: {out}"
        );
        // Anything else passed as `summary` (e.g. a text field of a database
        // item) is escaped.
        let hostile =
            context! { url => "/x/", title => "x", summary => "<script>alert(1)</script>" };
        let out = env
            .render_str(
                r#"{{ component("page_list", pages=[p]) }}"#,
                context! { p => hostile },
            )
            .unwrap();
        assert!(!out.contains("<script>"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
    }

    #[test]
    fn safe_filter_is_linted() {
        assert_eq!(
            safe_filter_lines(
                "a\n{{ x | safe }}\n{{ y|safe }}\n{{ z | safely }}\n{{ q | safe_url }}"
            ),
            [2, 3]
        );
        assert_eq!(
            safe_filter_lines("{# never `| safe`\n  here #}\n{{ a }}\n{{ b | safe }}"),
            [4]
        );
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("blocks")).unwrap();
        std::fs::write(d.path().join("blocks/q.html"), "{{ block.text | safe }}").unwrap();
        std::fs::write(d.path().join("ok.html"), "{{ page.content }}").unwrap();
        let w = lint_site_templates(d.path());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("templates/blocks/q.html:1:"), "{w:?}");
        // The engine's own templates never use it.
        fn walk(d: &Dir<'_>) {
            for f in d.files() {
                let src = f.contents_utf8().unwrap();
                assert!(safe_filter_lines(src).is_empty(), "{}", f.path().display());
            }
            for sub in d.dirs() {
                walk(sub);
            }
        }
        walk(&BUILTIN);
    }

    #[test]
    fn date_filter() {
        let env = env_with(None);
        let out = env
            .render_str(
                r#"{{ "2026-11-20" | date }} / {{ "2026-02-03T10:00:00Z" | date("short") }} / {{ "x" | date }}"#,
                context! {},
            )
            .unwrap();
        assert_eq!(out, "20 November 2026 / 3 Feb 2026 / x");
    }
}
