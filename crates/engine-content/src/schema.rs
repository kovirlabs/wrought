//! `engine types sync`: generate migrations from `types/*.toml`.
//!
//! The last synced shape of every type is kept in
//! `migrations/types-snapshot.json` (commit it with the migrations). Sync
//! compares the type files with the snapshot and writes one new migration
//! with the difference:
//!
//! * new type: `CREATE TABLE content_<type>` with the built-in columns
//!   (`id`, `slug`, `status`, `version`, timestamps), one column per field,
//!   and the change trigger that feeds publishing;
//! * new field: `ALTER TABLE ... ADD COLUMN`;
//! * removed field or type: `DROP` (only with `--allow-drop`, as it deletes
//!   data);
//! * changed field type or relation target: refused; write that migration by
//!   hand (it usually needs a data conversion) and update the snapshot with
//!   `engine types sync --snapshot-only`.
//!
//! Field columns are nullable; `required` is enforced by validation when
//! saving, so adding a required field to a type with existing rows works.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::slug;
use crate::types::{FieldKind, Registry, TypeDef, table_name};

pub const SNAPSHOT_FILE: &str = "types-snapshot.json";

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub types: BTreeMap<String, TypeSnap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeSnap {
    pub singleton: bool,
    pub columns: BTreeMap<String, ColumnSnap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSnap {
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// The database shape the type files describe.
/// Snapshot format: 2 added the published-list index of every type.
pub const SNAPSHOT_FORMAT: u32 = 2;

pub fn snapshot(reg: &Registry) -> Snapshot {
    Snapshot {
        format: SNAPSHOT_FORMAT,
        types: reg
            .types()
            .map(|t| {
                (
                    t.name.clone(),
                    TypeSnap {
                        singleton: t.singleton,
                        columns: t
                            .columns()
                            .map(|f| {
                                (
                                    f.name.clone(),
                                    ColumnSnap {
                                        kind: f.kind,
                                        to: f.to.clone(),
                                    },
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect(),
    }
}

pub fn snapshot_path(site: &Path) -> PathBuf {
    site.join("migrations").join(SNAPSHOT_FILE)
}

pub fn load_snapshot(site: &Path) -> Result<Snapshot> {
    let p = snapshot_path(site);
    if !p.is_file() {
        return Ok(Snapshot {
            format: 1,
            ..Default::default()
        });
    }
    let text = std::fs::read_to_string(&p)?;
    serde_json::from_str(&text).with_context(|| format!("reading {}", p.display()))
}

fn save_snapshot(site: &Path, snap: &Snapshot) -> Result<()> {
    let p = snapshot_path(site);
    std::fs::create_dir_all(p.parent().expect("has parent"))?;
    let mut text = serde_json::to_string_pretty(snap)?;
    text.push('\n');
    std::fs::write(&p, text).with_context(|| format!("writing {}", p.display()))
}

/// Error unless the type files match the last `engine types sync`.
///
/// Only the types are compared: a snapshot of an older format (missing
/// indexes) still matches, and `engine types sync` upgrades it.
pub fn ensure_synced(site: &Path, reg: &Registry) -> Result<()> {
    if load_snapshot(site)?.types != snapshot(reg).types {
        bail!(
            "the type files in types/ changed since the last `engine types sync`; run \
             `engine types sync` and `engine migrate`"
        );
    }
    Ok(())
}

/// The SQL of a planned migration and a one-line summary per change.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub sql: Vec<String>,
    pub summary: Vec<String>,
    /// Changes that delete data (need `--allow-drop`).
    pub destructive: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
    }
}

fn sql_type(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Terms => unreachable!("terms have no column"),
        FieldKind::Integer | FieldKind::Relation | FieldKind::Media => "bigint",
        FieldKind::Boolean => "boolean",
        FieldKind::Date => "date",
        FieldKind::Blocks => "jsonb",
        _ => "text",
    }
}

/// Column definition for a field (relations get their foreign keys after
/// every table exists, see [`foreign_key`]).
fn column_def(f: &crate::types::FieldDef) -> String {
    format!("\"{}\" {}", f.name, sql_type(f.kind))
}

fn create_table(t: &TypeDef) -> String {
    let mut sql = create_table_pg(t);
    sql.extend(list_index(t));
    sql
}

fn create_table_pg(t: &TypeDef) -> String {
    let table = t.table();
    let mut cols = vec![
        "    id bigserial PRIMARY KEY".to_string(),
        format!(
            "    slug text NOT NULL UNIQUE CHECK (slug ~ '{}' AND length(slug) <= {})",
            slug::SQL_PATTERN,
            slug::MAX_LEN
        ),
        "    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published'))"
            .to_string(),
        "    version integer NOT NULL DEFAULT 1".to_string(),
        "    created_at timestamptz NOT NULL DEFAULT now()".to_string(),
        "    updated_at timestamptz NOT NULL DEFAULT now()".to_string(),
        "    published_at timestamptz".to_string(),
    ];
    for f in t.columns() {
        cols.push(format!("    {}", column_def(f)));
    }
    let mut sql = format!("CREATE TABLE {table} (\n{}\n);\n", cols.join(",\n"));
    if t.singleton {
        sql.push_str(&format!(
            "-- Singleton: at most one row.\nCREATE UNIQUE INDEX {table}_one ON {table} ((true));\n"
        ));
    }
    sql.push_str(&format!(
        "CREATE TRIGGER {table}_changes\n    AFTER INSERT OR UPDATE OR DELETE ON {table}\n    \
         FOR EACH ROW EXECUTE FUNCTION engine_record_change('{}');\n",
        t.name
    ));
    sql
}

/// The index behind every published listing (`list_items`, list routes,
/// pattern routes): published rows only (a partial index), in the default
/// `newest` order, so a page of a list reads a few index entries instead of
/// sorting the table. The expression matches the `ORDER BY` of the store's
/// list query exactly, or the planner would not use it.
fn list_index(t: &TypeDef) -> Option<String> {
    if t.singleton {
        return None;
    }
    let table = t.table();
    Some(format!(
        "CREATE INDEX IF NOT EXISTS {table}_published_newest ON {table} \
         ((COALESCE(published_at, created_at)) DESC, id DESC) WHERE status = 'published';\n"
    ))
}

fn foreign_key(type_name: &str, field: &str, to: &str) -> String {
    let table = table_name(type_name);
    format!(
        "ALTER TABLE {table} ADD CONSTRAINT {table}_{field}_fk FOREIGN KEY (\"{field}\")\n    \
         REFERENCES {} (id) ON DELETE SET NULL;\n",
        table_name(to)
    )
}

/// Plan the migration from `old` to the registry's shape.
pub fn plan(old: &Snapshot, reg: &Registry) -> Result<Plan> {
    let new = snapshot(reg);
    let mut p = Plan::default();
    let mut fks = Vec::new();

    for t in reg.types() {
        match old.types.get(&t.name) {
            None => {
                p.sql.push(create_table(t));
                p.summary.push(format!("create type `{}`", t.name));
                for f in t.columns().filter(|f| f.kind == FieldKind::Relation) {
                    fks.push(foreign_key(&t.name, &f.name, f.to.as_deref().unwrap_or("")));
                }
            }
            Some(o) => {
                if old.format < 2
                    && let Some(ix) = list_index(t)
                {
                    p.sql.push(ix);
                    p.summary.push(format!("index published `{}`", t.name));
                }
                if o.singleton != t.singleton {
                    bail!(
                        "type `{}` changed `singleton`; that needs a hand-written migration",
                        t.name
                    );
                }
                let table = t.table();
                for f in t.columns() {
                    match o.columns.get(&f.name) {
                        None => {
                            p.sql.push(format!(
                                "ALTER TABLE {table} ADD COLUMN {};\n",
                                column_def(f)
                            ));
                            p.summary.push(format!("add `{}.{}`", t.name, f.name));
                            if f.kind == FieldKind::Relation {
                                fks.push(foreign_key(
                                    &t.name,
                                    &f.name,
                                    f.to.as_deref().unwrap_or(""),
                                ));
                            }
                        }
                        Some(c) if c.kind != f.kind || c.to != f.to => bail!(
                            "field `{}.{}` changed from {} to {}; write that migration by hand \
                             (it usually converts data), then run `engine types sync \
                             --snapshot-only`",
                            t.name,
                            f.name,
                            c.kind.as_str(),
                            f.kind.as_str()
                        ),
                        Some(_) => {}
                    }
                }
                for name in o.columns.keys() {
                    if t.field(name).is_none_or(|f| !f.kind.has_column()) {
                        p.sql
                            .push(format!("ALTER TABLE {table} DROP COLUMN \"{name}\";\n"));
                        let s = format!("drop `{}.{name}`", t.name);
                        p.destructive.push(s.clone());
                        p.summary.push(s);
                    }
                }
            }
        }
    }
    for name in old.types.keys() {
        if !new.types.contains_key(name) {
            p.sql.push(format!(
                "DROP TABLE {} CASCADE;\nDELETE FROM engine_item_terms WHERE content_type = '{name}';\n",
                table_name(name)
            ));
            let s = format!("drop type `{name}`");
            p.destructive.push(s.clone());
            p.summary.push(s);
        }
    }
    p.sql.extend(fks);
    Ok(p)
}

/// File-name words from a plan summary: the names in backticks
/// (`create type `article`` → `article`, `add `author.bio`` → `author_bio`),
/// cut at a word boundary.
fn file_words(summary: &[String]) -> String {
    let mut words: Vec<String> = Vec::new();
    for line in summary {
        for (i, part) in line.split('`').enumerate() {
            if i % 2 == 1 {
                let w = part.replace('.', "_");
                if !words.contains(&w) {
                    words.push(w);
                }
            }
        }
    }
    let mut out = String::new();
    for w in words {
        if !out.is_empty() && out.len() + 1 + w.len() > 48 {
            out.push_str("_more");
            break;
        }
        if !out.is_empty() {
            out.push('_');
        }
        out.push_str(&w);
    }
    if out.is_empty() { "change".into() } else { out }
}

/// What `engine types sync` did.
#[derive(Debug)]
pub struct SyncOutcome {
    /// The change.
    pub plan: Plan,
    /// The migration written (none when already in sync or `check` only).
    pub files: Vec<PathBuf>,
}

/// Options for [`sync`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions {
    /// Only report; write nothing.
    pub check: bool,
    pub allow_drop: bool,
    /// Record the current types as synced without writing a migration.
    pub snapshot_only: bool,
}

/// Write the migration for the change into `migrations/postgres/`. A site
/// whose folder has no migrations yet gets the whole schema.
pub fn sync(site: &Path, reg: &Registry, opts: SyncOptions) -> Result<SyncOutcome> {
    if opts.snapshot_only {
        if !opts.check {
            save_snapshot(site, &snapshot(reg))?;
        }
        return Ok(SyncOutcome {
            plan: Plan::default(),
            files: Vec::new(),
        });
    }
    let old = load_snapshot(site)?;
    let p = plan(&old, reg)?;
    if opts.check {
        return Ok(SyncOutcome {
            plan: p,
            files: Vec::new(),
        });
    }
    if !p.destructive.is_empty() && !opts.allow_drop {
        bail!(
            "these changes delete data: {}. Re-run with --allow-drop if that is intended",
            p.destructive.join(", ")
        );
    }
    let existing = engine_db::migrate::site_migrations(site)?;
    let written = if existing.is_empty() {
        plan(
            &Snapshot {
                format: 1,
                ..Default::default()
            },
            reg,
        )?
    } else {
        p.clone()
    };
    let mut files = Vec::new();
    if !written.is_empty() {
        let dir = engine_db::migrate::site_dir(site);
        std::fs::create_dir_all(&dir)?;
        let next = existing
            .iter()
            .filter_map(|m| m.id.rsplit('/').next()?.get(..4)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let words = file_words(&written.summary);
        let file = dir.join(format!("{next:04}_types_{words}.sql"));
        let mut text = format!(
            "-- Generated by `engine types sync` from types/*.toml (postgres).\n-- {}\n\n",
            written.summary.join("; ")
        );
        text.push_str(&written.sql.join("\n"));
        std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
        files.push(file);
    }
    save_snapshot(site, &snapshot(reg))?;
    Ok(SyncOutcome { plan: p, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tests::{sample_site, write};

    #[test]
    fn migration_file_words() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(file_words(&s(&["add `a.b`", "drop `c.d`"])), "a_b_c_d");
        let long = file_words(&s(&[
            "create type `aaaaaaaaaaaaaaaaaaaa`",
            "create type `bbbbbbbbbbbbbbbbbbbb`",
            "create type `cccccccccccccccccccc`",
        ]));
        assert_eq!(long, "aaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbb_more");
        assert!(engine_db::migrate::valid_file_name(&format!(
            "0001_types_{long}.sql"
        )));
    }

    #[test]
    fn first_sync_creates_tables_then_is_idempotent() {
        let d = sample_site();
        let reg = Registry::load(d.path()).unwrap();
        let out = sync(d.path(), &reg, SyncOptions::default()).unwrap();
        assert_eq!(out.files.len(), 1);
        let file = out.files[0].clone();
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "0001_types_article_author_settings.sql");
        assert!(file.parent().unwrap().ends_with("migrations/postgres"));
        let sql = std::fs::read_to_string(&file).unwrap();
        assert!(sql.contains("CREATE TABLE content_article ("), "{sql}");
        assert!(sql.contains("\"published_on\" date"), "{sql}");
        assert!(sql.contains("\"sections\" jsonb"), "{sql}");
        assert!(!sql.contains("\"topics\""), "terms have no column: {sql}");
        assert!(sql.contains("engine_record_change('article')"), "{sql}");
        assert!(sql.contains("REFERENCES content_author (id)"), "{sql}");
        assert!(sql.contains("from types/*.toml (postgres)"), "{sql}");
        assert!(
            sql.contains("CREATE UNIQUE INDEX content_settings_one"),
            "{sql}"
        );
        // FKs come after every CREATE TABLE.
        let fk = sql.find("FOREIGN KEY (\"author\")").unwrap();
        assert!(fk > sql.find("CREATE TABLE content_author").unwrap());
        assert!(snapshot_path(d.path()).is_file());
        ensure_synced(d.path(), &reg).unwrap();

        let again = sync(d.path(), &reg, SyncOptions::default()).unwrap();
        assert!(again.plan.is_empty() && again.files.is_empty());

        // Add a field: an ALTER, numbered 0002.
        let text = std::fs::read_to_string(d.path().join("types/author.toml")).unwrap();
        write(
            d.path(),
            "types/author.toml",
            &format!("{text}\n[[fields]]\nname='bio'\ntype='markdown'\n"),
        );
        let reg = Registry::load(d.path()).unwrap();
        assert!(ensure_synced(d.path(), &reg).is_err());
        let check = sync(
            d.path(),
            &reg,
            SyncOptions {
                check: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(check.plan.summary, ["add `author.bio`"]);
        assert!(check.files.is_empty());
        let out = sync(d.path(), &reg, SyncOptions::default()).unwrap();
        assert_eq!(out.files.len(), 1);
        let name = out.files[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(name, "0002_types_author_bio.sql");

        // Removing it needs --allow-drop.
        write(d.path(), "types/author.toml", &text);
        let reg = Registry::load(d.path()).unwrap();
        let err = sync(d.path(), &reg, SyncOptions::default()).unwrap_err();
        assert!(err.to_string().contains("--allow-drop"), "{err}");
        let out = sync(
            d.path(),
            &reg,
            SyncOptions {
                allow_drop: true,
                ..Default::default()
            },
        )
        .unwrap();
        let sql = std::fs::read_to_string(&out.files[0]).unwrap();
        assert!(sql.contains("DROP COLUMN \"bio\""), "{sql}");

        // Changing a field's type is refused.
        let art = std::fs::read_to_string(d.path().join("types/article.toml")).unwrap();
        write(
            d.path(),
            "types/article.toml",
            &art.replace(
                "name = \"rating\"\ntype = \"integer\"",
                "name = \"rating\"\ntype = \"text\"",
            )
            .replace("min = 1\nmax = 5\n", ""),
        );
        let reg = Registry::load(d.path()).unwrap();
        let err = sync(d.path(), &reg, SyncOptions::default()).unwrap_err();
        assert!(err.to_string().contains("by hand"), "{err}");
    }
}
