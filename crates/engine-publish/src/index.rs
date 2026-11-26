//! The persistent tag index (which published path carries which cache tags)
//! and the publisher's cursor into the change log.

use anyhow::Result;
use engine_db::{Db, Timestamp, Tx};

use crate::sql;

/// One entry of `engine_changes`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
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
        Ok(sql::LOOKUP.bind(tags).scalars(&self.db).await?)
    }

    /// Tags recorded for one path.
    pub async fn tags_of(&self, path: &str) -> Result<Vec<String>> {
        Ok(sql::TAGS_OF.bind(path).scalars(&self.db).await?)
    }

    /// Every indexed path with its output file.
    pub async fn all(&self) -> Result<Vec<(String, String)>> {
        Ok(sql::ALL_PAGES.q().fetch_all(&self.db).await?)
    }

    /// Record (or replace) one page's entry.
    pub async fn record(&self, e: &PageEntry) -> Result<()> {
        let mut tx = self.db.begin().await?;
        record_in(&mut tx, e).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        sql::REMOVE_PAGE.bind(path).execute(&self.db).await?;
        Ok(())
    }

    /// Replace the whole index (after `engine build`) and set the cursor, in
    /// one transaction.
    pub async fn replace_all(&self, entries: &[PageEntry], cursor: i64) -> Result<()> {
        let mut tx = self.db.begin().await?;
        sql::CLEAR_PAGES.q().execute(&mut tx).await?;
        for e in entries {
            record_in(&mut tx, e).await?;
        }
        sql::SET_CURSOR.bind(cursor).execute(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Id of the newest change-log entry (0 if none).
    pub async fn latest_change(&self) -> Result<i64> {
        Ok(sql::LATEST_CHANGE.q().scalar(&self.db).await?)
    }

    pub async fn cursor(&self) -> Result<i64> {
        Ok(sql::CURSOR.q().scalar(&self.db).await?)
    }

    /// Changes after the cursor, oldest first.
    pub async fn pending(&self, after: i64, limit: i64) -> Result<Vec<Change>> {
        Ok(sql::PENDING
            .bind(after)
            .bind(limit)
            .fetch_all(&self.db)
            .await?)
    }

    /// Move the cursor to `upto` and drop the processed change-log rows.
    pub async fn advance(&self, upto: i64) -> Result<()> {
        let mut tx = self.db.begin().await?;
        sql::ADVANCE.bind(upto).execute(&mut tx).await?;
        sql::PRUNE_CHANGES.bind(upto).execute(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn record_in(tx: &mut Tx, e: &PageEntry) -> Result<()> {
    sql::UPSERT_PAGE
        .bind(&e.path)
        .bind(&e.output)
        .bind(&e.sha256)
        .bind(Timestamp::now())
        .execute(&mut *tx)
        .await?;
    sql::CLEAR_PAGE_TAGS.bind(&e.path).execute(&mut *tx).await?;
    sql::INSERT_PAGE_TAGS
        .bind(&e.path)
        .bind(e.tags.as_slice())
        .execute(&mut *tx)
        .await?;
    Ok(())
}
