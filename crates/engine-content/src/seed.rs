//! Seed content from `seed/*.toml`.
//!
//! Each file holds arrays of tables named after content types:
//!
//! ```toml
//! [[author]]
//! slug = "ada"
//! name = "Ada Lovelace"
//!
//! [[article]]
//! title = "Hello"
//! status = "published"
//! author = "ada"          # relation, by slug
//! topics = ["Rust", "Web"]
//! body = """
//! Markdown here.
//! """
//! ```
//!
//! `slug` and `status` are optional (`status` defaults to `draft`). Seeding is
//! idempotent: an entry whose slug (given, or made from its title) already
//! exists is skipped, and a singleton that exists is left alone. Types are
//! seeded in dependency order, so relations can point at items from the
//! same run.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::slug;
use crate::store::{Changes, Key, Status, Store};
use crate::value;

#[derive(Debug, Default)]
pub struct SeedReport {
    /// `(type, slug)` of created items.
    pub created: Vec<(String, String)>,
    pub skipped: Vec<(String, String)>,
}

pub async fn seed_dir(store: &Store, site: &Path, author: Option<&str>) -> Result<SeedReport> {
    let dir = site.join("seed");
    let mut files = Vec::new();
    if dir.is_dir() {
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "toml") {
                files.push(p);
            }
        }
    }
    files.sort();
    if files.is_empty() {
        bail!("no seed files: add seed/*.toml to {}", site.display());
    }
    let mut entries: BTreeMap<String, Vec<(String, toml::Table)>> = BTreeMap::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        let table: toml::Table =
            toml::from_str(&text).with_context(|| format!("in {}", f.display()))?;
        for (type_name, list) in table {
            store
                .registry()
                .require(&type_name)
                .with_context(|| format!("in {}", f.display()))?;
            let toml::Value::Array(list) = list else {
                bail!(
                    "{}: `{type_name}` must be an array of tables (`[[{type_name}]]`)",
                    f.display()
                );
            };
            for v in list {
                let toml::Value::Table(t) = v else {
                    bail!("{}: `{type_name}` entries must be tables", f.display());
                };
                entries
                    .entry(type_name.clone())
                    .or_default()
                    .push((f.display().to_string(), t));
            }
        }
    }

    let mut report = SeedReport::default();
    let order: Vec<String> = store
        .registry()
        .dependency_order()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    for type_name in order {
        let Some(list) = entries.remove(&type_name) else {
            continue;
        };
        let t = store.registry().require(&type_name)?.clone();
        for (file, mut table) in list {
            let what = format!("{file}: [[{type_name}]]");
            let given_slug = match table.remove("slug") {
                Some(toml::Value::String(s)) => Some(s),
                Some(_) => bail!("{what}: `slug` must be text"),
                None => None,
            };
            let status = match table.remove("status") {
                Some(toml::Value::String(s)) => Some(
                    Status::parse(&s)
                        .with_context(|| format!("{what}: status must be draft or published"))?,
                ),
                Some(_) => bail!("{what}: status must be draft or published"),
                None => None,
            };
            let mut changes = Changes {
                slug: given_slug.clone(),
                status,
                ..Default::default()
            };
            for (k, v) in &table {
                changes.fields.push((k.clone(), value::from_toml(v)));
            }
            // Idempotency: skip what exists.
            let probe = if t.singleton {
                store.singleton(&type_name).await?.map(|i| i.slug)
            } else {
                let s = given_slug.clone().or_else(|| {
                    let from = t.slug_from.as_deref()?;
                    slug::slugify(table.get(from)?.as_str()?)
                });
                match s {
                    Some(s) => store
                        .get(&type_name, &Key::Slug(s.clone()), None)
                        .await?
                        .map(|_| s),
                    None => None,
                }
            };
            if let Some(s) = probe {
                report.skipped.push((type_name.clone(), s));
                continue;
            }
            let item = store
                .create(&type_name, &changes, author)
                .await
                .with_context(|| what.clone())?;
            report.created.push((type_name.clone(), item.slug));
        }
    }
    Ok(report)
}
