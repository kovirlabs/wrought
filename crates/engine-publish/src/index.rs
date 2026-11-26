//! The persistent tag index (which published path carries which cache tags)
//! and the publisher's cursor into the change log. Every query here is
//! checked against the schema at compile time.

use anyhow::Result;
use engine_db::Db;

/// One entry of `engine_changes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub id: i64,
    pub content_type: String,
    pub item_id: i64,
    pub op: String,
}

/// A page to record: URL path, output file, body hash and tags.
#[derive(Debug, Clone)]
pub struct PageEntry {
    pub path: String,
    pub output: String,
    pub sha256: String,
    pub tags: Vec<String>,
}

#[derive(Clone)]
pub struct TagIndex {
    db: Db,
}

impl TagIndex {
    pub fn new(db: Db) -> Self {
        TagIndex { db }
    }

    /// Paths whose last render recorded any of `tags`.
    pub async fn lookup(&self, tags: &[String]) -> Result<Vec<String>> {
        match &self.db {
            Db::Postgres(p) => Ok(sqlx::query_scalar!(
                "SELECT DISTINCT path FROM engine_page_tags WHERE tag = ANY($1) ORDER BY path",
                tags
            )
            .fetch_all(p)
            .await?),
        }
    }

    /// Tags recorded for one path.
    pub async fn tags_of(&self, path: &str) -> Result<Vec<String>> {
        match &self.db {
            Db::Postgres(p) => Ok(sqlx::query_scalar!(
                "SELECT tag FROM engine_page_tags WHERE path = $1 ORDER BY tag",
                path
            )
            .fetch_all(p)
            .await?),
        }
    }

    /// Every indexed path with its output file.
    pub async fn all(&self) -> Result<Vec<(String, String)>> {
        match &self.db {
            Db::Postgres(p) => {
                let rows = sqlx::query!("SELECT path, output FROM engine_pages ORDER BY path")
                    .fetch_all(p)
                    .await?;
                Ok(rows.into_iter().map(|r| (r.path, r.output)).collect())
            }
        }
    }

    /// Record (or replace) one page's entry.
    pub async fn record(&self, e: &PageEntry) -> Result<()> {
        match &self.db {
            Db::Postgres(p) => {
                let mut tx = p.begin().await?;
                record_pg(&mut tx, e).await?;
                tx.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        match &self.db {
            Db::Postgres(p) => {
                sqlx::query!("DELETE FROM engine_pages WHERE path = $1", path)
                    .execute(p)
                    .await?;
                Ok(())
            }
        }
    }

    /// Replace the whole index (after `engine build`) and set the cursor, in
    /// one transaction.
    pub async fn replace_all(&self, entries: &[PageEntry], cursor: i64) -> Result<()> {
        match &self.db {
            Db::Postgres(p) => {
                let mut tx = p.begin().await?;
                sqlx::query!("DELETE FROM engine_pages")
                    .execute(&mut *tx)
                    .await?;
                for e in entries {
                    record_pg(&mut tx, e).await?;
                }
                sqlx::query!("UPDATE engine_publish_state SET last_change = $1", cursor)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            }
        }
    }

    /// Id of the newest change-log entry (0 if none).
    pub async fn latest_change(&self) -> Result<i64> {
        match &self.db {
            Db::Postgres(p) => Ok(sqlx::query_scalar!(
                r#"SELECT COALESCE(max(id), 0) AS "id!" FROM engine_changes"#
            )
            .fetch_one(p)
            .await?),
        }
    }

    pub async fn cursor(&self) -> Result<i64> {
        match &self.db {
            Db::Postgres(p) => Ok(sqlx::query_scalar!(
                "SELECT last_change FROM engine_publish_state"
            )
            .fetch_one(p)
            .await?),
        }
    }

    /// Changes after the cursor, oldest first.
    pub async fn pending(&self, after: i64, limit: i64) -> Result<Vec<Change>> {
        match &self.db {
            Db::Postgres(p) => Ok(sqlx::query_as!(
                Change,
                "SELECT id, content_type, item_id, op FROM engine_changes
                 WHERE id > $1 ORDER BY id LIMIT $2",
                after,
                limit
            )
            .fetch_all(p)
            .await?),
        }
    }

    /// Move the cursor to `upto` and drop the processed change-log rows.
    pub async fn advance(&self, upto: i64) -> Result<()> {
        match &self.db {
            Db::Postgres(p) => {
                let mut tx = p.begin().await?;
                sqlx::query!(
                    "UPDATE engine_publish_state SET last_change = GREATEST(last_change, $1)",
                    upto
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query!("DELETE FROM engine_changes WHERE id <= $1", upto)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            }
        }
    }
}

async fn record_pg(tx: &mut sqlx::PgConnection, e: &PageEntry) -> Result<()> {
    sqlx::query!(
        "INSERT INTO engine_pages (path, output, sha256, rendered_at) VALUES ($1, $2, $3, now())
         ON CONFLICT (path) DO UPDATE
           SET output = EXCLUDED.output, sha256 = EXCLUDED.sha256, rendered_at = now()",
        e.path,
        e.output,
        e.sha256
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM engine_page_tags WHERE path = $1", e.path)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "INSERT INTO engine_page_tags (tag, path) SELECT DISTINCT unnest($2::text[]), $1",
        e.path,
        &e.tags
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}
