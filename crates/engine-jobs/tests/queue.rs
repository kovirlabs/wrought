//! The job queue against a real PostgreSQL (skipped without
//! ENGINE_TEST_DATABASE_URL): exclusive claims across workers, crash
//! recovery through leases, retries with backoff into `dead`, graceful
//! shutdown and schedules.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use engine_db::testing::TestDb;
use engine_db::{Db, DbConfig, migrate};
use engine_jobs::{Handlers, JobContext, Jobs, Schedule, WorkerOptions};
use serde_json::json;

fn options(lease: f64) -> WorkerOptions {
    WorkerOptions {
        id: engine_jobs::worker_id(),
        concurrency: 8,
        poll: Duration::from_millis(100),
        lease: Duration::from_secs_f64(lease),
        grace: Duration::from_secs(5),
        retry_base_seconds: 0.05,
        retry_max_seconds: 0.2,
        timeout: Duration::from_secs(30),
        schedules: Vec::new(),
    }
}

async fn setup(test: &str) -> Option<(TestDb, Db)> {
    let t = TestDb::create(test).await?;
    migrate::apply(&t.db, &engine_jobs::migrations(t.db.backend()))
        .await
        .unwrap();
    // Workers get their own pool, like a separate process would.
    let mut c = DbConfig::new(t.url.clone());
    c.max_connections = 6;
    let db = Db::connect(&c).await.unwrap();
    Some((t, db))
}

fn pool(db: &Db) -> &sqlx::PgPool {
    db.postgres().unwrap()
}

async fn wait_until<F, Fut>(what: &str, timeout: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while !f().await {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn status_count(db: &Db, status: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM engine_jobs WHERE status = $1")
        .bind(status)
        .fetch_one(pool(db))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_never_run_the_same_job() {
    let Some((t, db)) = setup("two_workers_never_run_the_same_job").await else {
        return;
    };
    sqlx::raw_sql("CREATE TABLE ran (job_id bigint NOT NULL, worker text NOT NULL)")
        .execute(pool(&db))
        .await
        .unwrap();
    let jobs = Jobs::new(db.clone());
    const N: i64 = 300;
    for i in 0..N {
        jobs.enqueue("test.count", json!({ "i": i })).await.unwrap();
    }
    let mut stops = Vec::new();
    let mut tasks = Vec::new();
    for w in ["a", "b"] {
        let mut h = Handlers::new();
        let d = db.clone();
        h.register("test.count", move |ctx: JobContext| {
            let d = d.clone();
            async move {
                // Overlap the two workers' claims as much as possible.
                tokio::time::sleep(Duration::from_millis(u64::from(ctx.id as u32 % 5))).await;
                sqlx::query("INSERT INTO ran (job_id, worker) VALUES ($1, $2)")
                    .bind(ctx.id)
                    .bind(w)
                    .execute(d.postgres().unwrap())
                    .await?;
                Ok(())
            }
        });
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        stops.push(tx);
        tasks.push(tokio::spawn(engine_jobs::run(
            db.clone(),
            h,
            options(30.0),
            async {
                let _ = rx.await;
            },
        )));
    }
    wait_until("all jobs done", Duration::from_secs(60), || async {
        status_count(&db, "done").await == N
    })
    .await;
    for s in stops {
        let _ = s.send(());
    }
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    let (total, distinct): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(DISTINCT job_id) FROM ran")
            .fetch_one(pool(&db))
            .await
            .unwrap();
    assert_eq!((total, distinct), (N, N), "every job ran exactly once");
    let per_worker: Vec<(String, i64)> =
        sqlx::query_as("SELECT worker, count(*) FROM ran GROUP BY worker ORDER BY worker")
            .fetch_all(pool(&db))
            .await
            .unwrap();
    assert_eq!(
        per_worker.len(),
        2,
        "both workers took jobs: {per_worker:?}"
    );
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM engine_jobs WHERE attempts <> 1")
        .fetch_one(pool(&db))
        .await
        .unwrap();
    assert_eq!(attempts, 0);
    db.close().await;
    t.cleanup().await;
}

/// A worker process dies mid-job (its runtime is torn down: no graceful
/// shutdown, no more heartbeats). Another worker picks the job up once the
/// lease runs out, and it completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crashed_job_is_retried() {
    let Some((t, db)) = setup("a_crashed_job_is_retried").await else {
        return;
    };
    let jobs = Jobs::new(db.clone());
    let id = jobs.enqueue("test.crashy", json!({})).await.unwrap();

    // Worker A, in its own runtime: starts the job and hangs.
    let started = Arc::new(AtomicUsize::new(0));
    let url = t.url.clone();
    let s = started.clone();
    let crashed = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.spawn(async move {
            let db = Db::connect(&DbConfig::new(url)).await.unwrap();
            let mut h = Handlers::new();
            h.register("test.crashy", move |_ctx: JobContext| {
                let s = s.clone();
                async move {
                    s.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                    Ok(())
                }
            });
            let _ = engine_jobs::run(db, h, options(3.0), std::future::pending()).await;
        });
        let start = Instant::now();
        while started.load(Ordering::SeqCst) == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "worker A never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // Crash: drop every task of that "process" at once.
        rt.shutdown_background();
    });
    crashed.join().unwrap();
    let info = jobs.job(id).await.unwrap().unwrap();
    assert_eq!(info.status, "running");
    assert_eq!(info.attempts, 1);

    // Worker B finishes it after the lease (3 s) runs out. (A short lease
    // on a loaded machine can expire under a live worker too: it is a
    // timeout, so keep it well above heartbeat delays.)
    let ran = Arc::new(AtomicUsize::new(0));
    let r = ran.clone();
    let mut h = Handlers::new();
    h.register("test.crashy", move |ctx: JobContext| {
        let r = r.clone();
        async move {
            assert_eq!(ctx.attempt, 2);
            r.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let b = tokio::spawn(engine_jobs::run(db.clone(), h, options(3.0), async {
        let _ = rx.await;
    }));
    wait_until(
        "the crashed job to be retried",
        Duration::from_secs(20),
        || async { jobs.job(id).await.unwrap().unwrap().status == "done" },
    )
    .await;
    let _ = tx.send(());
    b.await.unwrap().unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    let info = jobs.job(id).await.unwrap().unwrap();
    assert_eq!(info.attempts, 2);
    db.close().await;
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failures_back_off_then_die_and_can_be_retried() {
    let Some((t, db)) = setup("failures_back_off_then_die_and_can_be_retried").await else {
        return;
    };
    let jobs = Jobs::new(db.clone());
    let id = jobs
        .enqueue_with(
            "test.fail",
            json!({}),
            engine_jobs::Enqueue {
                max_attempts: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let panicky = jobs.enqueue("test.panic", json!({})).await.unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let mut h = Handlers::new();
    h.register("test.fail", move |_ctx: JobContext| {
        let r = r.clone();
        async move {
            r.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("smtp server said no")
        }
    });
    h.register("test.panic", |ctx: JobContext| async move {
        if ctx.attempt == 1 {
            panic!("boom");
        }
        Ok(())
    });
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let w = tokio::spawn(engine_jobs::run(db.clone(), h, options(30.0), async {
        let _ = rx.await;
    }));
    wait_until("the job to die", Duration::from_secs(20), || async {
        jobs.job(id).await.unwrap().unwrap().status == "dead"
    })
    .await;
    wait_until(
        "the panicking job to recover",
        Duration::from_secs(20),
        || async { jobs.job(panicky).await.unwrap().unwrap().status == "done" },
    )
    .await;
    let info = jobs.job(id).await.unwrap().unwrap();
    assert_eq!(info.attempts, 3);
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert!(
        info.last_error
            .as_deref()
            .unwrap()
            .contains("smtp server said no")
    );
    assert!(info.finished_at.is_some());
    // A dead job stays dead until retried by hand.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert!(jobs.retry(id).await.unwrap());
    assert!(!jobs.retry(panicky).await.unwrap(), "only dead jobs");
    wait_until(
        "the retried job to die again",
        Duration::from_secs(20),
        || async {
            runs.load(Ordering::SeqCst) == 6
                && jobs.job(id).await.unwrap().unwrap().status == "dead"
        },
    )
    .await;
    let _ = tx.send(());
    w.await.unwrap().unwrap();
    db.close().await;
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_finishes_or_releases_running_jobs() {
    let Some((t, db)) = setup("shutdown_finishes_or_releases_running_jobs").await else {
        return;
    };
    let jobs = Jobs::new(db.clone());
    let quick = jobs.enqueue("test.quick", json!({})).await.unwrap();
    let slow = jobs.enqueue("test.slow", json!({})).await.unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let mut h = Handlers::new();
    let s = started.clone();
    h.register("test.quick", move |_ctx: JobContext| {
        let s = s.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        }
    });
    let s = started.clone();
    h.register("test.slow", move |_ctx: JobContext| {
        let s = s.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        }
    });
    let mut opts = options(30.0);
    opts.grace = Duration::from_secs(1);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let w = tokio::spawn(engine_jobs::run(db.clone(), h, opts, async {
        let _ = rx.await;
    }));
    wait_until("both jobs to start", Duration::from_secs(10), || async {
        started.load(Ordering::SeqCst) == 2
    })
    .await;
    let t0 = Instant::now();
    let _ = tx.send(());
    w.await.unwrap().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(jobs.job(quick).await.unwrap().unwrap().status, "done");
    let s = jobs.job(slow).await.unwrap().unwrap();
    assert_eq!(s.status, "queued", "released, not failed");
    assert_eq!(s.attempts, 0, "a release does not count as an attempt");
    db.close().await;
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schedules_enqueue_once_per_interval_across_processes() {
    let Some((t, db)) = setup("schedules_enqueue_once_per_interval_across_processes").await else {
        return;
    };
    let runs = Arc::new(AtomicUsize::new(0));
    let mut stops = Vec::new();
    let mut tasks = Vec::new();
    // Two "processes" with the same schedule.
    for _ in 0..2 {
        let r = runs.clone();
        let mut h = Handlers::new();
        h.register("test.tick", move |_ctx: JobContext| {
            let r = r.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let mut opts = options(30.0);
        opts.schedules = vec![Schedule {
            name: "tick".into(),
            kind: "test.tick".into(),
            every: Duration::from_secs(1),
            payload: json!({}),
        }];
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        stops.push(tx);
        tasks.push(tokio::spawn(engine_jobs::run(db.clone(), h, opts, async {
            let _ = rx.await;
        })));
    }
    // Wait for a few runs (load on a busy machine can delay the start),
    // then check the schedule fired once per interval, not once per process.
    let runs_now = runs.clone();
    wait_until("three scheduled runs", Duration::from_secs(30), || {
        let r = runs_now.clone();
        async move { r.load(Ordering::SeqCst) >= 3 }
    })
    .await;
    for s in stops {
        let _ = s.send(());
    }
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    let (n, span): (i64, f64) = sqlx::query_as(
        "SELECT count(*), coalesce(extract(epoch FROM max(created_at) - min(created_at))::float8, 0)
         FROM engine_jobs WHERE kind = 'test.tick'",
    )
    .fetch_one(pool(&db))
    .await
    .unwrap();
    assert!(n >= 3);
    assert!(
        (n as f64) <= span + 1.5,
        "at most one job per 1 s interval across both processes: {n} jobs in {span:.2} s"
    );
    db.close().await;
    t.cleanup().await;
}
