//! Workers: claim, run, retry, reap, schedule, shut down.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use engine_db::Db;
use tokio::sync::Semaphore;
use tokio::task::{AbortHandle, JoinSet};

use crate::config::{JobsConfig, Schedule};
use crate::queue::pg;
use crate::{Handlers, JobContext, backoff_seconds};

/// How workers behave; usually from [`JobsConfig`].
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// Unique per worker pool; stored in `locked_by`.
    pub id: String,
    pub concurrency: usize,
    pub poll: Duration,
    pub lease: Duration,
    pub grace: Duration,
    pub retry_base_seconds: f64,
    pub retry_max_seconds: f64,
    pub timeout: Duration,
    pub schedules: Vec<Schedule>,
}

impl WorkerOptions {
    pub fn from_config(cfg: &JobsConfig) -> Result<Self> {
        Ok(WorkerOptions {
            id: worker_id(),
            concurrency: cfg.workers,
            poll: cfg.poll(),
            lease: cfg.lease(),
            grace: cfg.grace(),
            retry_base_seconds: cfg.retry_base_seconds,
            retry_max_seconds: cfg.retry_max_seconds,
            timeout: Duration::from_secs_f64(cfg.timeout_seconds),
            schedules: cfg.schedules()?,
        })
    }
}

/// `<host>-<pid>-<n>`: unique among the workers sharing a database.
pub fn worker_id() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "worker".into());
    format!(
        "{host}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

fn jitter() -> f64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(n % 1000) / 1000.0
}

struct Claimed {
    id: i64,
    kind: String,
    payload: serde_json::Value,
    attempts: i32,
    max_attempts: i32,
}

type InFlight = Arc<Mutex<BTreeMap<i64, AbortHandle>>>;

/// Run workers until `shutdown` resolves, then finish or release running
/// jobs (see the crate docs). Returns early only if the database is
/// unusable at start.
pub async fn run(
    db: Db,
    handlers: Handlers,
    opts: WorkerOptions,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<()> {
    let pool = pg(&db).clone();
    let kinds = handlers.kinds();
    sync_schedules(&pool, &opts.schedules).await?;
    let mut listener = match sqlx::postgres::PgListener::connect_with(&pool).await {
        Ok(mut l) => match l.listen("engine_jobs").await {
            Ok(()) => Some(l),
            Err(e) => {
                eprintln!("jobs: LISTEN failed ({e}); polling only");
                None
            }
        },
        Err(e) => {
            eprintln!("jobs: LISTEN connection failed ({e}); polling only");
            None
        }
    };
    let handlers = Arc::new(handlers);
    let opts = Arc::new(opts);
    let permits = Arc::new(Semaphore::new(opts.concurrency));
    let in_flight: InFlight = Arc::default();
    let mut running: JoinSet<()> = JoinSet::new();
    let housekeeping_every =
        (opts.lease / 3).clamp(Duration::from_millis(200), Duration::from_secs(5));
    let mut last_housekeeping: Option<Instant> = None;
    let mut last_schedule: Option<Instant> = None;
    tokio::pin!(shutdown);

    loop {
        if last_housekeeping.is_none_or(|t| t.elapsed() >= housekeeping_every) {
            last_housekeeping = Some(Instant::now());
            if let Err(e) = reap(&pool, opts.retry_base_seconds, opts.retry_max_seconds).await {
                eprintln!("jobs: checking leases: {e:#}");
            }
        }
        if !opts.schedules.is_empty() && last_schedule.is_none_or(|t| t.elapsed() >= opts.poll) {
            last_schedule = Some(Instant::now());
            if let Err(e) = tick_schedules(&pool).await {
                eprintln!("jobs: schedules: {e:#}");
            }
        }
        // Claim as many due jobs as there are free slots.
        while let Ok(permit) = permits.clone().try_acquire_owned() {
            match claim(&pool, &opts, &kinds).await {
                Ok(Some(job)) => {
                    let (pool, opts, handlers, in_flight) = (
                        pool.clone(),
                        opts.clone(),
                        handlers.clone(),
                        in_flight.clone(),
                    );
                    running.spawn(async move {
                        run_one(&pool, &opts, &handlers, &in_flight, job).await;
                        drop(permit);
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("jobs: claiming: {e:#}");
                    break;
                }
            }
        }
        let next_tick = opts.poll.min(housekeeping_every);
        tokio::select! {
            _ = &mut shutdown => break,
            n = async {
                match listener.as_mut() {
                    Some(l) => l.recv().await.map(|_| ()),
                    None => std::future::pending().await,
                }
            } => {
                if let Err(e) = n {
                    eprintln!("jobs: notification connection: {e}");
                    tokio::time::sleep(opts.poll).await;
                }
            }
            _ = tokio::time::sleep(next_tick) => {}
            Some(_) = running.join_next(), if !running.is_empty() => {}
        }
    }

    // Shutdown: no more claims; let running jobs finish within the grace
    // period, then stop them and give them back to the queue.
    if !running.is_empty() {
        eprintln!(
            "jobs: waiting up to {:.0} s for {} running job(s)",
            opts.grace.as_secs_f64(),
            running.len()
        );
    }
    let deadline = tokio::time::sleep(opts.grace);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            r = running.join_next() => if r.is_none() { break },
            _ = &mut deadline => {
                let left: Vec<(i64, AbortHandle)> = std::mem::take(
                    &mut *in_flight.lock().unwrap_or_else(|p| p.into_inner()),
                ).into_iter().collect();
                running.abort_all();
                for (id, handle) in left {
                    handle.abort();
                    if let Err(e) = release(&pool, &opts.id, id).await {
                        eprintln!("jobs: releasing job {id}: {e:#}");
                    } else {
                        eprintln!("jobs: job {id} stopped at shutdown; queued again");
                    }
                }
                break;
            }
        }
    }
    Ok(())
}

async fn claim(
    pool: &sqlx::PgPool,
    opts: &WorkerOptions,
    kinds: &[String],
) -> Result<Option<Claimed>> {
    Ok(sqlx::query_as!(
        Claimed,
        r#"UPDATE engine_jobs SET status = 'running', attempts = attempts + 1,
             locked_by = $1, locked_until = now() + make_interval(secs => $2),
             started_at = now()
           WHERE id = (
             SELECT id FROM engine_jobs
             WHERE status = 'queued' AND run_at <= now() AND kind = ANY($3)
             ORDER BY run_at, id
             LIMIT 1
             FOR UPDATE SKIP LOCKED)
           RETURNING id, kind, payload, attempts, max_attempts"#,
        opts.id,
        opts.lease.as_secs_f64(),
        kinds
    )
    .fetch_optional(pool)
    .await?)
}

async fn run_one(
    pool: &sqlx::PgPool,
    opts: &WorkerOptions,
    handlers: &Handlers,
    in_flight: &InFlight,
    job: Claimed,
) {
    let ctx = JobContext {
        id: job.id,
        kind: job.kind.clone(),
        payload: job.payload,
        attempt: job.attempts,
        max_attempts: job.max_attempts,
        scrub: Default::default(),
    };
    let Some(handler) = handlers.get(&job.kind).cloned() else {
        // Not claimable (kinds are filtered), but be safe.
        let _ = fail(pool, opts, job.id, job.attempts, "no handler for this kind").await;
        return;
    };
    let task = tokio::spawn(handler(ctx.clone()));
    in_flight
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(job.id, task.abort_handle());

    // Heartbeat: keep the lease while the handler runs.
    let hb = {
        let pool = pool.clone();
        let id = opts.id.clone();
        let lease = opts.lease;
        let job_id = job.id;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(lease / 3).await;
                match sqlx::query!(
                    r#"UPDATE engine_jobs SET locked_until = now() + make_interval(secs => $3)
                       WHERE id = $1 AND locked_by = $2 AND status = 'running'"#,
                    job_id,
                    id,
                    lease.as_secs_f64()
                )
                .execute(&pool)
                .await
                {
                    Ok(r) if r.rows_affected() == 0 => {
                        eprintln!("jobs: job {job_id} lost its lease (taken back by the queue)");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("jobs: heartbeat for job {job_id}: {e}"),
                }
            }
        })
    };

    let outcome = match tokio::time::timeout(opts.timeout, task).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(format!("{e:#}")),
        Ok(Err(join)) if join.is_panic() => Err("the handler panicked".to_string()),
        Ok(Err(_)) => Err("stopped".to_string()),
        Err(_) => Err(format!(
            "timed out after {:.0} s",
            opts.timeout.as_secs_f64()
        )),
    };
    hb.abort();
    in_flight
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&job.id);
    let res = match outcome {
        Ok(()) => finish(pool, &opts.id, job.id, ctx.scrubbed()).await,
        Err(msg) => {
            if job.attempts >= job.max_attempts {
                eprintln!("jobs: job {} ({}) failed for good: {msg}", job.id, job.kind);
            } else {
                eprintln!(
                    "jobs: job {} ({}) failed (attempt {}/{}), will retry: {msg}",
                    job.id, job.kind, job.attempts, job.max_attempts
                );
            }
            fail(pool, opts, job.id, job.attempts, &msg).await
        }
    };
    if let Err(e) = res {
        eprintln!("jobs: recording the result of job {}: {e:#}", job.id);
    }
}

async fn finish(pool: &sqlx::PgPool, worker: &str, id: i64, scrub: bool) -> Result<()> {
    sqlx::query!(
        r#"UPDATE engine_jobs SET status = 'done', finished_at = now(), locked_by = NULL,
             locked_until = NULL, last_error = NULL,
             payload = CASE WHEN $3 THEN '{}'::jsonb ELSE payload END
           WHERE id = $1 AND locked_by = $2 AND status = 'running'"#,
        id,
        worker,
        scrub
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn fail(
    pool: &sqlx::PgPool,
    opts: &WorkerOptions,
    id: i64,
    attempt: i32,
    msg: &str,
) -> Result<()> {
    let wait = backoff_seconds(
        opts.retry_base_seconds,
        opts.retry_max_seconds,
        attempt,
        jitter(),
    );
    let msg: String = msg.chars().take(2000).collect();
    sqlx::query!(
        r#"UPDATE engine_jobs SET
             status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             finished_at = CASE WHEN attempts >= max_attempts THEN now() END,
             run_at = now() + make_interval(secs => $3),
             last_error = $4, locked_by = NULL, locked_until = NULL
           WHERE id = $1 AND locked_by = $2 AND status = 'running'"#,
        id,
        opts.id,
        wait,
        msg
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Put back jobs whose worker stopped renewing the lease (crashed, killed
/// or cut off). Counts as a failed attempt.
async fn reap(pool: &sqlx::PgPool, base: f64, max: f64) -> Result<()> {
    let rows = sqlx::query!(
        r#"UPDATE engine_jobs SET
             status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             finished_at = CASE WHEN attempts >= max_attempts THEN now() END,
             run_at = now() + make_interval(secs => least($1 * power(2, greatest(attempts - 1, 0)), $2)),
             last_error = 'the worker running it stopped before finishing (lease expired; worker '
                          || coalesce(locked_by, '?') || ')',
             locked_by = NULL, locked_until = NULL
           WHERE status = 'running' AND locked_until < now()
           RETURNING id, status"#,
        base,
        max
    )
    .fetch_all(pool)
    .await?;
    for r in rows {
        eprintln!(
            "jobs: job {} lost its worker (lease expired); now {}",
            r.id, r.status
        );
    }
    Ok(())
}

async fn release(pool: &sqlx::PgPool, worker: &str, id: i64) -> Result<()> {
    sqlx::query!(
        r#"UPDATE engine_jobs SET status = 'queued', attempts = greatest(attempts - 1, 0),
             run_at = now(), locked_by = NULL, locked_until = NULL
           WHERE id = $1 AND locked_by = $2 AND status = 'running'"#,
        id,
        worker
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Make the schedule table match the configuration (a changed interval
/// starts counting from now).
async fn sync_schedules(pool: &sqlx::PgPool, schedules: &[Schedule]) -> Result<()> {
    let mut tx = pool.begin().await?;
    let names: Vec<String> = schedules.iter().map(|s| s.name.clone()).collect();
    sqlx::query!(
        "DELETE FROM engine_job_schedules WHERE NOT (name = ANY($1))",
        &names
    )
    .execute(&mut *tx)
    .await?;
    for s in schedules {
        let every = i32::try_from(s.every.as_secs()).unwrap_or(i32::MAX);
        sqlx::query!(
            r#"INSERT INTO engine_job_schedules (name, kind, payload, every_seconds, next_run_at)
               VALUES ($1, $2, $3, $4, now() + make_interval(secs => $4::int4::float8))
               ON CONFLICT (name) DO UPDATE SET kind = EXCLUDED.kind, payload = EXCLUDED.payload,
                 every_seconds = EXCLUDED.every_seconds,
                 next_run_at = CASE WHEN engine_job_schedules.every_seconds = EXCLUDED.every_seconds
                                    THEN engine_job_schedules.next_run_at
                                    ELSE EXCLUDED.next_run_at END"#,
            s.name,
            s.kind,
            s.payload,
            every
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Enqueue every due schedule once (across processes: the rows are locked
/// and skipped by others).
async fn tick_schedules(pool: &sqlx::PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let due = sqlx::query!(
        r#"UPDATE engine_job_schedules s SET last_run_at = now(),
             next_run_at = now() + make_interval(secs => s.every_seconds::float8)
           WHERE s.name IN (SELECT name FROM engine_job_schedules
                            WHERE next_run_at <= now() FOR UPDATE SKIP LOCKED)
           RETURNING s.name, s.kind, s.payload"#
    )
    .fetch_all(&mut *tx)
    .await?;
    for d in &due {
        crate::Jobs::enqueue_in(&mut tx, &d.kind, d.payload.clone(), &Default::default()).await?;
    }
    tx.commit().await?;
    Ok(())
}
