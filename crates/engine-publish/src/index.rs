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

    /// SHA-256 (hex) of the bytes last published at `path`, if indexed.
    pub async fn sha256_of(&self, path: &str) -> Result<Option<String>> {
        Ok(sql::PAGE_SHA256
            .bind(path)
            .scalar_opt::<String>(&self.db)
            .await?)
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

    /// Record several pages' entries in one transaction (a publish batch:
    /// one commit instead of one per page).
    pub async fn record_many(&self, entries: &[PageEntry]) -> Result<()> {
        self.record_many_and_advance(entries, None).await
    }

    /// [`Self::record_many`] and, with `upto`, [`Self::advance`] in the
    /// same transaction: a publish batch waits for one disk sync, not two.
    pub async fn record_many_and_advance(
        &self,
        entries: &[PageEntry],
        upto: Option<i64>,
    ) -> Result<()> {
        if entries.is_empty() && upto.is_none() {
            return Ok(());
        }
        let mut tx = self.db.begin().await?;
        if let Some(upto) = upto {
            sql::ADVANCE.bind(upto).execute(&mut tx).await?;
            sql::PRUNE_CHANGES.bind(upto).execute(&mut tx).await?;
        }
        if entries.is_empty() {
            tx.commit().await?;
            return Ok(());
        }
        if tx.backend() == engine_db::Backend::Postgres {
            // One entry per path (the last wins), as one statement may not
            // update a row twice.
            let mut by_path = std::collections::BTreeMap::new();
            for e in entries {
                by_path.insert(e.path.as_str(), e);
            }
            let entries: Vec<&PageEntry> = by_path.into_values().collect();
            let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
            let outputs: Vec<String> = entries.iter().map(|e| e.output.clone()).collect();
            let hashes: Vec<String> = entries.iter().map(|e| e.sha256.clone()).collect();
            sql::PG_UPSERT_PAGES
                .bind(paths.as_slice())
                .bind(outputs.as_slice())
                .bind(hashes.as_slice())
                .bind(Timestamp::now())
                .execute(&mut tx)
                .await?;
            sql::PG_CLEAR_TAGS_OF
                .bind(paths.as_slice())
                .execute(&mut tx)
                .await?;
            let (mut tags, mut tag_paths) = (Vec::new(), Vec::new());
            for e in &entries {
                for t in &e.tags {
                    tags.push(t.clone());
                    tag_paths.push(e.path.clone());
                }
            }
            sql::BULK_PAGE_TAGS
                .bind(tags.as_slice())
                .bind(tag_paths.as_slice())
                .execute(&mut tx)
                .await?;
        } else {
            for e in entries {
                record_in(&mut tx, e).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        sql::REMOVE_PAGE.bind(path).execute(&self.db).await?;
        Ok(())
    }

    /// Replace the whole index (after `engine build`) and set the cursor, in
    /// one transaction.
    ///
    /// On Postgres the commit does not wait for the disk sync
    /// (`synchronous_commit = off` for this transaction only): the pages it
    /// describes are written without fsync too, and the recovery after a
    /// crash is the same for both, `engine rebuild`. A build therefore never
    /// waits on the disk, which on a disk slow to sync stalled builds for
    /// seconds (DECISIONS.md, 2026-11-26).
    pub async fn replace_all(&self, entries: &[PageEntry], cursor: i64) -> Result<()> {
        let mut tx = self.db.begin().await?;
        if tx.backend() == engine_db::Backend::Postgres {
            sql::PG_ASYNC_COMMIT.q().execute(&mut tx).await?;
        }
        sql::CLEAR_PAGES.q().execute(&mut tx).await?;
        if tx.backend() == engine_db::Backend::Postgres {
            let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
            let outputs: Vec<String> = entries.iter().map(|e| e.output.clone()).collect();
            let hashes: Vec<String> = entries.iter().map(|e| e.sha256.clone()).collect();
            sql::BULK_PAGES
                .bind(paths.as_slice())
                .bind(outputs.as_slice())
                .bind(hashes.as_slice())
                .bind(Timestamp::now())
                .execute(&mut tx)
                .await?;
            let (mut tags, mut tag_paths) = (Vec::new(), Vec::new());
            for e in entries {
                for t in &e.tags {
                    tags.push(t.clone());
                    tag_paths.push(e.path.clone());
                }
            }
            sql::BULK_PAGE_TAGS
                .bind(tags.as_slice())
                .bind(tag_paths.as_slice())
                .execute(&mut tx)
                .await?;
        } else {
            let rows: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| serde_json::json!([e.path, e.output, e.sha256]))
                .collect();
            sql::BULK_PAGES
                .bind(serde_json::Value::Array(rows))
                .bind(Timestamp::now())
                .execute(&mut tx)
                .await?;
            let tags: Vec<serde_json::Value> = entries
                .iter()
                .flat_map(|e| e.tags.iter().map(move |t| serde_json::json!([t, e.path])))
                .collect();
            sql::BULK_PAGE_TAGS
                .bind(serde_json::Value::Array(tags))
                .execute(&mut tx)
                .await?;
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
