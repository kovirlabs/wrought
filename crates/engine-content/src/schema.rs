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
use engine_db::Backend;
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

fn sql_type(kind: FieldKind, backend: Backend) -> &'static str {
    match (backend, kind) {
        (_, FieldKind::Terms) => unreachable!("terms have no column"),
        (Backend::Postgres, k) => match k {
            FieldKind::Integer | FieldKind::Relation | FieldKind::Media => "bigint",
            FieldKind::Boolean => "boolean",
            FieldKind::Date => "date",
            FieldKind::Blocks => "jsonb",
            _ => "text",
        },
        (Backend::Sqlite, k) => match k {
            FieldKind::Integer | FieldKind::Relation | FieldKind::Media => "INTEGER",
            FieldKind::Boolean => "BOOLEAN",
            // ISO dates as text; the check keeps them in that shape.
            FieldKind::Date => "TEXT",
            FieldKind::Blocks => "TEXT",
            _ => "TEXT",
        },
    }
}

/// Column definition for a field (SQLite adds its constraints inline).
fn column_def(f: &crate::types::FieldDef, backend: Backend) -> String {
    let mut def = format!("\"{}\" {}", f.name, sql_type(f.kind, backend));
    if backend == Backend::Sqlite {
        match f.kind {
            FieldKind::Relation => def.push_str(&format!(
                " REFERENCES {} (id) ON DELETE SET NULL",
                table_name(f.to.as_deref().unwrap_or(""))
            )),
            FieldKind::Date => def.push_str(&format!(
                " CHECK (\"{0}\" IS NULL OR date(\"{0}\") = \"{0}\")",
                f.name
            )),
            FieldKind::Blocks => def.push_str(&format!(
                " CHECK (\"{0}\" IS NULL OR json_valid(\"{0}\"))",
                f.name
            )),
            _ => {}
        }
    }
    def
}

fn create_table(t: &TypeDef, backend: Backend) -> String {
    let mut sql = match backend {
        Backend::Postgres => create_table_pg(t),
        Backend::Sqlite => create_table_sqlite(t),
    };
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
        cols.push(format!("    {}", column_def(f, Backend::Postgres)));
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

const LITE_NOW: &str = "(strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))";

fn create_table_sqlite(t: &TypeDef) -> String {
    let table = t.table();
    let mut cols = vec![
        "    id INTEGER PRIMARY KEY AUTOINCREMENT".to_string(),
        format!(
            "    slug TEXT NOT NULL UNIQUE CHECK (length(slug) BETWEEN 1 AND {} \
             AND slug NOT GLOB '*[^a-z0-9-]*' AND slug NOT GLOB '-*' AND slug NOT GLOB '*-' \
             AND slug NOT GLOB '*--*')",
            slug::MAX_LEN
        ),
        "    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published'))"
            .to_string(),
        "    version INTEGER NOT NULL DEFAULT 1".to_string(),
        format!("    created_at TEXT NOT NULL DEFAULT {LITE_NOW}"),
        format!("    updated_at TEXT NOT NULL DEFAULT {LITE_NOW}"),
        "    published_at TEXT".to_string(),
    ];
    for f in t.columns() {
        cols.push(format!("    {}", column_def(f, Backend::Sqlite)));
    }
    let mut sql = format!("CREATE TABLE {table} (\n{}\n);\n", cols.join(",\n"));
    if t.singleton {
        sql.push_str(&format!(
            "-- Singleton: at most one row.\nCREATE TRIGGER {table}_one BEFORE INSERT ON {table}\n    \
             WHEN (SELECT count(*) FROM {table}) > 0\n    \
             BEGIN SELECT RAISE(ABORT, '{} is a singleton'); END;\n",
            t.name
        ));
    }
    // The change log (no NOTIFY on SQLite: the publisher polls, watches the
    // file and is woken in-process).
    for (ev, row, op) in [
        ("INSERT", "NEW", "INSERT"),
        ("UPDATE", "NEW", "UPDATE"),
        ("DELETE", "OLD", "DELETE"),
    ] {
        sql.push_str(&format!(
            "CREATE TRIGGER {table}_changes_{0} AFTER {ev} ON {table} BEGIN\n    \
             INSERT INTO engine_changes (content_type, item_id, op) VALUES ('{1}', {row}.id, '{op}');\n\
             END;\n",
            op.to_lowercase(),
            t.name
        ));
    }
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
pub fn plan(old: &Snapshot, reg: &Registry, backend: Backend) -> Result<Plan> {
    let new = snapshot(reg);
    let pg = backend == Backend::Postgres;
    let mut p = Plan::default();
    let mut fks = Vec::new();

    for t in reg.types() {
        match old.types.get(&t.name) {
            None => {
                p.sql.push(create_table(t, backend));
                p.summary.push(format!("create type `{}`", t.name));
                for f in t.columns().filter(|f| pg && f.kind == FieldKind::Relation) {
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
                                column_def(f, backend)
                            ));
                            p.summary.push(format!("add `{}.{}`", t.name, f.name));
                            if pg && f.kind == FieldKind::Relation {
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
                "DROP TABLE {}{};\nDELETE FROM engine_item_terms WHERE content_type = '{name}';\n",
                table_name(name),
                if pg { " CASCADE" } else { "" }
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
    /// The change (the same for every backend).
    pub plan: Plan,
    /// The migrations written, one per backend (none when already in sync
    /// or `check` only).
    pub files: Vec<PathBuf>,
}

impl SyncOutcome {
    /// The migration written for `backend`.
    pub fn file(&self, backend: Backend) -> Option<&PathBuf> {
        self.files
            .iter()
            .find(|f| f.parent().is_some_and(|d| d.ends_with(backend.as_str())))
    }
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

/// Write one migration per backend (`migrations/postgres/` and
/// `migrations/sqlite/`), so a site can move between backends. A backend
/// whose folder has no migrations yet gets the whole schema (a site made
/// before SQLite support gains its SQLite migrations on its next sync).
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
    let main = plan(&old, reg, Backend::Postgres)?;
    if opts.check {
        return Ok(SyncOutcome {
            plan: main,
            files: Vec::new(),
        });
    }
    if !main.destructive.is_empty() && !opts.allow_drop {
        bail!(
            "these changes delete data: {}. Re-run with --allow-drop if that is intended",
            main.destructive.join(", ")
        );
    }
    let empty = Snapshot {
        format: 1,
        ..Default::default()
    };
    let mut files = Vec::new();
    for backend in Backend::ALL {
        let existing = engine_db::migrate::site_migrations(site, backend)?;
        let base = if existing.is_empty() { &empty } else { &old };
        let p = plan(base, reg, backend)?;
        if p.is_empty() {
            continue;
        }
        let dir = engine_db::migrate::site_dir(site, backend);
        std::fs::create_dir_all(&dir)?;
        let next = existing
            .iter()
            .filter_map(|m| m.id.rsplit('/').next()?.get(..4)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let words = file_words(&p.summary);
        let file = dir.join(format!("{next:04}_types_{words}.sql"));
        let mut text = format!(
            "-- Generated by `engine types sync` from types/*.toml ({backend}).\n-- {}\n\n",
            p.summary.join("; ")
        );
        text.push_str(&p.sql.join("\n"));
        std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
        files.push(file);
    }
    save_snapshot(site, &snapshot(reg))?;
    Ok(SyncOutcome { plan: main, files })
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
        assert_eq!(out.files.len(), 2);
        let lite = std::fs::read_to_string(out.file(Backend::Sqlite).unwrap()).unwrap();
        assert!(lite.contains("CREATE TABLE content_article ("), "{lite}");
        assert!(lite.contains("REFERENCES content_author (id)"), "{lite}");
        assert!(lite.contains("content_article_changes_insert"), "{lite}");
        assert!(
            lite.contains("content_settings_one BEFORE INSERT"),
            "{lite}"
        );
        let file = out.file(Backend::Postgres).unwrap().clone();
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "0001_types_article_author_settings.sql");
        assert!(file.parent().unwrap().ends_with("migrations/postgres"));
        let sql = std::fs::read_to_string(&file).unwrap();
        assert!(sql.contains("CREATE TABLE content_article ("), "{sql}");
        assert!(sql.contains("\"published_on\" date"), "{sql}");
        assert!(sql.contains("\"sections\" jsonb"), "{sql}");
        assert!(!sql.contains("\"topics\""), "terms have no column: {sql}");
        assert!(sql.contains("engine_record_change('article')"), "{sql}");
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
        assert_eq!(out.files.len(), 2);
        let name = out
            .file(Backend::Postgres)
            .unwrap()
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
        let sql = std::fs::read_to_string(out.file(Backend::Postgres).unwrap()).unwrap();
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
