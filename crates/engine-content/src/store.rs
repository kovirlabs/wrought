//! Reading and writing content items.
//!
//! Every item row has a `version`. Updates name the version they edited and
//! fail with [`SaveError::Conflict`] if the row moved on since (optimistic
//! locking). Each create, update and delete writes its revision row in the
//! same transaction as the change, so there is never a save without a
//! revision or a revision without a save.
//!
//! Queries on the engine's fixed tables (`engine_revisions`, `engine_terms`,
//! `engine_item_terms`) are checked against the schema at compile time
//! (`sqlx::query!`, with offline data in `.sqlx/`). Queries on
//! `content_<type>` tables are built at run time from the type files: their
//! table and column names are only known per site. Names are validated
//! identifiers and all values are bound parameters.

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
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::list(pool, &self.reg, t, q).await,
        }
    }

    /// How many items [`Store::list`] would return without limit/offset.
    pub async fn count(&self, type_name: &str, q: &ListQuery) -> anyhow::Result<i64> {
        let t = self.reg.require(type_name)?;
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::count(pool, t, q).await,
        }
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
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => {
                pg::rename_term(pool, vocabulary, id, expected_version, name).await
            }
        }
    }

    /// Delete a taxonomy term (and its assignments) if it is unchanged.
    pub async fn delete_term(
        &self,
        vocabulary: &str,
        id: i64,
        expected_version: i32,
    ) -> Result<(), SaveError> {
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::delete_term(pool, vocabulary, id, expected_version).await,
        }
    }

    /// One item by id or slug; `status: None` finds drafts too.
    pub async fn get(
        &self,
        type_name: &str,
        key: &Key,
        status: Option<Status>,
    ) -> anyhow::Result<Option<Item>> {
        let t = self.reg.require(type_name)?;
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                let item = pg::fetch(&mut conn, &self.reg, t, key, false).await?;
                Ok(item.filter(|i| status.is_none_or(|s| i.status == s)))
            }
        }
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
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::create(pool, &self.reg, t, ch, v, author).await,
        }
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
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => {
                pg::update(pool, &self.reg, t, id, expected_version, ch, v, author).await
            }
        }
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
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => {
                pg::delete(pool, &self.reg, t, id, expected_version, author).await
            }
        }
    }

    /// Revisions of an item, newest first.
    pub async fn revisions(&self, type_name: &str, id: i64) -> anyhow::Result<Vec<Revision>> {
        self.reg.require(type_name)?;
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::revisions(pool, type_name, id).await,
        }
    }

    /// Terms of a vocabulary, by name.
    pub async fn terms(&self, vocabulary: &str) -> anyhow::Result<Vec<Term>> {
        if !self.reg.is_vocabulary(vocabulary) {
            anyhow::bail!(
                "no vocabulary `{vocabulary}` (vocabularies are named by `terms` fields)"
            );
        }
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => pg::terms(pool, vocabulary).await,
        }
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

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use anyhow::{Context, Result, bail};
    use sqlx::postgres::{PgArguments, PgRow};
    use sqlx::query::Query;
    use sqlx::{PgConnection, PgPool, Postgres, Row};

    const TS: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";

    fn ts(col: &str) -> String {
        format!("to_char({col} AT TIME ZONE 'UTC', {TS}) AS {col}")
    }

    fn select_list(t: &TypeDef) -> String {
        let mut cols = vec![
            "id".to_string(),
            "slug".into(),
            "status".into(),
            "version".into(),
            ts("created_at"),
            ts("updated_at"),
            ts("published_at"),
        ];
        for f in t.columns() {
            if f.kind == FieldKind::Date {
                cols.push(format!("\"{0}\"::text AS \"{0}\"", f.name));
            } else {
                cols.push(format!("\"{}\"", f.name));
            }
        }
        cols.join(", ")
    }

    fn row_to_item(t: &TypeDef, row: &PgRow) -> Result<Item> {
        let mut fields = Map::new();
        for f in t.columns() {
            let n = f.name.as_str();
            let v = match f.kind {
                FieldKind::Integer | FieldKind::Relation => {
                    row.try_get::<Option<i64>, _>(n)?.map(Json::from)
                }
                FieldKind::Boolean => row.try_get::<Option<bool>, _>(n)?.map(Json::Bool),
                FieldKind::Blocks => row.try_get::<Option<Json>, _>(n)?,
                _ => row.try_get::<Option<String>, _>(n)?.map(Json::String),
            };
            fields.insert(f.name.clone(), v.unwrap_or(Json::Null));
        }
        let status: String = row.try_get("status")?;
        Ok(Item {
            content_type: t.name.clone(),
            id: row.try_get("id")?,
            slug: row.try_get("slug")?,
            status: Status::parse(&status).context("bad status in database")?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            published_at: row.try_get("published_at")?,
            fields,
        })
    }

    /// Fill `terms` fields of `items`.
    async fn attach_terms(conn: &mut PgConnection, t: &TypeDef, items: &mut [Item]) -> Result<()> {
        let term_fields: Vec<&FieldDef> = t
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Terms)
            .collect();
        if term_fields.is_empty() || items.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
        let rows = sqlx::query!(
            r#"SELECT it.item_id, t.id, t.vocabulary, t.slug, t.name
               FROM engine_item_terms it JOIN engine_terms t ON t.id = it.term_id
               WHERE it.content_type = $1 AND it.item_id = ANY($2)
               ORDER BY t.name, t.id"#,
            t.name,
            &ids
        )
        .fetch_all(&mut *conn)
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
        let mut sql = String::from(" WHERE true");
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
                text.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            let p = param(&mut binds, pattern);
            match &t.title_field {
                Some(f) => sql.push_str(&format!(" AND (\"{f}\" ILIKE {p} OR slug ILIKE {p})")),
                None => sql.push_str(&format!(" AND slug ILIKE {p}")),
            }
        }
        Ok((sql, binds))
    }

    pub async fn count(pool: &PgPool, t: &TypeDef, q: &ListQuery) -> Result<i64> {
        let (filter, binds) = filter(t, q)?;
        let sql = format!("SELECT count(*) FROM {}{filter}", t.table());
        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        for b in binds {
            query = query.bind(b);
        }
        Ok(query.fetch_one(pool).await?)
    }

    pub async fn list(
        pool: &PgPool,
        reg: &Registry,
        t: &TypeDef,
        q: &ListQuery,
    ) -> Result<Vec<Item>> {
        let _ = reg;
        let (filter, binds) = filter(t, q)?;
        let (lim, off) = (binds.len() + 1, binds.len() + 2);
        let sql = format!(
            "SELECT {} FROM {}{filter} ORDER BY {} LIMIT ${lim} OFFSET ${off}",
            select_list(t),
            t.table(),
            order_sql(t, &q.order)?
        );
        let mut query = sqlx::query(&sql);
        for b in binds {
            query = query.bind(b);
        }
        query = query
            .bind(q.limit.unwrap_or(i64::MAX).max(0))
            .bind(q.offset.max(0));
        let mut conn = pool.acquire().await?;
        let rows = query.fetch_all(&mut *conn).await?;
        let mut items = rows
            .iter()
            .map(|r| row_to_item(t, r))
            .collect::<Result<Vec<_>>>()?;
        attach_terms(&mut conn, t, &mut items).await?;
        Ok(items)
    }

    pub async fn fetch(
        conn: &mut PgConnection,
        _reg: &Registry,
        t: &TypeDef,
        key: &Key,
        for_update: bool,
    ) -> Result<Option<Item>> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = match key {
            Key::Id(_) => format!(
                "SELECT {} FROM {} WHERE id = $1{lock}",
                select_list(t),
                t.table()
            ),
            Key::Slug(_) => {
                format!(
                    "SELECT {} FROM {} WHERE slug = $1{lock}",
                    select_list(t),
                    t.table()
                )
            }
        };
        let q = sqlx::query(&sql);
        let q = match key {
            Key::Id(i) => q.bind(*i),
            Key::Slug(s) => q.bind(s.clone()),
        };
        let Some(row) = q.fetch_optional(&mut *conn).await? else {
            return Ok(None);
        };
        let mut items = vec![row_to_item(t, &row)?];
        attach_terms(conn, t, &mut items).await?;
        Ok(items.pop())
    }

    /// A bound column value.
    enum Bind {
        Text(Option<String>),
        Int(Option<i64>),
        Bool(Option<bool>),
        Json(Option<Json>),
    }

    fn bind<'q>(q: Query<'q, Postgres, PgArguments>, b: Bind) -> Query<'q, Postgres, PgArguments> {
        match b {
            Bind::Text(v) => q.bind(v),
            Bind::Int(v) => q.bind(v),
            Bind::Bool(v) => q.bind(v),
            Bind::Json(v) => q.bind(v),
        }
    }

    /// Column assignments for validated values (relations resolved here).
    async fn column_values(
        conn: &mut PgConnection,
        reg: &Registry,
        v: &Validated,
    ) -> Result<Vec<(String, String, Bind)>, SaveError> {
        let mut out = Vec::new();
        let mut errs = ValidationErrors::default();
        for (f, val) in &v.values {
            if !f.kind.has_column() {
                continue;
            }
            let cast = if f.kind == FieldKind::Date {
                "::date"
            } else {
                ""
            };
            let b = match (f.kind, val) {
                (FieldKind::Integer, Value::Int(i)) => Bind::Int(Some(*i)),
                (FieldKind::Integer | FieldKind::Relation, Value::Null) => Bind::Int(None),
                (FieldKind::Boolean, Value::Bool(b)) => Bind::Bool(Some(*b)),
                (FieldKind::Boolean, Value::Null) => Bind::Bool(None),
                (FieldKind::Blocks, Value::Blocks(j)) => Bind::Json(Some(j.clone())),
                (FieldKind::Blocks, Value::Null) => Bind::Json(None),
                (FieldKind::Relation, Value::Relation(r)) => {
                    let to = reg.require(f.to.as_deref().unwrap_or(""))?;
                    let sql = match r {
                        RelRef::Id(_) => format!("SELECT id FROM {} WHERE id = $1", to.table()),
                        RelRef::Slug(_) => format!("SELECT id FROM {} WHERE slug = $1", to.table()),
                    };
                    let q = sqlx::query_scalar::<_, i64>(&sql);
                    let q = match r {
                        RelRef::Id(i) => q.bind(*i),
                        RelRef::Slug(s) => q.bind(s.clone()),
                    };
                    match q.fetch_optional(&mut *conn).await? {
                        Some(id) => Bind::Int(Some(id)),
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
                (_, Value::Text(s) | Value::Date(s)) => Bind::Text(Some(s.clone())),
                (_, Value::Null) => Bind::Text(None),
                (k, other) => {
                    return Err(SaveError::Other(anyhow::anyhow!(
                        "internal: {} value {other:?} for a {} field",
                        f.name,
                        k.as_str()
                    )));
                }
            };
            out.push((f.name.clone(), cast.to_string(), b));
        }
        if !errs.is_empty() {
            return Err(errs.into());
        }
        Ok(out)
    }

    async fn taken_slugs(conn: &mut PgConnection, t: &TypeDef, base: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT slug FROM {} WHERE slug = $1 OR slug LIKE $2",
            t.table()
        );
        Ok(sqlx::query_scalar::<_, String>(&sql)
            .bind(base)
            .bind(format!("{base}-%"))
            .fetch_all(&mut *conn)
            .await?)
    }

    async fn set_terms(
        conn: &mut PgConnection,
        t: &TypeDef,
        item_id: i64,
        vocab: &str,
        names: &[String],
    ) -> Result<()> {
        sqlx::query!(
            r#"DELETE FROM engine_item_terms it USING engine_terms tt
               WHERE it.term_id = tt.id AND it.content_type = $1 AND it.item_id = $2
                 AND tt.vocabulary = $3"#,
            t.name,
            item_id,
            vocab
        )
        .execute(&mut *conn)
        .await?;
        for name in names {
            let s = slug::slugify(name).context("term without letters or digits")?;
            sqlx::query!(
                r#"INSERT INTO engine_terms (vocabulary, slug, name) VALUES ($1, $2, $3)
                   ON CONFLICT (vocabulary, slug) DO NOTHING"#,
                vocab,
                s,
                name
            )
            .execute(&mut *conn)
            .await?;
            let term_id = sqlx::query_scalar!(
                "SELECT id FROM engine_terms WHERE vocabulary = $1 AND slug = $2",
                vocab,
                s
            )
            .fetch_one(&mut *conn)
            .await?;
            sqlx::query!(
                r#"INSERT INTO engine_item_terms (content_type, item_id, term_id)
                   VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"#,
                t.name,
                item_id,
                term_id
            )
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }

    async fn write_terms(
        conn: &mut PgConnection,
        t: &TypeDef,
        id: i64,
        v: &Validated,
    ) -> Result<()> {
        for (f, val) in &v.values {
            if let (FieldKind::Terms, Value::Terms(names)) = (f.kind, val) {
                set_terms(conn, t, id, f.vocabulary.as_deref().unwrap_or(""), names).await?;
            }
        }
        Ok(())
    }

    async fn revision(
        conn: &mut PgConnection,
        item: &Item,
        version: i32,
        op: &str,
        author: Option<&str>,
    ) -> Result<()> {
        let data = serde_json::to_value(item)?;
        sqlx::query!(
            r#"INSERT INTO engine_revisions (content_type, item_id, version, op, data, author)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
            item.content_type,
            item.id,
            version,
            op,
            data,
            author
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    pub async fn create(
        pool: &PgPool,
        reg: &Registry,
        t: &TypeDef,
        ch: &Changes,
        v: Validated,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let mut tx = pool.begin().await?;
        let cols = column_values(&mut tx, reg, &v).await?;

        if t.singleton {
            let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}", t.table()))
                .fetch_one(&mut *tx)
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

        let mut names = vec!["slug".to_string(), "status".into(), "published_at".into()];
        let mut params = vec![
            "$1".to_string(),
            "$2".into(),
            "CASE WHEN $2 = 'published' THEN now() END".into(),
        ];
        for (i, (name, cast, _)) in cols.iter().enumerate() {
            names.push(format!("\"{name}\""));
            params.push(format!("${}{cast}", i + 3));
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
            t.table(),
            names.join(", "),
            params.join(", ")
        );
        let mut q = sqlx::query(&sql).bind(slug).bind(status.as_str());
        for (_, _, b) in cols {
            q = bind(q, b);
        }
        let id: i64 = q.fetch_one(&mut *tx).await?.try_get("id")?;
        write_terms(&mut tx, t, id, &v).await?;
        let item = fetch(&mut tx, reg, t, &Key::Id(id), false)
            .await?
            .context("created item vanished")?;
        revision(&mut tx, &item, item.version, "create", author).await?;
        tx.commit().await?;
        Ok(item)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        pool: &PgPool,
        reg: &Registry,
        t: &TypeDef,
        id: i64,
        expected: i32,
        ch: &Changes,
        v: Validated,
        author: Option<&str>,
    ) -> Result<Item, SaveError> {
        let mut tx = pool.begin().await?;
        let cols = column_values(&mut tx, reg, &v).await?;
        if let Some(s) = &ch.slug {
            let sql = format!("SELECT id FROM {} WHERE slug = $1 AND id <> $2", t.table());
            let other: Option<i64> = sqlx::query_scalar(&sql)
                .bind(s)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            if other.is_some() {
                let mut e = ValidationErrors::default();
                e.push(
                    "slug",
                    format!("`{s}` is already used by another {}", t.name),
                );
                return Err(e.into());
            }
        }

        let mut sets = vec![
            "version = version + 1".to_string(),
            "updated_at = now()".to_string(),
        ];
        let mut n = 2; // $1 = id, $2 = expected version
        for (name, cast, _) in &cols {
            n += 1;
            sets.push(format!("\"{name}\" = ${n}{cast}"));
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
                "published_at = CASE WHEN ${p} = 'published' THEN COALESCE(published_at, now()) \
                 ELSE published_at END"
            ));
        }
        let sql = format!(
            "UPDATE {} SET {} WHERE id = $1 AND version = $2 RETURNING version",
            t.table(),
            sets.join(", ")
        );
        let mut q = sqlx::query(&sql).bind(id).bind(expected);
        for (_, _, b) in cols {
            q = bind(q, b);
        }
        if let Some(s) = &ch.slug {
            q = q.bind(s.clone());
        }
        if let Some(s) = ch.status {
            q = q.bind(s.as_str());
        }
        let updated = q.fetch_optional(&mut *tx).await?;
        if updated.is_none() {
            let current: Option<i32> =
                sqlx::query_scalar(&format!("SELECT version FROM {} WHERE id = $1", t.table()))
                    .bind(id)
                    .fetch_optional(&mut *tx)
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
        let item = fetch(&mut tx, reg, t, &Key::Id(id), false)
            .await?
            .context("updated item vanished")?;
        revision(&mut tx, &item, item.version, "update", author).await?;
        tx.commit().await?;
        Ok(item)
    }

    pub async fn delete(
        pool: &PgPool,
        reg: &Registry,
        t: &TypeDef,
        id: i64,
        expected: Option<i32>,
        author: Option<&str>,
    ) -> Result<(), SaveError> {
        let mut tx = pool.begin().await?;
        let Some(item) = fetch(&mut tx, reg, t, &Key::Id(id), true).await? else {
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
        sqlx::query!(
            "DELETE FROM engine_item_terms WHERE content_type = $1 AND item_id = $2",
            t.name,
            id
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!("DELETE FROM {} WHERE id = $1", t.table()))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        revision(&mut tx, &item, item.version + 1, "delete", author).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn revisions(pool: &PgPool, type_name: &str, id: i64) -> Result<Vec<Revision>> {
        let rows = sqlx::query!(
            r#"SELECT id, version, op, author, data,
                      to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!"
               FROM engine_revisions WHERE content_type = $1 AND item_id = $2
               ORDER BY version DESC"#,
            type_name,
            id
        )
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Revision {
                id: r.id,
                version: r.version,
                op: r.op,
                author: r.author,
                created_at: r.created_at,
                data: r.data,
            })
            .collect())
    }

    pub async fn rename_term(
        pool: &PgPool,
        vocabulary: &str,
        id: i64,
        expected: i32,
        name: &str,
    ) -> Result<Term, SaveError> {
        let updated = sqlx::query_as!(
            Term,
            r#"UPDATE engine_terms SET name = $4, version = version + 1, updated_at = now()
               WHERE vocabulary = $1 AND id = $2 AND version = $3
               RETURNING id, vocabulary, slug, name, version"#,
            vocabulary,
            id,
            expected,
            name
        )
        .fetch_optional(pool)
        .await?;
        match updated {
            Some(t) => Ok(t),
            None => Err(term_conflict(pool, vocabulary, id, expected).await?),
        }
    }

    pub async fn delete_term(
        pool: &PgPool,
        vocabulary: &str,
        id: i64,
        expected: i32,
    ) -> Result<(), SaveError> {
        let deleted = sqlx::query!(
            "DELETE FROM engine_terms WHERE vocabulary = $1 AND id = $2 AND version = $3",
            vocabulary,
            id,
            expected
        )
        .execute(pool)
        .await?;
        if deleted.rows_affected() == 1 {
            Ok(())
        } else {
            Err(term_conflict(pool, vocabulary, id, expected).await?)
        }
    }

    async fn term_conflict(
        pool: &PgPool,
        vocabulary: &str,
        id: i64,
        expected: i32,
    ) -> Result<SaveError> {
        let current = sqlx::query_scalar!(
            "SELECT version FROM engine_terms WHERE vocabulary = $1 AND id = $2",
            vocabulary,
            id
        )
        .fetch_optional(pool)
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

    pub async fn terms(pool: &PgPool, vocabulary: &str) -> Result<Vec<Term>> {
        let rows = sqlx::query_as!(
            Term,
            "SELECT id, vocabulary, slug, name, version FROM engine_terms
             WHERE vocabulary = $1 ORDER BY name, id",
            vocabulary
        )
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}
