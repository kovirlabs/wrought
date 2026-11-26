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

/// The `name = ...` line in each example's site.toml that `new` rewrites.
const NAME_LINE: &str = "name = \"Static Blog\"";
#[cfg(feature = "cms")]
const CMS_NAME_LINE: &str = "name = \"CMS Site\"";
#[cfg(feature = "app")]
const APP_NAME_LINE: &str = "name = \"Small App\"";

pub fn new_site(dest: &Path, preset: &str) -> Result<()> {
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

/// Copy embedded files, skipping build output and hidden files.
fn extract(dir: &Dir, dest: &Path) -> Result<()> {
    for entry in dir.entries() {
        let rel = entry.path();
        let skip = rel.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s.starts_with('.') || s == "public"
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
