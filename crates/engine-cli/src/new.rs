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
    let quoted = toml::Value::String(site_name.clone()).to_string();
    #[allow(unused_mut)]
    let mut text = text.replacen(name_line, &format!("name = {quoted}"), 1);
    #[cfg(feature = "cms")]
    if preset != "static" {
        text = local_database(dest, &site_name, &text)?;
    }
    std::fs::write(&toml_path, text)?;
    Ok(())
}

/// The first port of `127.0.0.1` from here that nothing listens on is the
/// new site's PostgreSQL port: below the kernel's ephemeral range (32768 and
/// up on Linux), where an outgoing connection could hold it at any moment.
#[cfg(feature = "cms")]
const DB_PORTS: std::ops::RangeInclusive<u16> = 15432..=15531;

/// A database site's own PostgreSQL: `compose.yml` (PostgreSQL 17 in Docker,
/// on a free port of `127.0.0.1`, a password made for this folder) and
/// `[database] url` in `site_toml` pointing at it. Returns the new
/// site.toml. `docker compose up -d --wait` in the folder starts it;
/// `ENGINE_DATABASE_URL` still overrides the URL (production, or a server
/// the user already runs).
#[cfg(feature = "cms")]
fn local_database(dest: &Path, site_name: &str, site_toml: &str) -> Result<String> {
    let slug: String = site_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "site".to_string()
    } else {
        slug.chars()
            .take(40)
            .collect::<String>()
            .trim_end_matches('-')
            .to_string()
    };
    let port = DB_PORTS
        .clone()
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or(*DB_PORTS.start());
    let password: String = engine_core::web::secret::random_bytes(18)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let compose = format!(
        r#"# This site's own PostgreSQL, for development and small installs. Start it
# with `docker compose up -d --wait` in this folder; stop it with
# `docker compose down` (add `-v` to delete its data too).
#
# Made by `engine new`: the port ({port}) was free on 127.0.0.1 and the
# password was generated for this folder; `[database] url` in site.toml
# names both. In production, or with a PostgreSQL server you already run,
# set ENGINE_DATABASE_URL instead (it overrides site.toml) and delete this
# file.
name: wrought-site-{slug}

services:
  postgres:
    image: postgres:17
    environment:
      POSTGRES_USER: engine
      POSTGRES_PASSWORD: "{password}"
      POSTGRES_DB: site
    ports:
      - "127.0.0.1:{port}:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data
    # The image stops PostgreSQL cleanly on SIGINT (fast shutdown).
    stop_signal: SIGINT
    stop_grace_period: 2m
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -h 127.0.0.1 -U engine -d site"]
      interval: 1s
      timeout: 3s
      retries: 60
      start_period: 30s

volumes:
  pgdata:
"#
    );
    std::fs::write(dest.join("compose.yml"), compose)
        .with_context(|| format!("writing {}", dest.join("compose.yml").display()))?;
    let url = format!("postgres://engine:{password}@127.0.0.1:{port}/site");
    let line = format!(
        "[database]\n# This site's PostgreSQL from compose.yml (`docker compose up -d --wait`).\n\
         # ENGINE_DATABASE_URL overrides it.\nurl = \"{url}\"\n"
    );
    let text = match site_toml.find("\n[database]\n") {
        Some(i) => format!(
            "{}{line}{}",
            &site_toml[..i + 1],
            &site_toml[i + 1 + "[database]\n".len()..]
        ),
        None => format!("{}\n\n{line}", site_toml.trim_end()),
    };
    toml::from_str::<toml::Table>(&text).context("site.toml with the database URL")?;
    Ok(text)
}

/// Copy embedded files, skipping build output, local data (uploads, if
/// the example was run in place) and hidden files.
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
            // Its own PostgreSQL: compose.yml and the URL in site.toml agree.
            let compose = std::fs::read_to_string(d.path().join("c/compose.yml")).unwrap();
            let cfg: toml::Table = toml::from_str(&toml).unwrap();
            let url = cfg["database"]["url"].as_str().unwrap();
            let (pw, rest) = url
                .strip_prefix("postgres://engine:")
                .unwrap()
                .split_once("@127.0.0.1:")
                .unwrap();
            let port = rest.strip_suffix("/site").unwrap();
            assert_eq!(pw.len(), 36, "{url}");
            assert!(
                compose.contains(&format!("POSTGRES_PASSWORD: \"{pw}\"")),
                "{compose}"
            );
            assert!(
                compose.contains(&format!("\"127.0.0.1:{port}:5432\"")),
                "{compose}"
            );
            assert!(compose.contains("name: wrought-site-c\n"), "{compose}");
            assert!(
                !dest.join("compose.yml").exists(),
                "static sites need no database"
            );
            assert!(d.path().join("c/types/article.toml").is_file());
        } else {
            assert!(cms.unwrap_err().to_string().contains("--features cms"));
        }
        assert!(new_site(&d.path().join("y"), "wordpress").is_err());
    }
}
