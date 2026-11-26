//! Commands for database-backed sites (`content` and `publish` features):
//! `engine migrate`, `engine types`, `engine content`, `engine rebuild`, and
//! the database-aware parts of `build`, `dev` and `serve`.
//!
//! `engine content` goes through the same store (validation, optimistic
//! locking, revisions) as the admin, for scripts and quick edits.

use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use engine_content::schema::{self, SyncOptions};
use engine_content::store::{Changes, Item, Key, ListQuery, SaveError, Status};
use engine_content::{ContentApp, Registry, site as content_site, value};
use engine_core::builder::{self, BuildOptions, BuildReport};

use crate::SiteArg;
use crate::db::{Conn, connect, migrations_for};

#[derive(Subcommand)]
pub enum TypesCmd {
    /// Generate a migration from changes to types/*.toml.
    Sync {
        #[command(flatten)]
        site: SiteArg,
        /// Only report what would change (exit status 1 if anything would).
        #[arg(long)]
        check: bool,
        /// Allow changes that drop columns or tables (deletes data).
        #[arg(long)]
        allow_drop: bool,
        /// Record the current types as synced without writing a migration
        /// (after a hand-written migration).
        #[arg(long)]
        snapshot_only: bool,
    },
    /// List content types, their fields, blocks and vocabularies.
    List {
        #[command(flatten)]
        site: SiteArg,
    },
}

#[derive(Subcommand)]
pub enum ContentCmd {
    /// List items of a type (published only, unless --all).
    List {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        /// Include drafts.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        limit: Option<i64>,
        /// newest, oldest, updated, title, <field> or -<field>.
        #[arg(long, default_value = "newest")]
        order: String,
    },
    /// Show one item (by id or slug) as JSON.
    Show {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        /// Id or slug (omit for a singleton).
        key: Option<String>,
    },
    /// Create an item.
    Create {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        /// FIELD=VALUE; VALUE `@file` reads a file, `@@x` is a literal `@x`.
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        set: Vec<String>,
        /// Slug (default: made from the title).
        #[arg(long)]
        slug: Option<String>,
        /// Publish it (default: draft).
        #[arg(long)]
        publish: bool,
        /// Recorded on the revision.
        #[arg(long, default_value = "cli")]
        author: String,
    },
    /// Change an item. Only the given fields change.
    Set {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        /// Id or slug (omit for a singleton; created if missing).
        key: Option<String>,
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        set: Vec<String>,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long, conflicts_with = "unpublish")]
        publish: bool,
        #[arg(long)]
        unpublish: bool,
        /// Fail if the item is no longer at this version (optimistic lock).
        /// Without it, the version read just before saving is used.
        #[arg(long)]
        expect_version: Option<i32>,
        #[arg(long, default_value = "cli")]
        author: String,
    },
    /// Delete an item.
    Delete {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        key: String,
        #[arg(long)]
        expect_version: Option<i32>,
        #[arg(long, default_value = "cli")]
        author: String,
    },
    /// List an item's revisions, newest first.
    Revisions {
        #[command(flatten)]
        site: SiteArg,
        #[arg(value_name = "TYPE")]
        type_name: String,
        key: Option<String>,
    },
    /// List the terms of a vocabulary.
    Terms {
        #[command(flatten)]
        site: SiteArg,
        vocabulary: String,
    },
    /// Create items from seed/*.toml (skips items that exist).
    Seed {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long, default_value = "seed")]
        author: String,
    },
}

impl Conn {
    fn app(&self) -> Result<ContentApp> {
        let migrations = migrations_for(&self.site, self.db.backend())?;
        self.rt.block_on(ContentApp::open(
            &self.site,
            self.db.clone(),
            self.rt.handle().clone(),
            &migrations,
        ))
    }
}

pub fn types_cmd(cmd: TypesCmd) -> Result<std::process::ExitCode> {
    match cmd {
        TypesCmd::Sync {
            site,
            check,
            allow_drop,
            snapshot_only,
        } => {
            let site = site.site;
            builder::load_config(&site)?;
            let reg = Registry::load(&site)?;
            let out = schema::sync(
                &site,
                &reg,
                engine_db::Backend::Postgres,
                SyncOptions {
                    check,
                    allow_drop,
                    snapshot_only,
                },
            )?;
            if snapshot_only {
                println!("Recorded the current types as synced.");
            } else if out.plan.is_empty() {
                println!("Types are in sync with migrations.");
            } else {
                for s in &out.plan.summary {
                    println!("  {s}");
                }
                match out.file {
                    Some(f) => println!("Wrote {}. Next: engine migrate", f.display()),
                    None => {
                        println!("(check only: nothing written)");
                        return Ok(std::process::ExitCode::FAILURE);
                    }
                }
            }
        }
        TypesCmd::List { site } => {
            let reg = Registry::load(&site.site)?;
            if reg.is_empty() {
                println!("No content types (add types/<name>.toml).");
            }
            for t in reg.types() {
                println!(
                    "{}{}  ({}, table {})",
                    t.name,
                    if t.singleton { " [singleton]" } else { "" },
                    t.label,
                    t.table()
                );
                for f in &t.fields {
                    let mut extra = Vec::new();
                    if f.required {
                        extra.push("required".to_string());
                    }
                    if let Some(to) = &f.to {
                        extra.push(format!("-> {to}"));
                    }
                    if let Some(v) = &f.vocabulary {
                        extra.push(format!("vocabulary {v}"));
                    }
                    if !f.options.is_empty() {
                        extra.push(f.options.join("|"));
                    }
                    println!(
                        "  {:<20} {:<9} {}",
                        f.name,
                        f.kind.as_str(),
                        extra.join(", ")
                    );
                }
            }
            for b in reg.blocks() {
                let fields: Vec<&str> = b.fields.iter().map(|f| f.name.as_str()).collect();
                println!("block {}: {}", b.name, fields.join(", "));
            }
            let vocabs: Vec<&str> = reg.vocabularies().collect();
            if !vocabs.is_empty() {
                println!("vocabularies: {}", vocabs.join(", "));
            }
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn parse_sets(
    reg: &Registry,
    type_name: &str,
    sets: &[String],
) -> Result<Vec<(String, serde_json::Value)>> {
    let t = reg.require(type_name)?;
    let mut out = Vec::new();
    for s in sets {
        let (field, raw) = s
            .split_once('=')
            .with_context(|| format!("--set `{s}`: write FIELD=VALUE"))?;
        let def = t
            .field(field)
            .with_context(|| format!("--set: `{type_name}` has no field `{field}`"))?;
        let text = if let Some(lit) = raw.strip_prefix("@@") {
            format!("@{lit}")
        } else if let Some(path) = raw.strip_prefix('@') {
            std::fs::read_to_string(path)
                .with_context(|| format!("--set {field}: reading {path}"))?
        } else {
            raw.to_string()
        };
        let v = value::from_text(def, &text).map_err(|e| anyhow::anyhow!("--set {field}: {e}"))?;
        out.push((field.to_string(), v));
    }
    Ok(out)
}

fn find(app: &ContentApp, type_name: &str, key: Option<&str>) -> Result<Item> {
    let t = app.store.registry().require(type_name)?;
    let item = match key {
        Some(k) => app
            .rt
            .block_on(app.store.get(type_name, &Key::parse(k), None))?,
        None if t.singleton => app.rt.block_on(app.store.singleton(type_name))?,
        None => bail!("give the id or slug of the {type_name} to use"),
    };
    item.with_context(|| match key {
        Some(k) => format!("no {type_name} {}", Key::parse(k)),
        None => format!("the {type_name} singleton does not exist yet"),
    })
}

fn title_of(app: &ContentApp, i: &Item) -> String {
    app.store
        .registry()
        .get(&i.content_type)
        .and_then(|t| t.title_field.as_ref())
        .and_then(|f| i.fields.get(f))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn saved(app: &ContentApp, verb: &str, i: &Item) {
    println!(
        "{verb} {} #{} `{}` ({}, version {}): {}",
        i.content_type,
        i.id,
        i.slug,
        i.status.as_str(),
        i.version,
        title_of(app, i)
    );
}

fn save_error(e: SaveError) -> anyhow::Error {
    anyhow::Error::new(e)
}

pub fn content_cmd(cmd: ContentCmd) -> Result<()> {
    let site = match &cmd {
        ContentCmd::List { site, .. }
        | ContentCmd::Show { site, .. }
        | ContentCmd::Create { site, .. }
        | ContentCmd::Set { site, .. }
        | ContentCmd::Delete { site, .. }
        | ContentCmd::Revisions { site, .. }
        | ContentCmd::Terms { site, .. }
        | ContentCmd::Seed { site, .. } => site.site.clone(),
    };
    let conn = connect(&site)?;
    let app = conn.app()?;
    let rt = &conn.rt;
    let store = &app.store;
    match cmd {
        ContentCmd::List {
            type_name,
            all,
            limit,
            order,
            ..
        } => {
            let items = rt.block_on(store.list(
                &type_name,
                &ListQuery {
                    status: (!all).then_some(Status::Published),
                    limit,
                    order,
                    ..Default::default()
                },
            ))?;
            println!(
                "{:>5}  {:<32} {:<9} {:>3}  title",
                "id", "slug", "status", "v"
            );
            for i in &items {
                println!(
                    "{:>5}  {:<32} {:<9} {:>3}  {}",
                    i.id,
                    i.slug,
                    i.status.as_str(),
                    i.version,
                    title_of(&app, i)
                );
            }
            println!(
                "({} item{})",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            );
        }
        ContentCmd::Show { type_name, key, .. } => {
            let item = find(&app, &type_name, key.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&item)?);
        }
        ContentCmd::Create {
            type_name,
            set,
            slug,
            publish,
            author,
            ..
        } => {
            let changes = Changes {
                fields: parse_sets(store.registry(), &type_name, &set)?,
                slug,
                status: Some(if publish {
                    Status::Published
                } else {
                    Status::Draft
                }),
            };
            let item = rt
                .block_on(store.create(&type_name, &changes, Some(&author)))
                .map_err(save_error)?;
            saved(&app, "created", &item);
        }
        ContentCmd::Set {
            type_name,
            key,
            set,
            slug,
            publish,
            unpublish,
            expect_version,
            author,
            ..
        } => {
            let t = store.registry().require(&type_name)?.clone();
            let changes = Changes {
                fields: parse_sets(store.registry(), &type_name, &set)?,
                slug,
                status: if publish {
                    Some(Status::Published)
                } else if unpublish {
                    Some(Status::Draft)
                } else {
                    None
                },
            };
            let existing = if key.is_none() && t.singleton {
                rt.block_on(store.singleton(&type_name))?
            } else {
                Some(find(&app, &type_name, key.as_deref())?)
            };
            let item = match existing {
                Some(cur) => rt
                    .block_on(store.update(
                        &type_name,
                        cur.id,
                        expect_version.unwrap_or(cur.version),
                        &changes,
                        Some(&author),
                    ))
                    .map_err(save_error)?,
                None => rt
                    .block_on(store.create(&type_name, &changes, Some(&author)))
                    .map_err(save_error)?,
            };
            saved(&app, "saved", &item);
        }
        ContentCmd::Delete {
            type_name,
            key,
            expect_version,
            author,
            ..
        } => {
            let cur = find(&app, &type_name, Some(&key))?;
            rt.block_on(store.delete(&type_name, cur.id, expect_version, Some(&author)))
                .map_err(save_error)?;
            println!("deleted {type_name} #{} `{}`", cur.id, cur.slug);
        }
        ContentCmd::Revisions { type_name, key, .. } => {
            let cur = find(&app, &type_name, key.as_deref())?;
            for r in rt.block_on(store.revisions(&type_name, cur.id))? {
                println!(
                    "v{:<4} {:<7} {}  by {}",
                    r.version,
                    r.op,
                    r.created_at,
                    r.author.as_deref().unwrap_or("-")
                );
            }
        }
        ContentCmd::Terms { vocabulary, .. } => {
            for t in rt.block_on(store.terms(&vocabulary))? {
                println!("{:>5}  {:<24} {}", t.id, t.slug, t.name);
            }
        }
        ContentCmd::Seed { author, .. } => {
            let report =
                rt.block_on(engine_content::seed::seed_dir(store, &site, Some(&author)))?;
            for (t, s) in &report.created {
                println!("created {t} `{s}`");
            }
            println!(
                "Seeded {} item(s); {} already existed.",
                report.created.len(),
                report.skipped.len()
            );
        }
    }
    Ok(())
}

/// `engine build` for a site with database content. `Ok(None)` if the site
/// has none (the plain build applies).
pub fn build(site: &Path, opts: &BuildOptions) -> Result<Option<BuildReport>> {
    let (cfg, _) = builder::load_config(site)?;
    if !content_site::uses_content(site, &cfg) {
        return Ok(None);
    }
    let conn = connect(site)?;
    #[cfg(feature = "publish")]
    {
        let cms = conn.rt.block_on(engine_publish::CmsSite::open(
            site,
            conn.db.clone(),
            conn.rt.handle().clone(),
        ))?;
        Ok(Some(cms.build(opts)?))
    }
    #[cfg(not(feature = "publish"))]
    {
        let app = conn.app()?;
        Ok(Some(builder::build_site_with(
            site,
            opts,
            Some(app.extension()),
        )?))
    }
}

/// A build function for `engine dev` that keeps one connection open.
pub type DevBuild = Box<dyn Fn(&Path, &BuildOptions) -> Result<BuildReport> + Send + Sync>;

pub fn dev_builder(site: &Path) -> Result<Option<DevBuild>> {
    let (cfg, _) = builder::load_config(site)?;
    if !content_site::uses_content(site, &cfg) {
        return Ok(None);
    }
    let conn = connect(site)?;
    Ok(Some(Box::new(move |site: &Path, opts: &BuildOptions| {
        // Reopen per build so edited type files and templates are picked up.
        #[cfg(feature = "publish")]
        {
            let cms = conn.rt.block_on(engine_publish::CmsSite::open(
                site,
                conn.db.clone(),
                conn.rt.handle().clone(),
            ))?;
            cms.build(opts)
        }
        #[cfg(not(feature = "publish"))]
        {
            let app = conn.app()?;
            builder::build_site_with(site, opts, Some(app.extension()))
        }
    })))
}

/// `engine rebuild [--tag T]...`.
#[cfg(feature = "publish")]
pub fn rebuild(site: &Path, tags: &[String]) -> Result<()> {
    use engine_core::tags::CacheTag;
    let tags: Vec<CacheTag> = tags
        .iter()
        .map(|t| t.parse::<CacheTag>().map_err(anyhow::Error::from))
        .collect::<Result<_>>()?;
    let conn = connect(site)?;
    let cms = conn.rt.block_on(engine_publish::CmsSite::open(
        site,
        conn.db.clone(),
        conn.rt.handle().clone(),
    ))?;
    let publisher = cms.publisher()?;
    let report = if tags.is_empty() {
        publisher.rebuild_all()?
    } else {
        publisher.rebuild_tags(&tags)?
    };
    println!("{}", report.summary());
    if !report.errors.is_empty() {
        bail!("{} page(s) failed to render", report.errors.len());
    }
    Ok(())
}
