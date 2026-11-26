//! `engine components`: the component catalogue of this build (and of a
//! site, when run in one) and the site's design tokens, as a table or JSON.
//! The admin serves the same data at `/admin/api/components` and
//! `/admin/api/tokens`.

use std::path::Path;

use anyhow::Result;
use engine_core::builder;
use engine_core::components::{Catalogue, GROUPS};
use include_dir::Dir;

/// The component template folders of the compiled features (core's own
/// are added by the catalogue).
pub fn feature_dirs() -> Vec<&'static Dir<'static>> {
    #[allow(unused_mut)]
    let mut v: Vec<&'static Dir<'static>> = Vec::new();
    #[cfg(feature = "content")]
    v.push(&engine_content::render::LISTINGS);
    #[cfg(feature = "search")]
    v.push(&engine_search::ext::COMPONENTS);
    #[cfg(feature = "auth")]
    v.push(&engine_auth::site::COMPONENTS);
    #[cfg(feature = "forms")]
    v.extend(engine_forms::component_dirs());
    v
}

/// The catalogue of this build, with the site's own components and
/// overrides when `site` holds a site.
pub fn catalogue(site: &Path) -> Result<Catalogue> {
    let templates = site.join("templates");
    let site_templates =
        (site.join("site.toml").is_file() && templates.is_dir()).then_some(templates);
    Catalogue::load(&feature_dirs(), site_templates.as_deref()).map_err(|e| anyhow::anyhow!(e))
}

/// The site's design tokens as JSON (TagBrush defaults when not in a site).
pub fn tokens(site: &Path) -> Result<serde_json::Value> {
    let tokens = if site.join("site.toml").is_file() {
        builder::load_config(site)?.0.ui.tokens
    } else {
        Default::default()
    };
    Ok(engine_core::ui::tokens_json(&tokens))
}

pub fn run(site: &Path, json: bool) -> Result<()> {
    let cat = catalogue(site)?;
    let tokens = tokens(site)?;
    if json {
        let mut v = cat.to_json();
        v["design"] = tokens;
        v["preset"] = serde_json::json!(engine_core::features::PRESET);
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "Components in this build (preset `{}`):",
        engine_core::features::PRESET
    );
    for (group, label) in GROUPS {
        let list: Vec<_> = cat
            .components()
            .iter()
            .filter(|c| c.group == *group)
            .collect();
        if list.is_empty() {
            continue;
        }
        println!("\n{label}");
        for c in list {
            let mut notes = Vec::new();
            if c.script.is_some() {
                notes.push("script");
            }
            if c.style.is_some() {
                notes.push("style");
            }
            if c.overridden {
                notes.push("overridden by the site");
            }
            if c.source == "site" {
                notes.push("site component");
            }
            println!(
                "  {:<18} {:<7} {:<14} {}",
                c.name,
                c.version,
                c.features.join("+"),
                notes.join(", ")
            );
        }
    }
    println!("\nDesign tokens ([ui.tokens] in site.toml):");
    if let Some(list) = tokens["tokens"].as_array() {
        for t in list {
            let value = match &t["value"] {
                serde_json::Value::Null => "(TagBrush default)".to_string(),
                serde_json::Value::String(s) => s.clone(),
                v => format!("light {} / dark {}", v["light"], v["dark"]),
            };
            println!(
                "  {:<16} {:<24} {}",
                t["name"].as_str().unwrap_or_default(),
                t["property"].as_str().unwrap_or_default(),
                value
            );
        }
    }
    println!("\n`engine components --json` prints all of this, with arguments and editor hints.");
    Ok(())
}
