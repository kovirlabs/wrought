//! Wrought's `search` feature: full-text search over published content.
//!
//! * **PostgreSQL:** a generated `tsvector` (title weighted above body) with
//!   a GIN index, queried with `websearch_to_tsquery` (quotes, `or`, `-word`
//!   work as people expect), ranked by `ts_rank`, highlighted with
//!   `ts_headline`. `[search] language` picks the text search configuration
//!   (`english` by default; `simple` for no stemming, or any installed one).
//! * **SQLite:** an FTS5 index (Porter stemming, accents folded), ranked by
//!   BM25 with titles weighted ten times; visitors' words are quoted so FTS5
//!   syntax in a query cannot error or widen it.
//!
//! The index holds one document per *published* item: its title, its text
//! (text and Markdown fields as plain text, text inside blocks, term names)
//! and its URL. `engine serve` keeps it in step from the same change feed
//! that drives publishing; `engine build` and `engine search reindex`
//! rebuild it whole.
//!
//! Templates get `search(q, type=none, limit=10, page=1)` and the
//! components `search_box` and `search_results`, which work without
//! JavaScript on a `request` route (`/search/?q=...`). Static sites use
//! Pagefind instead (`[search] pagefind = true`, run by `engine build`; see
//! the docs).

pub mod ext;
pub mod snippet;
pub mod sql;

use std::collections::BTreeSet;

use anyhow::Result;
use engine_content::ContentSite;
use engine_content::store::{Item, ListQuery, Status};
use engine_content::types::{FieldKind, TypeDef};
use engine_db::{Backend, Db, Migration, Timestamp};
use serde::{Deserialize, Serialize};

pub use ext::SearchExtension;

/// The `search` feature's schema.
pub fn migrations(backend: Backend) -> Vec<Migration> {
    match backend {
        Backend::Postgres => vec![
            Migration::new(
                "search/0001_search",
                include_str!("../migrations/postgres/0001_search.sql"),
            ),
            Migration::new(
                "search/0002_unlogged",
                include_str!("../migrations/postgres/0002_unlogged.sql"),
            ),
            Migration::new(
                "search/0003_tsv_inline",
                include_str!("../migrations/postgres/0003_tsv_inline.sql"),
            ),
        ],
        Backend::Sqlite => vec![Migration::new(
            "search/0001_search",
            include_str!("../migrations/sqlite/0001_search.sql"),
        )],
    }
}

fn d_language() -> String {
    "english".into()
}
fn d_path() -> String {
    "/search/".into()
}

/// `[search]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// PostgreSQL text search configuration (SQLite always stems English).
    #[serde(default = "d_language")]
    pub language: String,
    /// The site's search page, where `search_box` sends queries.
    #[serde(default = "d_path")]
    pub path: String,
    /// Types to leave out of the index.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Static sites: run Pagefind after `engine build` (handled by the
    /// build command, accepted here so one `[search]` serves both).
    #[serde(default)]
    pub pagefind: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl SearchConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let cfg: SearchConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[search]: {e}"))?,
            None => SearchConfig::default(),
        };
        if cfg.language.is_empty()
            || cfg.language.len() > 40
            || !cfg
                .language
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            anyhow::bail!(
                "[search] language must be a text search configuration name like `english`"
            );
        }
        let p = &cfg.path;
        if !p.starts_with('/')
            || p.starts_with("//")
            || p.chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        {
            anyhow::bail!("[search] path must be a path on this site, like `/search/`");
        }
        Ok(cfg)
    }
}

/// One result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    #[serde(rename = "type")]
    pub content_type: String,
    pub id: i64,
    pub title: String,
    pub url: Option<String>,
    pub published_at: Option<String>,
    /// HTML: the text around the matches, escaped, with `<mark>`s.
    pub snippet: String,
    pub rank: f64,
}

/// A page of results.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Results {
    pub query: String,
    pub hits: Vec<Hit>,
    pub total: i64,
    pub page: i64,
    pub pages: i64,
}

#[derive(sqlx::FromRow)]
struct HitRow {
    content_type: String,
    item_id: i64,
    title: String,
    path: Option<String>,
    published_at: Option<Timestamp>,
    snippet: Option<String>,
    rank: f64,
}

/// A row of `sql::PG_PAGE`: `snippet` is the document's text (or, with
/// `PG_PAGE_HEADLINE`, the headline), `total` the number of matches.
#[derive(sqlx::FromRow)]
struct PgHitRow {
    content_type: String,
    item_id: i64,
    title: String,
    path: Option<String>,
    published_at: Option<Timestamp>,
    snippet: Option<String>,
    rank: f64,
    total: i64,
    lexemes: String,
}

/// The index of one site.
#[derive(Clone)]
pub struct SearchIndex {
    db: Db,
    site: ContentSite,
    cfg: SearchConfig,
}

impl SearchIndex {
    pub fn new(db: Db, site: ContentSite, cfg: SearchConfig) -> Self {
        SearchIndex { db, site, cfg }
    }

    pub fn config(&self) -> &SearchConfig {
        &self.cfg
    }

    fn indexed(&self, t: &TypeDef) -> bool {
        !t.singleton && !self.cfg.exclude.contains(&t.name)
    }

    /// Bring one item's document up to date: written if the item is
    /// published, removed otherwise.
    pub async fn reindex_item(&self, type_name: &str, id: i64) -> Result<()> {
        let Some(t) = self.site.registry().get(type_name).cloned() else {
            return Ok(());
        };
        let item = self
            .site
            .store()
            .get(
                type_name,
                &engine_content::Key::Id(id),
                Some(Status::Published),
            )
            .await?;
        match item.filter(|_| self.indexed(&t)) {
            Some(item) => self.write(&t, &item).await,
            None => {
                let mut tx = self.db.begin_relaxed().await?;
                sql::REMOVE
                    .bind(type_name)
                    .bind(id)
                    .execute(&mut tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            }
        }
    }

    /// Every write to the index commits without waiting for the
    /// write-ahead log to reach the disk ([`Db::begin_relaxed`]): on
    /// Postgres the index is an unlogged table, so there is nothing durable
    /// to wait for, but a transaction that happens to write any WAL at all
    /// (PostgreSQL pruning dead catalog rows while planning, for one, which
    /// a fresh database has after its migrations) would otherwise wait for
    /// a sync at commit, which on a slow disk took seconds (DECISIONS.md,
    /// "the stall that remained").
    async fn write(&self, t: &TypeDef, item: &Item) -> Result<()> {
        let mut tx = self.db.begin_relaxed().await?;
        self.write_in(&mut tx, t, item).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn write_in(&self, tx: &mut engine_db::Tx, t: &TypeDef, item: &Item) -> Result<()> {
        let doc = document(t, item);
        sql::UPSERT
            .bind(self.cfg.language.as_str())
            .bind(t.name.as_str())
            .bind(item.id)
            .bind(doc.title)
            .bind(doc.body)
            .bind(self.site.item_path(item))
            .bind(item.published_at.as_deref().and_then(Timestamp::parse))
            .bind(Timestamp::now())
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    /// Rebuild the whole index. Returns the number of documents.
    ///
    /// One transaction: searches never see a half-built index, and the
    /// database syncs its log once instead of once per document (a
    /// 1000-article rebuild went from about 20 s of commit waits on
    /// Postgres to well under a second).
    ///
    /// On Postgres the index is an unlogged table (it is derived from the
    /// content and rebuilt whenever it is found empty, see
    /// [`Self::ensure_built`]), every document goes in one statement, and
    /// only documents that changed are written: rebuilding an index that is
    /// already current writes nothing and waits for no disk sync.
    pub async fn reindex_all(&self) -> Result<usize> {
        use engine_core::timings::phase;
        let mut docs = Vec::new();
        let types: Vec<TypeDef> = self.site.registry().types().cloned().collect();
        let p = phase("list items");
        for t in types.iter().filter(|t| self.indexed(t)) {
            let items = self
                .site
                .store()
                .list(
                    &t.name,
                    &ListQuery {
                        status: Some(Status::Published),
                        ..Default::default()
                    },
                )
                .await?;
            for item in items {
                docs.push((t.clone(), item));
            }
        }
        drop(p);
        let n = docs.len();
        if self.db.backend() == Backend::Postgres {
            let _p = phase("write documents");
            self.reindex_pg(&docs).await?;
            return Ok(n);
        }
        let mut tx = self.db.begin().await?;
        sql::CLEAR.q().execute(&mut tx).await?;
        for (t, item) in &docs {
            self.write_in(&mut tx, t, item).await?;
        }
        tx.commit().await?;
        Ok(n)
    }

    async fn reindex_pg(&self, docs: &[(TypeDef, Item)]) -> Result<()> {
        let mut types = Vec::with_capacity(docs.len());
        let mut ids = Vec::with_capacity(docs.len());
        let mut titles = Vec::with_capacity(docs.len());
        let mut bodies = Vec::with_capacity(docs.len());
        let mut paths = Vec::with_capacity(docs.len());
        let mut dates = Vec::with_capacity(docs.len());
        for (t, item) in docs {
            let doc = document(t, item);
            types.push(t.name.clone());
            ids.push(item.id);
            titles.push(doc.title);
            bodies.push(doc.body);
            paths.push(self.site.item_path(item).unwrap_or_default());
            dates.push(
                item.published_at
                    .as_deref()
                    .and_then(Timestamp::parse)
                    .map(|t| t.iso())
                    .unwrap_or_default(),
            );
        }
        // Relaxed commit: see `write`.
        let mut tx = self.db.begin_relaxed().await?;
        sql::PG_PRUNE
            .bind(types.clone())
            .bind(ids.clone())
            .execute(&mut tx)
            .await?;
        sql::PG_BULK_UPSERT
            .bind(self.cfg.language.as_str())
            .bind(types)
            .bind(ids)
            .bind(titles)
            .bind(bodies)
            .bind(paths)
            .bind(dates)
            .bind(Timestamp::now())
            .execute(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Build the index if it holds no document at all: a new database, one
    /// restored from a backup without it, or (Postgres, where the index is
    /// an unlogged table) after the database server crashed or failed
    /// over, which empties unlogged tables. Returns the documents written,
    /// if it ran.
    pub async fn ensure_built(&self) -> Result<Option<usize>> {
        if sql::ANY_DOCS.q().scalar::<bool>(&self.db).await? {
            return Ok(None);
        }
        Ok(Some(self.reindex_all().await?))
    }

    /// Documents in the index.
    pub async fn len(&self) -> Result<i64> {
        Ok(sql::COUNT_DOCS.q().scalar(&self.db).await?)
    }

    /// Search. `page` counts from 1; an empty query finds nothing.
    pub async fn search(
        &self,
        q: &str,
        type_name: Option<&str>,
        limit: i64,
        page: i64,
    ) -> Result<Results> {
        let query: String = q.chars().filter(|c| !c.is_control()).take(200).collect();
        let query = query.trim().to_string();
        let limit = limit.clamp(1, 100);
        let page = page.max(1);
        let empty = |query: String| Results {
            query,
            hits: Vec::new(),
            total: 0,
            page: 1,
            pages: 0,
        };
        let term = match self.db.backend() {
            Backend::Postgres => query.clone(),
            Backend::Sqlite => fts5_query(&query),
        };
        if term.is_empty() {
            return Ok(empty(query));
        }
        let lang = self.cfg.language.as_str();
        let (total, rows) = match self.db.backend() {
            Backend::Postgres => self.page_pg(&term, type_name, limit, page).await?,
            Backend::Sqlite => {
                // Independent: the count and the page of hits run at the
                // same time on two connections.
                let (total, rows): (i64, Vec<HitRow>) = tokio::try_join!(
                    sql::COUNT
                        .bind(lang)
                        .bind(term.as_str())
                        .bind(type_name)
                        .scalar(&self.db),
                    sql::QUERY
                        .bind(lang)
                        .bind(term.as_str())
                        .bind(type_name)
                        .bind(limit)
                        .bind((page - 1) * limit)
                        .fetch_all(&self.db),
                )?;
                let rows = rows
                    .into_iter()
                    .map(|r| {
                        let snippet = highlight(r.snippet.as_deref().unwrap_or(""));
                        (r, snippet)
                    })
                    .collect();
                (total, rows)
            }
        };
        engine_core::tags::record(engine_core::tags::CacheTag::collection("search"));
        Ok(Results {
            query,
            hits: rows
                .into_iter()
                .map(|(r, snippet)| Hit {
                    content_type: r.content_type,
                    id: r.item_id,
                    title: r.title,
                    url: r.path,
                    published_at: r.published_at.map(|t| t.iso()),
                    snippet,
                    rank: r.rank,
                })
                .collect(),
            total,
            page,
            pages: (total + limit - 1) / limit,
        })
    }

    /// One page of results on Postgres, in one statement (see
    /// `sql::PG_PAGE`), with the snippets cut here (`snippet.rs`) when the
    /// text search configuration has a stemmer in the engine.
    async fn page_pg(
        &self,
        term: &str,
        type_name: Option<&str>,
        limit: i64,
        page: i64,
    ) -> Result<(i64, Vec<(HitRow, String)>)> {
        let lang = self.cfg.language.as_str();
        let language = snippet::Language::for_config(lang);
        let stmt = if language.is_some() {
            &sql::PG_PAGE
        } else {
            &sql::PG_PAGE_HEADLINE
        };
        let rows: Vec<PgHitRow> = stmt
            .bind(lang)
            .bind(term)
            .bind(type_name)
            .bind(limit)
            .bind((page - 1) * limit)
            .fetch_all(&self.db)
            .await?;
        let total = match rows.first() {
            Some(r) => r.total,
            // Past the last page the window count has no row to ride on.
            None if page > 1 => {
                sql::COUNT
                    .bind(lang)
                    .bind(term)
                    .bind(type_name)
                    .scalar(&self.db)
                    .await?
            }
            None => 0,
        };
        let mut cut = match (language, rows.first()) {
            (Some(l), Some(r)) => Some(snippet::Snippets::new(
                l,
                snippet::query_lexemes(&r.lexemes),
            )),
            _ => None,
        };
        let rows = rows
            .into_iter()
            .map(|r| {
                let text = r.snippet.as_deref().unwrap_or("");
                let snippet = match &mut cut {
                    Some(c) => highlight(&c.cut(text)),
                    None => highlight(text),
                };
                (
                    HitRow {
                        content_type: r.content_type,
                        item_id: r.item_id,
                        title: r.title,
                        path: r.path,
                        published_at: r.published_at,
                        snippet: None,
                        rank: r.rank,
                    },
                    snippet,
                )
            })
            .collect();
        Ok((total, rows))
    }
}

/// An item's searchable text.
pub struct Document {
    pub title: String,
    pub body: String,
}

/// Plain text of Markdown (no markup, no HTML).
pub fn markdown_text(md: &str) -> String {
    use pulldown_cmark::{Event, Parser};
    let mut out = String::new();
    for ev in Parser::new(md) {
        match ev {
            Event::Text(t) | Event::Code(t) => {
                out.push_str(&t);
            }
            Event::SoftBreak | Event::HardBreak | Event::End(_) => out.push(' '),
            _ => {}
        }
    }
    out
}

fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

fn block_text(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(markdown_text(s)),
        serde_json::Value::Array(a) => a.iter().for_each(|x| block_text(x, out)),
        serde_json::Value::Object(m) => {
            for (k, x) in m {
                // `type` names the block and `id` is its stable id; links
                // are not prose.
                if k != "type" && k != "id" && !k.ends_with("href") && !k.ends_with("url") {
                    block_text(x, out);
                }
            }
        }
        _ => {}
    }
}

/// What goes into the index for `item`.
pub fn document(t: &TypeDef, item: &Item) -> Document {
    let title = t
        .title_field
        .as_ref()
        .and_then(|f| item.fields.get(f))
        .and_then(|v| v.as_str())
        .map(clean)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| item.slug.replace('-', " "));
    let mut parts = Vec::new();
    for f in &t.fields {
        if Some(&f.name) == t.title_field.as_ref() {
            continue;
        }
        let Some(v) = item.fields.get(&f.name) else {
            continue;
        };
        match f.kind {
            FieldKind::Text | FieldKind::Select => {
                if let Some(s) = v.as_str() {
                    parts.push(s.to_string());
                }
            }
            FieldKind::Markdown => {
                if let Some(s) = v.as_str() {
                    parts.push(markdown_text(s));
                }
            }
            FieldKind::Blocks => block_text(v, &mut parts),
            FieldKind::Terms => {
                for term in v.as_array().into_iter().flatten() {
                    if let Some(n) = term.get("name").and_then(|n| n.as_str()) {
                        parts.push(n.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    Document {
        title,
        body: clean(&parts.join(" ")),
    }
}

/// Visitors' words as an FTS5 query: each word quoted (so `AND`, `NEAR`,
/// `*`, `"` and column filters are plain text), all of them required, the
/// last one also as a prefix (results while typing).
pub fn fts5_query(q: &str) -> String {
    let words: Vec<String> = q
        .split(|c: char| c.is_whitespace() || c == '"')
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '\'' | '_' | '.'))
                .collect::<String>()
        })
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(12)
        .collect();
    let n = words.len();
    let mut seen = BTreeSet::new();
    words
        .into_iter()
        .enumerate()
        .filter(|(_, w)| seen.insert(w.clone()))
        .map(|(i, w)| {
            if i + 1 == n && w.chars().count() >= 3 {
                format!("\"{w}\"*")
            } else {
                format!("\"{w}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escape a snippet and turn the \x01..\x02 markers into `<mark>`.
pub fn highlight(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut open = false;
    for c in s.chars() {
        match c {
            '\u{1}' if !open => {
                out.push_str("<mark>");
                open = true;
            }
            '\u{2}' if open => {
                out.push_str("</mark>");
                open = false;
            }
            '\u{1}' | '\u{2}' => {}
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    if open {
        out.push_str("</mark>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts5_queries_are_quoted() {
        assert_eq!(fts5_query("rust web"), "\"rust\" \"web\"*");
        assert_eq!(fts5_query("  "), "");
        assert_eq!(fts5_query("a OR b"), "\"a\" \"OR\" \"b\"");
        assert_eq!(
            fts5_query("\"x\" NEAR(y) title:z*"),
            "\"x\" \"NEARy\" \"titlez\"*"
        );
        assert_eq!(fts5_query("rust rust"), "\"rust\"");
    }

    #[test]
    fn snippets_are_escaped_and_marked() {
        assert_eq!(
            highlight("a <b>\u{1}rust\u{2}</b> & \u{1}x"),
            "a &lt;b&gt;<mark>rust</mark>&lt;/b&gt; &amp; <mark>x</mark>"
        );
    }

    #[test]
    fn markdown_becomes_text() {
        let t =
            markdown_text("# Title\n\nSome *emphasis* and <script>x</script> [a link](http://x).");
        assert!(t.contains("Title") && t.contains("emphasis") && t.contains("a link"));
        assert!(!t.contains('*') && !t.contains("http://x"));
    }

    #[test]
    fn config_checks() {
        assert_eq!(SearchConfig::from_site(None).unwrap().language, "english");
        let t: toml::Table = toml::from_str("language = \"english'; drop\"").unwrap();
        assert!(SearchConfig::from_site(Some(&t)).is_err());
        let t: toml::Table = toml::from_str("path = \"//evil\"").unwrap();
        assert!(SearchConfig::from_site(Some(&t)).is_err());
    }
}
