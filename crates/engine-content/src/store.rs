//! Reading and writing content items.
//!
//! Every item row has a `version`. Updates name the version they edited and
//! fail with [`SaveError::Conflict`] if the row moved on since (optimistic
//! locking). Each create, update and delete writes its revision row in the
//! same transaction as the change, so there is never a save without a
//! revision or a revision without a save.
//!
//! Queries on the engine's fixed tables (`engine_revisions`, `engine_terms`,
//! `engine_item_terms`) are statements in [`crate::sql`], checked against
//! both backends by the catalogue test. Queries on `content_<type>` tables
//! are built at run time from the type files: their table and column names
//! are only known per site. Names are validated identifiers and all values
//! are bound parameters. The same code runs on PostgreSQL and SQLite; the
//! few dialect differences are marked where the SQL is built.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use engine_db::Db;

use crate::slug;
use crate::types::{FieldDef, FieldKind, Registry, TypeDef};
use crate::value::{self, RelRef, ValidationErrors, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Draft,
    Published,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Published => "published",
        }
    }
    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "draft" => Some(Status::Draft),
            "published" => Some(Status::Published),
            _ => None,
        }
    }
}

/// One stored item, with raw field values (Markdown unrendered). `terms`
/// fields hold `[{id, slug, name}]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    #[serde(rename = "type")]
    pub content_type: String,
    pub id: i64,
    pub slug: String,
    pub status: Status,
    pub version: i32,
    pub created_at: String,
    pub updated_at: String,
    pub published_at: Option<String>,
    pub fields: Map<String, Json>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Term {
    pub id: i64,
    pub vocabulary: String,
    pub slug: String,
    pub name: String,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub id: i64,
    pub version: i32,
    pub op: String,
    pub author: Option<String>,
    pub created_at: String,
    pub data: Json,
}

/// How to find an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Id(i64),
    Slug(String),
}

impl Key {
    /// Digits are an id, anything else a slug.
    pub fn parse(s: &str) -> Key {
        match s.parse::<i64>() {
            Ok(i) => Key::Id(i),
            Err(_) => Key::Slug(s.to_string()),
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Id(i) => write!(f, "#{i}"),
            Key::Slug(s) => write!(f, "`{s}`"),
        }
    }
}

/// Changes to save: field values (raw input, validated by the store), and
/// optionally a new slug or status.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    pub fields: Vec<(String, Json)>,
    pub slug: Option<String>,
    pub status: Option<Status>,
}

impl Changes {
    pub fn set(mut self, field: &str, v: impl Into<Json>) -> Self {
        self.fields.push((field.to_string(), v.into()));
        self
    }
    pub fn status(mut self, s: Status) -> Self {
        self.status = Some(s);
        self
    }
}

/// What [`Store::list`] returns.
#[derive(Debug, Clone)]
pub struct ListQuery {
    /// `None`: any status.
    pub status: Option<Status>,
    pub limit: Option<i64>,
    pub offset: i64,
    /// `newest` (default), `oldest`, `updated`, `title`, `<field>` or
    /// `-<field>` (descending).
    pub order: String,
    /// Only items with this term: `rust`, or `topics/rust` when the type has
    /// several terms fields.
    pub term: Option<String>,
    /// Only items whose title (or slug) contains this text, ignoring case.
    pub search: Option<String>,
}

impl Default for ListQuery {
    fn default() -> Self {
        ListQuery {
            status: Some(Status::Published),
            limit: None,
            offset: 0,
            order: "newest".into(),
            term: None,
            search: None,
        }
    }
}

impl Item {
    /// The item's values as save input (what a form or `Changes` holds):
    /// terms as names, relations as ids. Used to restore a revision.
    pub fn input_fields(&self, t: &TypeDef) -> Vec<(String, Json)> {
        t.fields
            .iter()
            .map(|f| {
                let raw = self.fields.get(&f.name).cloned().unwrap_or(Json::Null);
                let v = match f.kind {
                    FieldKind::Terms => Json::Array(
                        raw.as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|t| match t {
                                Json::String(s) => Some(Json::String(s.clone())),
                                other => other.get("name").cloned(),
                            })
                            .collect(),
                    ),
                    _ => raw,
                };
                (f.name.clone(), v)
            })
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Invalid(#[from] ValidationErrors),
    #[error(
        "{content_type} #{id} was changed by someone else: you edited version {expected}, but \
         it is now at version {current}. Reload it and apply your change again"
    )]
    Conflict {
        content_type: String,
        id: i64,
        expected: i32,
        current: i32,
    },
    #[error("no {content_type} {key}")]
    NotFound { content_type: String, key: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for SaveError {
    fn from(e: sqlx::Error) -> Self {
        SaveError::Other(e.into())
    }
}

/// Access to a site's content.
#[derive(Clone)]
pub struct Store {
    db: Db,
    reg: Arc<Registry>,
}

/// Validated field values of one save, with relations resolved later.
struct Validated {
    values: Vec<(FieldDef, Value)>,
}

impl Store {
    pub fn new(db: Db, reg: Arc<Registry>) -> Self {
        Store { db, reg }
    }

    pub fn registry(&self) -> &Registry {
        &self.reg
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    fn validate(
        &self,
        t: &TypeDef,
        ch: &Changes,
        creating: bool,
    ) -> Result<Validated, ValidationErrors> {
        let mut errs = ValidationErrors::default();
        let mut values = Vec::new();
        for (name, _) in &ch.fields {
            if t.field(name).is_none() {
                errs.push(name, format!("is not a field of `{}`", t.name));
            }
            if ch.fields.iter().filter(|(n, _)| n == name).count() > 1 {
                errs.push(name, "is given twice");
            }
        }
        for f in &t.fields {
            let given = ch.fields.iter().find(|(n, _)| n == &f.name).map(|(_, v)| v);
            let input = match (given, creating) {
                (Some(v), _) => v,
                (None, true) => &Json::Null,
                (None, false) => continue,
            };
            match value::validate(&self.reg, f, input) {
                Ok(v) => values.push((f.clone(), v)),
                Err(m) => errs.push(&f.name, m),
            }
        }
        if let Some(s) = &ch.slug
            && !slug::is_valid(s)
        {
            errs.push(
                "slug",
                "must be lowercase letters and digits in words joined by `-` (at most 80)",
            );
        }
        if errs.is_empty() {
            errs.0.dedup();
            Ok(Validated { values })
        } else {
            errs.0.dedup();
            Err(errs)
        }
    }

    /// List items of a type.
    pub async fn list(&self, type_name: &str, q: &ListQuery) -> anyhow::Result<Vec<Item>> {
        let t = self.reg.require(type_name)?;
        imp::list(&self.db, t, q).await
    }

    /// How many items [`Store::list`] would return without limit/offset.
    pub async fn count(&self, type_name: &str, q: &ListQuery) -> anyhow::Result<i64> {
        let t = self.reg.require(type_name)?;
        imp::count(&self.db, t, q).await
    }

    /// The revision of an item at `version`, if it exists.
    pub async fn revision(
        &self,
        type_name: &str,
        id: i64,
        version: i32,
    ) -> anyhow::Result<Option<Revision>> {
        Ok(self
            .revisions(type_name, id)
            .await?
            .into_iter()
            .find(|r| r.version == version))
    }

    /// Rename a taxonomy term the caller read at `expected_version`. Its
    /// slug (and URL) stays.
    pub async fn rename_term(
        &self,
        vocabulary: &str,
        id: i64,
        expected_version: i32,
        name: &str,
    ) -> Result<Term, SaveError> {
        let name = name.trim();
        let mut errs = ValidationErrors::default();
        if name.is_empty() {
            errs.push("name", "is required");
        } else if name.chars().count() > 100 {
            errs.push("name", "is longer than 100 characters");
        }
        if !errs.is_empty() {
            return Err(errs.into());
        }
        let t = imp::rename_term(&self.db, vocabulary, id, expected_version, name).await?;
        self.db.notify_changed();
        Ok(t)
    }

    /// Delete a taxonomy term (and its assignments) if it is unchanged.
    pub async fn delete_term(
        &self,
        vocabulary: &str,
        id: i64,
        expected_version: i32,
    ) -> Result<(), SaveError> {
        imp::delete_term(&self.db, vocabulary, id, expected_version).await?;
        self.db.notify_changed();
        Ok(())
    }

    /// One item by id or slug; `status: None` finds drafts too.
    pub async fn get(
        &self,
        type_name: &str,
        key: &Key,
        status: Option<Status>,
    ) -> anyhow::Result<Option<Item>> {
        let t = self.reg.require(type_name)?;
        let mut conn = self.db.acquire().await?;
        let item = imp::fetch(&mut conn, self.db.backend(), t, key).await?;
        Ok(item.filter(|i| status.is_none_or(|s| i.status == s)))
    }

    /// The single item of a singleton type, if it exists.
    pub async fn singleton(&self, type_name: &str) -> anyhow::Result<Option<Item>> {
        let t = self.reg.require(type_name)?;
        if !t.singleton {
            anyhow::bail!("`{type_name}` is not a singleton type");
        }
        Ok(self
            .list(
                type_name,
                &ListQuery {
                    status: None,
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await?
            .into_iter()
            .next())
    }

    /// Create an item (version 1) and its first revision.
    pub async fn create(
        &self,
        type_name: &str,
        ch: &Changes,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let t = self.reg.require(type_name)?;
        let v = self.validate(t, ch, true)?;
        let item = imp::create(&self.db, &self.reg, t, ch, v, author).await?;
        self.db.notify_changed();
        Ok(item)
    }

    /// Update an item that the caller read at `expected_version`. Only the
    /// given fields change.
    pub async fn update(
        &self,
        type_name: &str,
        id: i64,
        expected_version: i32,
        ch: &Changes,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let t = self.reg.require(type_name)?;
        let v = self.validate(t, ch, false)?;
        let item = imp::update(&self.db, &self.reg, t, id, expected_version, ch, v, author).await?;
        self.db.notify_changed();
        Ok(item)
    }

    /// Delete an item. With `expected_version`, only if it is unchanged.
    pub async fn delete(
        &self,
        type_name: &str,
        id: i64,
        expected_version: Option<i32>,
        author: Option<&str>,
    ) -> Result<(), SaveError> {
        let t = self.reg.require(type_name)?;
        imp::delete(&self.db, t, id, expected_version, author).await?;
        self.db.notify_changed();
        Ok(())
    }

    /// Revisions of an item, newest first.
    pub async fn revisions(&self, type_name: &str, id: i64) -> anyhow::Result<Vec<Revision>> {
        self.reg.require(type_name)?;
        imp::revisions(&self.db, type_name, id).await
    }

    /// Terms of a vocabulary, by name.
    pub async fn terms(&self, vocabulary: &str) -> anyhow::Result<Vec<Term>> {
        if !self.reg.is_vocabulary(vocabulary) {
            anyhow::bail!(
                "no vocabulary `{vocabulary}` (vocabularies are named by `terms` fields)"
            );
        }
        imp::terms(&self.db, vocabulary).await
    }

    /// Terms of a vocabulary with the number of *published* items carrying
    /// each (terms used only by drafts are left out), by name.
    pub async fn term_counts(&self, vocabulary: &str) -> anyhow::Result<Vec<(Term, i64)>> {
        if !self.reg.is_vocabulary(vocabulary) {
            anyhow::bail!("no vocabulary `{vocabulary}`");
        }
        let types: Vec<&TypeDef> = self
            .reg
            .types()
            .filter(|t| {
                t.fields.iter().any(|f| {
                    f.kind == FieldKind::Terms && f.vocabulary.as_deref() == Some(vocabulary)
                })
            })
            .collect();
        if types.is_empty() {
            return Ok(Vec::new());
        }
        // Type names are validated identifiers (table names); bound too.
        let mut parts = Vec::new();
        for (i, t) in types.iter().enumerate() {
            parts.push(format!(
                "SELECT it.term_id FROM engine_item_terms it JOIN {} c ON c.id = it.item_id \
                 WHERE it.content_type = ${} AND c.status = 'published'",
                t.table(),
                i + 2
            ));
        }
        let sql = format!(
            "SELECT t.id, t.vocabulary, t.slug, t.name, t.version, count(*) AS n \
             FROM engine_terms t JOIN ({}) x ON x.term_id = t.id \
             WHERE t.vocabulary = $1 \
             GROUP BY t.id, t.vocabulary, t.slug, t.name, t.version ORDER BY t.name, t.id",
            parts.join(" UNION ALL ")
        );
        let mut q = engine_db::query(sql).bind(vocabulary);
        for t in &types {
            q.push(t.name.as_str());
        }
        let rows: Vec<(i64, String, String, String, i32, i64)> = q.fetch_all(&self.db).await?;
        Ok(rows
            .into_iter()
            .map(|(id, vocabulary, slug, name, version, n)| {
                (
                    Term {
                        id,
                        vocabulary,
                        slug,
                        name,
                        version,
                    },
                    n,
                )
            })
            .collect())
    }

    /// Published items of the same type sharing the most terms with item
    /// `id` (then newest first), at most `limit`.
    pub async fn related(&self, type_name: &str, id: i64, limit: i64) -> anyhow::Result<Vec<Item>> {
        self.reg.require(type_name)?;
        let limit = limit.clamp(1, 50);
        // Two indexed lookups instead of one self-join with the content
        // table: about five times faster on a site with 1000 items.
        let terms: Vec<i64> = crate::sql::ITEM_TERM_IDS
            .bind(type_name)
            .bind(id)
            .scalars(&self.db)
            .await?;
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let candidates: Vec<(i64, i64)> = crate::sql::SHARING_TERMS
            .bind(terms)
            .bind(type_name)
            .bind(id)
            .bind(limit * 4)
            .fetch_all(&self.db)
            .await?;
        let mut out = Vec::new();
        for (item_id, _) in candidates {
            if out.len() as i64 >= limit {
                break;
            }
            if let Some(i) = self
                .get(type_name, &Key::Id(item_id), Some(Status::Published))
                .await?
            {
                out.push(i);
            }
        }
        Ok(out)
    }

    pub async fn term(&self, vocabulary: &str, key: &Key) -> anyhow::Result<Option<Term>> {
        Ok(self
            .terms(vocabulary)
            .await?
            .into_iter()
            .find(|t| match key {
                Key::Id(i) => t.id == *i,
                Key::Slug(s) => &t.slug == s,
            }))
    }
}

mod imp {
    use super::*;
    use crate::sql;
    use anyhow::{Context, Result, bail};
    use engine_db::{Arg, Backend, Exec, Row, Timestamp, Tx, query};

    fn select_list(t: &TypeDef, backend: Backend) -> String {
        let mut cols = vec![
            "id".to_string(),
            "slug".into(),
            "status".into(),
            "version".into(),
            "created_at".into(),
            "updated_at".into(),
            "published_at".into(),
        ];
        for f in t.columns() {
            if f.kind == FieldKind::Date && backend == Backend::Postgres {
                cols.push(format!("\"{0}\"::text AS \"{0}\"", f.name));
            } else {
                cols.push(format!("\"{}\"", f.name));
            }
        }
        cols.join(", ")
    }

    fn row_to_item(t: &TypeDef, row: &Row) -> Result<Item> {
        let mut fields = Map::new();
        for f in t.columns() {
            let n = f.name.as_str();
            let v = match f.kind {
                FieldKind::Integer | FieldKind::Relation | FieldKind::Media => {
                    row.get::<Option<i64>>(n)?.map(Json::from)
                }
                FieldKind::Boolean => row.get::<Option<bool>>(n)?.map(Json::Bool),
                FieldKind::Blocks => row.get::<Option<sqlx::types::Json<Json>>>(n)?.map(|j| j.0),
                _ => row.get::<Option<String>>(n)?.map(Json::String),
            };
            fields.insert(f.name.clone(), v.unwrap_or(Json::Null));
        }
        let status: String = row.get("status")?;
        Ok(Item {
            content_type: t.name.clone(),
            id: row.get("id")?,
            slug: row.get("slug")?,
            status: Status::parse(&status).context("bad status in database")?,
            version: row.get("version")?,
            created_at: row.get::<Timestamp>("created_at")?.iso(),
            updated_at: row.get::<Timestamp>("updated_at")?.iso(),
            published_at: row
                .get::<Option<Timestamp>>("published_at")?
                .map(|t| t.iso()),
            fields,
        })
    }

    #[derive(sqlx::FromRow)]
    struct TermRow {
        item_id: i64,
        id: i64,
        vocabulary: String,
        slug: String,
        name: String,
    }

    /// Fill `terms` fields of `items`.
    async fn attach_terms(ex: impl Exec, t: &TypeDef, items: &mut [Item]) -> Result<()> {
        let term_fields: Vec<&FieldDef> = t
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Terms)
            .collect();
        if term_fields.is_empty() || items.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
        let rows: Vec<TermRow> = sql::ITEM_TERMS
            .bind(&t.name)
            .bind(ids)
            .fetch_all(ex)
            .await?;
        for item in items.iter_mut() {
            for f in &term_fields {
                let vocab = f.vocabulary.as_deref().unwrap_or("");
                let list: Vec<Json> = rows
                    .iter()
                    .filter(|r| r.item_id == item.id && r.vocabulary == vocab)
                    .map(|r| serde_json::json!({ "id": r.id, "slug": r.slug, "name": r.name }))
                    .collect();
                item.fields.insert(f.name.clone(), Json::Array(list));
            }
        }
        Ok(())
    }

    fn order_sql(t: &TypeDef, order: &str) -> Result<String> {
        Ok(match order {
            "" | "newest" => "COALESCE(published_at, created_at) DESC, id DESC".into(),
            "oldest" => "COALESCE(published_at, created_at) ASC, id ASC".into(),
            "updated" => "updated_at DESC, id DESC".into(),
            "title" => match &t.title_field {
                Some(f) => format!("\"{f}\" ASC NULLS LAST, id ASC"),
                None => "slug ASC".into(),
            },
            "slug" => "slug ASC".into(),
            other => {
                let (name, desc) = match other.strip_prefix('-') {
                    Some(n) => (n, true),
                    None => (other, false),
                };
                match t.field(name) {
                    Some(f) if f.kind.has_column() && f.kind != FieldKind::Blocks => {
                        if desc {
                            format!("\"{name}\" DESC NULLS LAST, id DESC")
                        } else {
                            format!("\"{name}\" ASC NULLS LAST, id ASC")
                        }
                    }
                    _ => bail!(
                        "cannot order `{}` by `{other}`: use newest, oldest, updated, title, \
                         slug, or a field name (prefix `-` for descending)",
                        t.name
                    ),
                }
            }
        })
    }

    /// `WHERE` conditions of a list query, with their text parameters.
    fn filter(t: &TypeDef, q: &ListQuery) -> Result<(String, Vec<String>)> {
        let mut sql = String::from(" WHERE 1 = 1");
        let mut binds: Vec<String> = Vec::new();
        let param = |binds: &mut Vec<String>, v: String| {
            binds.push(v);
            format!("${}", binds.len())
        };
        if let Some(s) = q.status {
            let p = param(&mut binds, s.as_str().to_string());
            sql.push_str(&format!(" AND status = {p}"));
        }
        if let Some(term) = &q.term {
            let term_fields: Vec<&FieldDef> = t
                .fields
                .iter()
                .filter(|f| f.kind == FieldKind::Terms)
                .collect();
            let (vocab, slug) = match term.split_once('/') {
                Some((v, s)) => (v.to_string(), s.to_string()),
                None => match term_fields.as_slice() {
                    [f] => (f.vocabulary.clone().unwrap_or_default(), term.clone()),
                    [] => bail!("`{}` has no terms field to filter by", t.name),
                    _ => bail!(
                        "`{}` has several terms fields; write the term as \
                         `<vocabulary>/<slug>`",
                        t.name
                    ),
                },
            };
            let a = param(&mut binds, t.name.clone());
            let b = param(&mut binds, vocab);
            let c = param(&mut binds, slug);
            sql.push_str(&format!(
                " AND id IN (SELECT it.item_id FROM engine_item_terms it JOIN engine_terms tt \
                 ON tt.id = it.term_id WHERE it.content_type = {a} AND tt.vocabulary = {b} \
                 AND tt.slug = {c})"
            ));
        }
        if let Some(text) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let pattern = format!(
                "%{}%",
                text.to_lowercase()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            let p = param(&mut binds, pattern);
            // `lower(..) LIKE` with an explicit escape works the same on both
            // backends (Postgres' ILIKE does not exist in SQLite).
            match &t.title_field {
                Some(f) => sql.push_str(&format!(
                    " AND (lower(\"{f}\") LIKE {p} ESCAPE '\\' OR slug LIKE {p} ESCAPE '\\')"
                )),
                None => sql.push_str(&format!(" AND slug LIKE {p} ESCAPE '\\'")),
            }
        }
        Ok((sql, binds))
    }

    pub async fn count(db: &engine_db::Db, t: &TypeDef, q: &ListQuery) -> Result<i64> {
        let (filter, binds) = filter(t, q)?;
        let sql = format!("SELECT count(*) FROM {}{filter}", t.table());
        let mut query = query(sql);
        for b in binds {
            query.push(b);
        }
        Ok(query.scalar(db).await?)
    }

    pub async fn list(db: &engine_db::Db, t: &TypeDef, q: &ListQuery) -> Result<Vec<Item>> {
        let (filter, binds) = filter(t, q)?;
        let (lim, off) = (binds.len() + 1, binds.len() + 2);
        let sql = format!(
            "SELECT {} FROM {}{filter} ORDER BY {} LIMIT ${lim} OFFSET ${off}",
            select_list(t, db.backend()),
            t.table(),
            order_sql(t, &q.order)?
        );
        let mut query = query(sql);
        for b in binds {
            query.push(b);
        }
        query.push(q.limit.unwrap_or(i64::MAX).max(0));
        query.push(q.offset.max(0));
        let mut conn = db.acquire().await?;
        let rows = query.rows(&mut conn).await?;
        let mut items = rows
            .iter()
            .map(|r| row_to_item(t, r))
            .collect::<Result<Vec<_>>>()?;
        attach_terms(&mut conn, t, &mut items).await?;
        Ok(items)
    }

    /// One item, with its terms. `E` is a held connection or a transaction
    /// (both are needed: the row, then its terms).
    pub async fn fetch<E>(
        ex: &mut E,
        backend: Backend,
        t: &TypeDef,
        key: &Key,
    ) -> Result<Option<Item>>
    where
        for<'a> &'a mut E: Exec,
    {
        let col = match key {
            Key::Id(_) => "id",
            Key::Slug(_) => "slug",
        };
        let sql = format!(
            "SELECT {} FROM {} WHERE {col} = $1",
            select_list(t, backend),
            t.table()
        );
        let q = match key {
            Key::Id(i) => query(sql).bind(*i),
            Key::Slug(s) => query(sql).bind(s.as_str()),
        };
        let Some(row) = q.row_opt(&mut *ex).await? else {
            return Ok(None);
        };
        let mut items = vec![row_to_item(t, &row)?];
        attach_terms(&mut *ex, t, &mut items).await?;
        Ok(items.pop())
    }

    /// The SQL placeholder for a field value: Postgres needs `date`
    /// columns cast from the bound text.
    fn placeholder(backend: Backend, kind: FieldKind, n: usize) -> String {
        if backend == Backend::Postgres && kind == FieldKind::Date {
            format!("${n}::date")
        } else {
            format!("${n}")
        }
    }

    /// Column assignments for validated values (relations resolved here).
    async fn column_values(
        tx: &mut Tx,
        reg: &Registry,
        v: &Validated,
    ) -> Result<Vec<(String, FieldKind, Arg)>, SaveError> {
        let mut out = Vec::new();
        let mut errs = ValidationErrors::default();
        for (f, val) in &v.values {
            if !f.kind.has_column() {
                continue;
            }
            let b = match (f.kind, val) {
                (FieldKind::Integer | FieldKind::Media, Value::Int(i)) => Arg::I64(Some(*i)),
                (FieldKind::Integer | FieldKind::Relation | FieldKind::Media, Value::Null) => {
                    Arg::I64(None)
                }
                (FieldKind::Boolean, Value::Bool(b)) => Arg::Bool(Some(*b)),
                (FieldKind::Boolean, Value::Null) => Arg::Bool(None),
                (FieldKind::Blocks, Value::Blocks(j)) => Arg::Json(Some(j.clone())),
                (FieldKind::Blocks, Value::Null) => Arg::Json(None),
                (FieldKind::Relation, Value::Relation(r)) => {
                    let to = reg.require(f.to.as_deref().unwrap_or(""))?;
                    let q = match r {
                        RelRef::Id(i) => {
                            query(format!("SELECT id FROM {} WHERE id = $1", to.table())).bind(*i)
                        }
                        RelRef::Slug(s) => {
                            query(format!("SELECT id FROM {} WHERE slug = $1", to.table()))
                                .bind(s.as_str())
                        }
                    };
                    match q.scalar_opt::<i64>(&mut *tx).await? {
                        Some(id) => Arg::I64(Some(id)),
                        None => {
                            let shown = match r {
                                RelRef::Id(i) => format!("#{i}"),
                                RelRef::Slug(s) => format!("`{s}`"),
                            };
                            errs.push(&f.name, format!("refers to no {} {shown}", to.name));
                            continue;
                        }
                    }
                }
                (_, Value::Text(s) | Value::Date(s)) => Arg::Text(Some(s.clone())),
                (_, Value::Null) => Arg::Text(None),
                (k, other) => {
                    return Err(SaveError::Other(anyhow::anyhow!(
                        "internal: {} value {other:?} for a {} field",
                        f.name,
                        k.as_str()
                    )));
                }
            };
            out.push((f.name.clone(), f.kind, b));
        }
        if !errs.is_empty() {
            return Err(errs.into());
        }
        Ok(out)
    }

    async fn taken_slugs(tx: &mut Tx, t: &TypeDef, base: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT slug FROM {} WHERE slug = $1 OR slug LIKE $2",
            t.table()
        );
        Ok(query(sql)
            .bind(base)
            .bind(format!("{base}-%"))
            .scalars(&mut *tx)
            .await?)
    }

    async fn set_terms(
        tx: &mut Tx,
        t: &TypeDef,
        item_id: i64,
        vocab: &str,
        names: &[String],
    ) -> Result<()> {
        sql::CLEAR_ITEM_TERMS
            .bind(&t.name)
            .bind(item_id)
            .bind(vocab)
            .execute(&mut *tx)
            .await?;
        for name in names {
            let s = slug::slugify(name).context("term without letters or digits")?;
            sql::INSERT_TERM
                .bind(vocab)
                .bind(&s)
                .bind(name)
                .execute(&mut *tx)
                .await?;
            let term_id: i64 = sql::TERM_ID.bind(vocab).bind(&s).scalar(&mut *tx).await?;
            sql::INSERT_ITEM_TERM
                .bind(&t.name)
                .bind(item_id)
                .bind(term_id)
                .execute(&mut *tx)
                .await?;
        }
        Ok(())
    }

    async fn write_terms(tx: &mut Tx, t: &TypeDef, id: i64, v: &Validated) -> Result<()> {
        for (f, val) in &v.values {
            if let (FieldKind::Terms, Value::Terms(names)) = (f.kind, val) {
                set_terms(tx, t, id, f.vocabulary.as_deref().unwrap_or(""), names).await?;
            }
        }
        Ok(())
    }

    async fn revision(
        tx: &mut Tx,
        item: &Item,
        version: i32,
        op: &str,
        author: Option<&str>,
    ) -> Result<()> {
        let data = serde_json::to_value(item)?;
        sql::INSERT_REVISION
            .bind(&item.content_type)
            .bind(item.id)
            .bind(version)
            .bind(op)
            .bind(data)
            .bind(author)
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    pub async fn create(
        db: &engine_db::Db,
        reg: &Registry,
        t: &TypeDef,
        ch: &Changes,
        v: Validated,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let backend = db.backend();
        let mut tx = db.begin().await?;
        let cols = column_values(&mut tx, reg, &v).await?;

        if t.singleton {
            let n: i64 = query(format!("SELECT count(*) FROM {}", t.table()))
                .scalar(&mut tx)
                .await?;
            if n > 0 {
                return Err(SaveError::Other(anyhow::anyhow!(
                    "`{}` is a singleton and already exists; change it with `set`",
                    t.name
                )));
            }
        }
        let slug = match (&ch.slug, t.singleton) {
            (Some(s), _) => {
                if !taken_slugs(&mut tx, t, s).await?.iter().any(|x| x == s) {
                    s.clone()
                } else {
                    let mut e = ValidationErrors::default();
                    e.push(
                        "slug",
                        format!("`{s}` is already used by another {}", t.name),
                    );
                    return Err(e.into());
                }
            }
            (None, true) => t.name.replace('_', "-"),
            (None, false) => {
                let from = t.slug_from.as_deref().unwrap_or("");
                let text = v
                    .values
                    .iter()
                    .find(|(f, _)| f.name == from)
                    .and_then(|(_, val)| match val {
                        Value::Text(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                let base = slug::slugify(text).unwrap_or_else(|| t.name.replace('_', "-"));
                slug::unique(&base, &taken_slugs(&mut tx, t, &base).await?)
            }
        };
        let status = ch.status.unwrap_or(Status::Draft);
        let now = Timestamp::now();

        let mut names = vec![
            "slug".to_string(),
            "status".into(),
            "created_at".into(),
            "updated_at".into(),
            "published_at".into(),
        ];
        let mut params = vec![
            "$1".to_string(),
            "$2".into(),
            "$3".into(),
            "$3".into(),
            "CASE WHEN $2 = 'published' THEN $3 END".into(),
        ];
        for (i, (name, kind, _)) in cols.iter().enumerate() {
            names.push(format!("\"{name}\""));
            params.push(placeholder(backend, *kind, i + 4));
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
            t.table(),
            names.join(", "),
            params.join(", ")
        );
        let mut q = query(sql).bind(slug).bind(status.as_str()).bind(now);
        for (_, _, b) in cols {
            q.push(b);
        }
        let id: i64 = q.scalar(&mut tx).await?;
        write_terms(&mut tx, t, id, &v).await?;
        let item = fetch(&mut tx, backend, t, &Key::Id(id))
            .await?
            .context("created item vanished")?;
        revision(&mut tx, &item, item.version, "create", author).await?;
        tx.commit().await?;
        Ok(item)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        db: &engine_db::Db,
        reg: &Registry,
        t: &TypeDef,
        id: i64,
        expected: i32,
        ch: &Changes,
        v: Validated,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let backend = db.backend();
        let mut tx = db.begin().await?;
        let cols = column_values(&mut tx, reg, &v).await?;
        if let Some(s) = &ch.slug {
            let sql = format!("SELECT id FROM {} WHERE slug = $1 AND id <> $2", t.table());
            let other: Option<i64> = query(sql).bind(s).bind(id).scalar_opt(&mut tx).await?;
            if other.is_some() {
                let mut e = ValidationErrors::default();
                e.push(
                    "slug",
                    format!("`{s}` is already used by another {}", t.name),
                );
                return Err(e.into());
            }
        }

        // $1 = id, $2 = expected version, $3 = now
        let mut sets = vec![
            "version = version + 1".to_string(),
            "updated_at = $3".to_string(),
        ];
        let mut n = 3;
        for (name, kind, _) in &cols {
            n += 1;
            sets.push(format!("\"{name}\" = {}", placeholder(backend, *kind, n)));
        }
        let slug_param = ch.slug.as_ref().map(|_| {
            n += 1;
            n
        });
        if let Some(p) = slug_param {
            sets.push(format!("slug = ${p}"));
        }
        let status_param = ch.status.map(|_| {
            n += 1;
            n
        });
        if let Some(p) = status_param {
            sets.push(format!("status = ${p}"));
            sets.push(format!(
                "published_at = CASE WHEN ${p} = 'published' THEN COALESCE(published_at, $3) \
                 ELSE published_at END"
            ));
        }
        let sql = format!(
            "UPDATE {} SET {} WHERE id = $1 AND version = $2 RETURNING version",
            t.table(),
            sets.join(", ")
        );
        let mut q = query(sql).bind(id).bind(expected).bind(Timestamp::now());
        for (_, _, b) in cols {
            q.push(b);
        }
        if let Some(s) = &ch.slug {
            q.push(s.clone());
        }
        if let Some(s) = ch.status {
            q.push(s.as_str());
        }
        let updated: Option<i32> = q.scalar_opt(&mut tx).await?;
        if updated.is_none() {
            let current: Option<i32> =
                query(format!("SELECT version FROM {} WHERE id = $1", t.table()))
                    .bind(id)
                    .scalar_opt(&mut tx)
                    .await?;
            return Err(match current {
                Some(current) => SaveError::Conflict {
                    content_type: t.name.clone(),
                    id,
                    expected,
                    current,
                },
                None => SaveError::NotFound {
                    content_type: t.name.clone(),
                    key: format!("#{id}"),
                },
            });
        }
        write_terms(&mut tx, t, id, &v).await?;
        let item = fetch(&mut tx, backend, t, &Key::Id(id))
            .await?
            .context("updated item vanished")?;
        revision(&mut tx, &item, item.version, "update", author).await?;
        tx.commit().await?;
        Ok(item)
    }

    pub async fn delete(
        db: &engine_db::Db,
        t: &TypeDef,
        id: i64,
        expected: Option<i32>,
        author: Option<&str>,
    ) -> Result<(), SaveError> {
        // The transaction holds the write lock (Postgres: the row lock taken
        // by the DELETE; SQLite: BEGIN IMMEDIATE), so the version read here
        // is the one deleted.
        let mut tx = db.begin().await?;
        if db.backend() == Backend::Postgres {
            query(format!(
                "SELECT id FROM {} WHERE id = $1 FOR UPDATE",
                t.table()
            ))
            .bind(id)
            .execute(&mut tx)
            .await?;
        }
        let Some(item) = fetch(&mut tx, db.backend(), t, &Key::Id(id)).await? else {
            return Err(SaveError::NotFound {
                content_type: t.name.clone(),
                key: format!("#{id}"),
            });
        };
        if let Some(e) = expected
            && e != item.version
        {
            return Err(SaveError::Conflict {
                content_type: t.name.clone(),
                id,
                expected: e,
                current: item.version,
            });
        }
        sql::DELETE_ITEM_TERMS
            .bind(&t.name)
            .bind(id)
            .execute(&mut tx)
            .await?;
        query(format!("DELETE FROM {} WHERE id = $1", t.table()))
            .bind(id)
            .execute(&mut tx)
            .await?;
        revision(&mut tx, &item, item.version + 1, "delete", author).await?;
        tx.commit().await?;
        Ok(())
    }

    #[derive(sqlx::FromRow)]
    struct RevisionRow {
        id: i64,
        version: i32,
        op: String,
        author: Option<String>,
        data: sqlx::types::Json<Json>,
        created_at: Timestamp,
    }

    pub async fn revisions(db: &engine_db::Db, type_name: &str, id: i64) -> Result<Vec<Revision>> {
        let rows: Vec<RevisionRow> = sql::REVISIONS
            .bind(type_name)
            .bind(id)
            .fetch_all(db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| Revision {
                id: r.id,
                version: r.version,
                op: r.op,
                author: r.author,
                created_at: r.created_at.iso(),
                data: r.data.0,
            })
            .collect())
    }

    #[derive(sqlx::FromRow)]
    struct TermOnly {
        id: i64,
        vocabulary: String,
        slug: String,
        name: String,
        version: i32,
    }

    impl From<TermOnly> for Term {
        fn from(r: TermOnly) -> Term {
            Term {
                id: r.id,
                vocabulary: r.vocabulary,
                slug: r.slug,
                name: r.name,
                version: r.version,
            }
        }
    }

    pub async fn rename_term(
        db: &engine_db::Db,
        vocabulary: &str,
        id: i64,
        expected: i32,
        name: &str,
    ) -> Result<Term, SaveError> {
        let updated: Option<TermOnly> = sql::RENAME_TERM
            .bind(vocabulary)
            .bind(id)
            .bind(expected)
            .bind(name)
            .bind(Timestamp::now())
            .fetch_optional(db)
            .await?;
        match updated {
            Some(t) => Ok(t.into()),
            None => Err(term_conflict(db, vocabulary, id, expected).await?),
        }
    }

    pub async fn delete_term(
        db: &engine_db::Db,
        vocabulary: &str,
        id: i64,
        expected: i32,
    ) -> Result<(), SaveError> {
        let deleted = sql::DELETE_TERM
            .bind(vocabulary)
            .bind(id)
            .bind(expected)
            .execute(db)
            .await?;
        if deleted == 1 {
            Ok(())
        } else {
            Err(term_conflict(db, vocabulary, id, expected).await?)
        }
    }

    async fn term_conflict(
        db: &engine_db::Db,
        vocabulary: &str,
        id: i64,
        expected: i32,
    ) -> Result<SaveError> {
        let current: Option<i32> = sql::TERM_VERSION
            .bind(vocabulary)
            .bind(id)
            .scalar_opt(db)
            .await?;
        Ok(match current {
            Some(current) => SaveError::Conflict {
                content_type: vocabulary.to_string(),
                id,
                expected,
                current,
            },
            None => SaveError::NotFound {
                content_type: vocabulary.to_string(),
                key: format!("#{id}"),
            },
        })
    }

    pub async fn terms(db: &engine_db::Db, vocabulary: &str) -> Result<Vec<Term>> {
        let rows: Vec<TermOnly> = sql::TERMS.bind(vocabulary).fetch_all(db).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
