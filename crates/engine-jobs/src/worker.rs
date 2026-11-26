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
use crate::queue::require_pg;
use crate::sql;
use crate::{Handlers, JobContext, backoff_seconds};
use engine_db::Db as Pool;

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

#[derive(sqlx::FromRow)]
struct Claimed {
    id: i64,
    kind: String,
    payload: sqlx::types::Json<serde_json::Value>,
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
    require_pg(&db)?;
    let pool = db.clone();
    let kinds = handlers.kinds();
    sync_schedules(&pool, &opts.schedules).await?;
    let mut listener = match db.pg_listener().await {
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

async fn claim(pool: &Pool, opts: &WorkerOptions, kinds: &[String]) -> Result<Option<Claimed>> {
    let mut tx = pool.begin_relaxed().await?;
    let job = sql::CLAIM
        .bind(opts.id.as_str())
        .bind(opts.lease.as_secs_f64())
        .bind(kinds)
        .fetch_optional(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(job)
}

async fn run_one(
    pool: &Pool,
    opts: &WorkerOptions,
    handlers: &Handlers,
    in_flight: &InFlight,
    job: Claimed,
) {
    let ctx = JobContext {
        id: job.id,
        kind: job.kind.clone(),
        payload: job.payload.0,
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
                match heartbeat(&pool, job_id, &id, lease).await {
                    Ok(0) => {
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

/// Renew a lease; the number of rows touched (0: the lease was lost).
async fn heartbeat(pool: &Pool, job: i64, worker: &str, lease: Duration) -> Result<u64> {
    let mut tx = pool.begin_relaxed().await?;
    let n = sql::HEARTBEAT
        .bind(job)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .execute(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(n)
}

async fn finish(pool: &Pool, worker: &str, id: i64, scrub: bool) -> Result<()> {
    let mut tx = pool.begin_relaxed().await?;
    sql::FINISH
        .bind(id)
        .bind(worker)
        .bind(scrub)
        .execute(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn fail(pool: &Pool, opts: &WorkerOptions, id: i64, attempt: i32, msg: &str) -> Result<()> {
    let wait = backoff_seconds(
        opts.retry_base_seconds,
        opts.retry_max_seconds,
        attempt,
        jitter(),
    );
    let msg: String = msg.chars().take(2000).collect();
    let mut tx = pool.begin_relaxed().await?;
    sql::FAIL
        .bind(id)
        .bind(opts.id.as_str())
        .bind(wait)
        .bind(msg)
        .execute(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Put back jobs whose worker stopped renewing the lease (crashed, killed
/// or cut off). Counts as a failed attempt.
async fn reap(pool: &Pool, base: f64, max: f64) -> Result<()> {
    let mut tx = pool.begin_relaxed().await?;
    let rows: Vec<(i64, String)> = sql::REAP.bind(base).bind(max).fetch_all(&mut tx).await?;
    tx.commit().await?;
    for (id, status) in rows {
        eprintln!("jobs: job {id} lost its worker (lease expired); now {status}");
    }
    Ok(())
}

async fn release(pool: &Pool, worker: &str, id: i64) -> Result<()> {
    sql::RELEASE.bind(id).bind(worker).execute(pool).await?;
    Ok(())
}

/// Make the schedule table match the configuration (a changed interval
/// starts counting from now).
async fn sync_schedules(pool: &Pool, schedules: &[Schedule]) -> Result<()> {
    let mut tx = pool.begin().await?;
    let names: Vec<String> = schedules.iter().map(|s| s.name.clone()).collect();
    sql::DROP_SCHEDULES.bind(names).execute(&mut tx).await?;
    for s in schedules {
        let every = i32::try_from(s.every.as_secs()).unwrap_or(i32::MAX);
        sql::UPSERT_SCHEDULE
            .bind(s.name.as_str())
            .bind(s.kind.as_str())
            .bind(&s.payload)
            .bind(every)
            .execute(&mut tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Enqueue every due schedule once (across processes: the rows are locked
/// and skipped by others).
async fn tick_schedules(pool: &Pool) -> Result<()> {
    let mut tx = pool.begin().await?;
    type Due = (String, String, sqlx::types::Json<serde_json::Value>);
    let due: Vec<Due> = sql::DUE_SCHEDULES.q().fetch_all(&mut tx).await?;
    for (_, kind, payload) in &due {
        crate::Jobs::enqueue_in(&mut tx, kind, payload.0.clone(), &Default::default()).await?;
    }
    tx.commit().await?;
    Ok(())
}
