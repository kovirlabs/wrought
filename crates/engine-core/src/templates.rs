//! minijinja templates.
//!
//! Names resolve against the site's `templates/` folder first, then the
//! built-in templates embedded in the binary (`crates/engine-core/builtin/`).
//! So a site overrides any built-in layout or component by adding a file with
//! the same name, e.g. `templates/components/card.html`.
//!
//! Auto-escaping is on for `.html`, `.xml` and `.txt` templates. Rendered
//! Markdown is inserted with `{{ page.content | safe }}`.
//!
//! Functions available in every template:
//!
//! * `component(name, **args)` renders `components/<name>.html` with `args`
//!   plus `site`, `page`, `ui` and `engine` from the caller.
//! * `section_pages(section, limit=None)` lists a section's pages, in the
//!   section's sort order (records `c:<type>`).
//! * `get_page(id)` looks up one page by id (`blog/hello`, `docs`) or URL
//!   (records `i:<type>:<id>`).
//! * `tag(name)` records an `s:<name>` singleton tag.
//!
//! Filters: `date` (`"2026-11-20"` → `20 November 2026`; `date("short")` →
//! `20 Nov 2026`), `absolute_url` (prefixes `site.base_url`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use include_dir::{Dir, include_dir};
use minijinja::value::{Kwargs, Value};
use minijinja::{AutoEscape, Environment, Error, ErrorKind, State, UndefinedBehavior};

use crate::content::{Content, Page};
use crate::tags::{self, CacheTag};

/// Built-in layouts and components, embedded at compile time.
pub static BUILTIN: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin");

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
    validate_template_name(name).ok()?;
    if let Some(dir) = site_templates {
        let p = dir.join(name);
        if p.is_file() {
            return Some(TemplateSource::Site(p));
        }
    }
    BUILTIN.get_file(name).map(|_| TemplateSource::Builtin)
}

fn load(site_templates: Option<&Path>, name: &str) -> Result<Option<String>, Error> {
    match resolve(site_templates, name) {
        Some(TemplateSource::Site(p)) => std::fs::read_to_string(&p).map(Some).map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("reading {}: {e}", p.display()),
            )
        }),
        Some(TemplateSource::Builtin) => Ok(BUILTIN
            .get_file(name)
            .and_then(|f| f.contents_utf8())
            .map(str::to_string)),
        None => Ok(None),
    }
}

/// A page as templates see it.
pub fn page_value(p: &Page) -> Value {
    Value::from_serialize(p)
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
    env.set_loader(move |name| load(dir.as_deref(), name));

    env.add_function("component", component);

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

/// `component(name, **args)`: render `components/<name>.html`.
fn component(state: &State, name: String, kwargs: Kwargs) -> Result<Value, Error> {
    if name.is_empty()
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
    for key in [
        "site",
        "page",
        "ui",
        "engine",
        "route",
        "listing",
        "breadcrumbs",
    ] {
        if let Some(v) = state.lookup(key) {
            ctx.insert(key.to_string(), v);
        }
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
        let env = environment(None, content, String::new());
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
