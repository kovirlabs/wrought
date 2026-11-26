//! Putting jobs in the queue and looking at it.

use std::time::Duration;

use anyhow::{Result, bail};
use engine_db::Db;
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

pub(crate) fn pg(db: &Db) -> &sqlx::PgPool {
    match db {
        Db::Postgres(p) => p,
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
        let mut conn = pg(&self.db).acquire().await?;
        enqueue_on(&mut conn, kind, payload, &opts).await
    }

    /// Queue a job inside the caller's transaction, so it exists only if
    /// the transaction commits (e.g. a receipt for a stored submission).
    pub async fn enqueue_in(
        conn: &mut sqlx::PgConnection,
        kind: &str,
        payload: Json,
        opts: &Enqueue,
    ) -> Result<i64> {
        enqueue_on(conn, kind, payload, opts).await
    }

    pub async fn job(&self, id: i64) -> Result<Option<JobInfo>> {
        Ok(sqlx::query_as!(
            JobInfo,
            r#"SELECT id, kind, status, attempts, max_attempts,
                 to_char(run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "run_at!",
                 locked_by, last_error,
                 to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!",
                 to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS finished_at,
                 payload
               FROM engine_jobs WHERE id = $1"#,
            id
        )
        .fetch_optional(pg(&self.db))
        .await?)
    }

    /// Newest first; `status` filters (`queued`, `running`, `done`, `dead`).
    pub async fn list(&self, status: Option<&str>, limit: i64) -> Result<Vec<JobInfo>> {
        Ok(sqlx::query_as!(
            JobInfo,
            r#"SELECT id, kind, status, attempts, max_attempts,
                 to_char(run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "run_at!",
                 locked_by, last_error,
                 to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!",
                 to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS finished_at,
                 payload
               FROM engine_jobs WHERE ($1::text IS NULL OR status = $1)
               ORDER BY id DESC LIMIT $2"#,
            status,
            limit
        )
        .fetch_all(pg(&self.db))
        .await?)
    }

    /// Number of jobs per status.
    pub async fn counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query!(
            r#"SELECT status, count(*) AS "n!" FROM engine_jobs GROUP BY status ORDER BY status"#
        )
        .fetch_all(pg(&self.db))
        .await?;
        Ok(rows.into_iter().map(|r| (r.status, r.n)).collect())
    }

    /// Queue a dead job again with a fresh set of attempts. `false` if it
    /// is not dead.
    pub async fn retry(&self, id: i64) -> Result<bool> {
        let r = sqlx::query!(
            r#"UPDATE engine_jobs SET status = 'queued', attempts = 0, run_at = now(),
                 finished_at = NULL WHERE id = $1 AND status = 'dead'"#,
            id
        )
        .execute(pg(&self.db))
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Delete finished jobs: `done` older than `done_after`, `dead` older
    /// than `dead_after`. Returns how many.
    pub async fn prune(&self, done_after: Duration, dead_after: Duration) -> Result<u64> {
        let r = sqlx::query!(
            r#"DELETE FROM engine_jobs
               WHERE (status = 'done' AND finished_at < now() - make_interval(secs => $1))
                  OR (status = 'dead' AND finished_at < now() - make_interval(secs => $2))"#,
            done_after.as_secs_f64(),
            dead_after.as_secs_f64()
        )
        .execute(pg(&self.db))
        .await?;
        Ok(r.rows_affected())
    }
}

async fn enqueue_on(
    conn: &mut sqlx::PgConnection,
    kind: &str,
    payload: Json,
    opts: &Enqueue,
) -> Result<i64> {
    check(kind, opts)?;
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO engine_jobs (kind, payload, max_attempts, run_at)
           VALUES ($1, $2, $3, now() + make_interval(secs => $4)) RETURNING id"#,
        kind,
        payload,
        opts.max_attempts,
        opts.delay.as_secs_f64()
    )
    .fetch_one(conn)
    .await?)
}
