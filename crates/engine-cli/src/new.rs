//! `engine new <name> --preset static|cms|app`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use include_dir::{Dir, DirEntry, include_dir};

/// The static-blog example, embedded at compile time.
static STATIC_BLOG: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../examples/static-blog");

/// The cms-site example, embedded in cms builds.
#[cfg(feature = "cms")]
static CMS_SITE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../examples/cms-site");

/// The small-app example, embedded in app builds.
#[cfg(feature = "app")]
static SMALL_APP: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../examples/small-app");

/// The starter themes (`themes/<name>/theme.toml` + `files/`), embedded.
static THEMES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../themes");

/// The `name = ...` line in each example's site.toml that `new` rewrites.
const NAME_LINE: &str = "name = \"Static Blog\"";
#[cfg(feature = "cms")]
const CMS_NAME_LINE: &str = "name = \"CMS Site\"";
#[cfg(feature = "app")]
const APP_NAME_LINE: &str = "name = \"Small App\"";

#[cfg(test)]
pub fn new_site(dest: &Path, preset: &str) -> Result<()> {
    new_site_with(dest, preset, None)
}

/// A theme's `theme.toml`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeMeta {
    label: String,
    description: String,
    presets: Vec<String>,
    #[serde(default)]
    nav: Vec<NavItem>,
    #[serde(default)]
    routes: String,
    #[serde(default)]
    tokens: toml::Table,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq)]
struct NavItem {
    label: String,
    url: String,
}

fn theme_meta(name: &str) -> Option<ThemeMeta> {
    let text = THEMES
        .get_file(format!("{name}/theme.toml"))?
        .contents_utf8()?;
    Some(toml::from_str(text).expect("built-in theme.toml is valid (tested)"))
}

/// The starter themes: (name, label, description, presets).
pub fn themes() -> Vec<(String, String, String, Vec<String>)> {
    let mut out: Vec<_> = THEMES
        .dirs()
        .filter_map(|d| {
            let name = d.path().file_name()?.to_str()?.to_string();
            let m = theme_meta(&name)?;
            Some((name, m.label, m.description, m.presets))
        })
        .collect();
    out.sort();
    out
}

/// `engine new` with an optional starter theme laid over the preset.
pub fn new_site_with(dest: &Path, preset: &str, theme: Option<&str>) -> Result<()> {
    let meta = match theme {
        None => None,
        Some(t) => {
            let Some(m) = theme_meta(t) else {
                let names: Vec<String> = themes().into_iter().map(|t| t.0).collect();
                bail!("unknown theme `{t}`: use one of {}", names.join(", "));
            };
            if !m.presets.iter().any(|p| p == preset) {
                bail!(
                    "the `{t}` theme needs the {} preset{} (it uses components of features \
                     the `{preset}` preset does not have)",
                    m.presets.join(" or "),
                    if m.presets.len() > 1 { "s" } else { "" }
                );
            }
            Some((t, m))
        }
    };
    new_preset(dest, preset)?;
    if let Some((t, m)) = meta {
        apply_theme(dest, t, &m)?;
    }
    Ok(())
}

/// Copy the theme's files over the preset's, then add its design tokens,
/// navigation and routes to site.toml.
fn apply_theme(dest: &Path, name: &str, m: &ThemeMeta) -> Result<()> {
    let files = THEMES
        .get_dir(format!("{name}/files"))
        .with_context(|| format!("theme `{name}` has no files/"))?;
    extract_under(files, files.path(), dest)?;
    let toml_path = dest.join("site.toml");
    let mut text = std::fs::read_to_string(&toml_path)?;
    let cfg: toml::Table = toml::from_str(&text).context("reading the preset's site.toml")?;

    // Navigation: the theme's entries first, then the preset's.
    let mut nav = m.nav.clone();
    if let Some(old) = cfg
        .get("site")
        .and_then(|s| s.get("nav"))
        .and_then(|n| n.as_array())
    {
        for v in old {
            if let Ok(item) = v.clone().try_into::<NavItem>()
                && !nav.iter().any(|n| n.url == item.url)
            {
                nav.push(item);
            }
        }
    }
    let nav_text = {
        let mut s = String::from("nav = [\n");
        for n in &nav {
            s.push_str(&format!(
                "  {{ label = {}, url = {} }},\n",
                toml::Value::String(n.label.clone()),
                toml::Value::String(n.url.clone())
            ));
        }
        s.push(']');
        s
    };
    text = match text.find("\nnav = [") {
        Some(i) => {
            let start = i + 1;
            let end = if text[start..]
                .lines()
                .next()
                .is_some_and(|l| l.trim_end().ends_with(']'))
            {
                start + text[start..].find('\n').unwrap_or(text.len() - start)
            } else {
                start + text[start..].find("\n]").context("unterminated nav list")? + 2
            };
            format!("{}{nav_text}{}", &text[..start], &text[end..])
        }
        None => text.replacen("[site]\n", &format!("[site]\n{nav_text}\n"), 1),
    };

    // Design tokens: the theme's replace the preset's.
    if !m.tokens.is_empty() {
        let tokens = format!(
            "[ui.tokens]\n# From the `{name}` starter theme.\n{}",
            toml::to_string(&m.tokens)?
        );
        text = match text.find("\n[ui.tokens]") {
            Some(i) => {
                let start = i + 1;
                let body = &text[start + "[ui.tokens]".len()..];
                let end = body
                    .find("\n[")
                    .map_or(text.len(), |j| start + "[ui.tokens]".len() + j + 1);
                format!("{}{tokens}\n{}", &text[..start], &text[end..])
            }
            None => format!("{}\n\n{tokens}", text.trim_end()),
        };
    }
    if !m.routes.trim().is_empty() {
        text = format!("{}\n{}", text.trim_end(), m.routes);
    }
    toml::from_str::<toml::Table>(&text)
        .with_context(|| format!("site.toml after applying the `{name}` theme"))?;
    std::fs::write(&toml_path, text)?;
    Ok(())
}

/// Copy embedded files below `base` into `dest` (paths relative to `base`).
fn extract_under(dir: &Dir, base: &Path, dest: &Path) -> Result<()> {
    for entry in dir.entries() {
        let rel = entry.path().strip_prefix(base).unwrap_or(entry.path());
        match entry {
            DirEntry::Dir(d) => {
                std::fs::create_dir_all(dest.join(rel))?;
                extract_under(d, base, dest)?;
            }
            DirEntry::File(f) => {
                let path = dest.join(rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, f.contents())
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }
    Ok(())
}

fn new_preset(dest: &Path, preset: &str) -> Result<()> {
    let (example, name_line): (&Dir<'static>, &str) = match preset {
        "static" => (&STATIC_BLOG, NAME_LINE),
        #[cfg(feature = "cms")]
        "cms" => (&CMS_SITE, CMS_NAME_LINE),
        #[cfg(not(feature = "cms"))]
        "cms" => bail!(
            "the `cms` preset needs an engine built with the cms features (content, publish, \
             db-postgres): `cargo build --release --features cms`. This one is `static`."
        ),
        #[cfg(feature = "app")]
        "app" => (&SMALL_APP, APP_NAME_LINE),
        #[cfg(not(feature = "app"))]
        "app" => bail!(
            "the `app` preset needs an engine built with the app features (cms plus forms, jobs, \
             mail): `cargo build --release --features app`. This one is `{}`.",
            engine_core::features::PRESET
        ),
        other => bail!("unknown preset `{other}`: use static, cms or app"),
    };
    if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
        bail!("{} already exists and is not empty", dest.display());
    }
    let site_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("My site")
        .to_string();

    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    extract(example, dest)?;

    let toml_path = dest.join("site.toml");
    let text = std::fs::read_to_string(&toml_path)?;
    let quoted = toml::Value::String(site_name).to_string();
    std::fs::write(
        &toml_path,
        text.replacen(name_line, &format!("name = {quoted}"), 1),
    )?;
    Ok(())
}

/// Copy embedded files, skipping build output, local data (a SQLite
/// database and uploads, if the example was run in place) and hidden files.
fn extract(dir: &Dir, dest: &Path) -> Result<()> {
    for entry in dir.entries() {
        let rel = entry.path();
        let skip = rel.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s.starts_with('.') || s == "public" || s == "data"
        });
        if skip {
            continue;
        }
        match entry {
            DirEntry::Dir(d) => {
                std::fs::create_dir_all(dest.join(rel))?;
                extract(d, dest)?;
            }
            DirEntry::File(f) => {
                let path = dest.join(rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, f.contents())
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_has_the_name_line() {
        let toml = STATIC_BLOG
            .get_file("site.toml")
            .and_then(|f| f.contents_utf8())
            .unwrap();
        assert!(toml.contains(NAME_LINE));
    }

    #[test]
    fn scaffolds_and_rejects() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("my-blog");
        new_site(&dest, "static").unwrap();
        let toml = std::fs::read_to_string(dest.join("site.toml")).unwrap();
        assert!(toml.contains("name = \"my-blog\""), "{toml}");
        assert!(dest.join("content/_index.md").is_file());
        assert!(!dest.join("public").exists());
        // Not twice into the same folder.
        assert!(new_site(&dest, "static").is_err());
        let app = new_site(&d.path().join("x"), "app");
        if cfg!(feature = "app") {
            app.unwrap();
            let toml = std::fs::read_to_string(d.path().join("x/site.toml")).unwrap();
            assert!(toml.contains("name = \"x\""), "{toml}");
            assert!(d.path().join("x/forms/support.toml").is_file());
            assert!(d.path().join("x/migrations/postgres").is_dir());
        } else {
            let err = app.unwrap_err().to_string();
            assert!(err.contains("--features app"), "{err}");
        }
        let cms = new_site(&d.path().join("c"), "cms");
        if cfg!(feature = "cms") {
            cms.unwrap();
            let toml = std::fs::read_to_string(d.path().join("c/site.toml")).unwrap();
            assert!(toml.contains("name = \"c\""), "{toml}");
            assert!(d.path().join("c/types/article.toml").is_file());
        } else {
            assert!(cms.unwrap_err().to_string().contains("--features cms"));
        }
        assert!(new_site(&d.path().join("y"), "wordpress").is_err());
    }
}
