//! Putting jobs in the queue and looking at it.

use std::time::Duration;

use anyhow::{Result, bail};
use engine_db::{Db, Exec, Timestamp, Tx};

use crate::sql;
use serde::Serialize;
use serde_json::Value as Json;

/// The job queue of one site. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Jobs {
    db: Db,
}

/// Options for [`Jobs::enqueue_with`].
#[derive(Debug, Clone)]
pub struct Enqueue {
    /// Run no earlier than this from now.
    pub delay: Duration,
    /// Attempts before the job is `dead` (1..=100).
    pub max_attempts: i32,
}

impl Default for Enqueue {
    fn default() -> Self {
        Enqueue {
            delay: Duration::ZERO,
            max_attempts: 5,
        }
    }
}

/// A job as stored.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JobInfo {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub run_at: String,
    pub locked_by: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub payload: Json,
}

/// Jobs need Postgres (`FOR UPDATE SKIP LOCKED`, `LISTEN`).
pub(crate) fn require_pg(db: &Db) -> Result<()> {
    if db.backend() != engine_db::Backend::Postgres {
        return Err(engine_db::postgres_only("jobs"));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: i64,
    kind: String,
    status: String,
    attempts: i32,
    max_attempts: i32,
    run_at: Timestamp,
    locked_by: Option<String>,
    last_error: Option<String>,
    created_at: Timestamp,
    finished_at: Option<Timestamp>,
    payload: sqlx::types::Json<Json>,
}

impl From<JobRow> for JobInfo {
    fn from(r: JobRow) -> JobInfo {
        JobInfo {
            id: r.id,
            kind: r.kind,
            status: r.status,
            attempts: r.attempts,
            max_attempts: r.max_attempts,
            run_at: r.run_at.iso(),
            locked_by: r.locked_by,
            last_error: r.last_error,
            created_at: r.created_at.iso(),
            finished_at: r.finished_at.map(|t| t.iso()),
            payload: r.payload.0,
        }
    }
}

fn check(kind: &str, opts: &Enqueue) -> Result<()> {
    if !crate::valid_kind(kind) {
        bail!("invalid job kind `{kind}`");
    }
    if !(1..=100).contains(&opts.max_attempts) {
        bail!("max_attempts must be between 1 and 100");
    }
    Ok(())
}

impl Jobs {
    pub fn new(db: Db) -> Self {
        Jobs { db }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Queue a job to run as soon as a worker is free. Returns its id.
    pub async fn enqueue(&self, kind: &str, payload: Json) -> Result<i64> {
        self.enqueue_with(kind, payload, Enqueue::default()).await
    }

    pub async fn enqueue_with(&self, kind: &str, payload: Json, opts: Enqueue) -> Result<i64> {
        require_pg(&self.db)?;
        enqueue_on(&self.db, kind, payload, &opts).await
    }

    /// Queue a job inside the caller's transaction, so it exists only if
    /// the transaction commits (e.g. a receipt for a stored submission).
    pub async fn enqueue_in(tx: &mut Tx, kind: &str, payload: Json, opts: &Enqueue) -> Result<i64> {
        enqueue_on(tx, kind, payload, opts).await
    }

    pub async fn job(&self, id: i64) -> Result<Option<JobInfo>> {
        require_pg(&self.db)?;
        let row: Option<JobRow> = sql::JOB.bind(id).fetch_optional(&self.db).await?;
        Ok(row.map(Into::into))
    }

    /// Newest first; `status` filters (`queued`, `running`, `done`, `dead`).
    pub async fn list(&self, status: Option<&str>, limit: i64) -> Result<Vec<JobInfo>> {
        self.page(status, limit, 0).await
    }

    /// Like [`Jobs::list`], skipping the newest `offset`.
    pub async fn page(
        &self,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<JobInfo>> {
        require_pg(&self.db)?;
        let rows: Vec<JobRow> = sql::LIST
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Number of jobs per status.
    pub async fn counts(&self) -> Result<Vec<(String, i64)>> {
        require_pg(&self.db)?;
        Ok(sql::COUNTS.q().fetch_all(&self.db).await?)
    }

    /// Queue a dead job again with a fresh set of attempts. `false` if it
    /// is not dead.
    pub async fn retry(&self, id: i64) -> Result<bool> {
        require_pg(&self.db)?;
        Ok(sql::RETRY.bind(id).execute(&self.db).await? == 1)
    }

    /// Delete a job that is not running (dead or done ones, or a queued one
    /// that should not run). `false` if it is running or gone.
    pub async fn delete(&self, id: i64) -> Result<bool> {
        require_pg(&self.db)?;
        Ok(sql::DELETE.bind(id).execute(&self.db).await? == 1)
    }

    /// Delete finished jobs: `done` older than `done_after`, `dead` older
    /// than `dead_after`. Returns how many.
    pub async fn prune(&self, done_after: Duration, dead_after: Duration) -> Result<u64> {
        require_pg(&self.db)?;
        Ok(sql::PRUNE
            .bind(done_after.as_secs_f64())
            .bind(dead_after.as_secs_f64())
            .execute(&self.db)
            .await?)
    }
}

async fn enqueue_on(ex: impl Exec, kind: &str, payload: Json, opts: &Enqueue) -> Result<i64> {
    check(kind, opts)?;
    Ok(sql::ENQUEUE
        .bind(kind)
        .bind(payload)
        .bind(opts.max_attempts)
        .bind(opts.delay.as_secs_f64())
        .scalar(ex)
        .await?)
}
