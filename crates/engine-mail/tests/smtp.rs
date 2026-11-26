//! Mail through the job queue to a real SMTP sink (Mailpit), read back
//! through its API. Skipped without ENGINE_TEST_DATABASE_URL,
//! ENGINE_TEST_SMTP_URL and ENGINE_TEST_MAILPIT_URL.

use std::time::Duration;

use engine_db::migrate;
use engine_db::testing::{TestDb, query_i64};
use engine_jobs::{Handlers, Jobs, WorkerOptions};
use engine_mail::testing::Mailpit;
use engine_mail::{MailConfig, Mailer};

fn options() -> WorkerOptions {
    WorkerOptions {
        id: engine_jobs::worker_id(),
        concurrency: 2,
        poll: Duration::from_millis(100),
        lease: Duration::from_secs(30),
        grace: Duration::from_secs(5),
        retry_base_seconds: 0.1,
        retry_max_seconds: 0.5,
        timeout: Duration::from_secs(30),
        schedules: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_mail_reaches_the_smtp_server_and_the_payload_is_scrubbed() {
    let Some(mp) = Mailpit::from_env("queued_mail_reaches_the_smtp_server") else {
        return;
    };
    let Some(t) = TestDb::create_postgres("queued_mail_reaches_the_smtp_server").await else {
        return;
    };
    migrate::apply(&t.db, &engine_jobs::migrations(t.db.backend()))
        .await
        .unwrap();
    let jobs = Jobs::new(t.db.clone());
    let table: toml::Table = toml::from_str(&format!(
        "from = 'Test Site <noreply@example.test>'\nsmtp_url = '{}'",
        mp.smtp_url
    ))
    .unwrap();
    let cfg = MailConfig::from_site(Some(&table)).unwrap();
    let dir = std::env::temp_dir();
    let mailer = Mailer::new(jobs.clone(), cfg, &dir, "Test Site", "https://site.example").unwrap();
    assert!(mailer.configured());
    let to = Mailpit::address("reset");
    let id = mailer
        .send(
            &to,
            "Ada",
            "password_reset",
            serde_json::json!({ "link": "https://site.example/account/reset/confirm/?token=abc&x=1", "valid_minutes": 60 }),
        )
        .await
        .unwrap();
    // A message to a broken address is refused before it is queued.
    assert!(
        mailer
            .send(
                "not an address",
                "",
                "password_reset",
                serde_json::json!({})
            )
            .await
            .is_err()
    );
    let mut h = Handlers::new();
    mailer.register(&mut h);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let w = tokio::spawn(engine_jobs::run(t.db.clone(), h, options(), async {
        let _ = rx.await;
    }));
    let got = tokio::task::spawn_blocking(move || mp.wait_for(&to, 1, Duration::from_secs(20)))
        .await
        .unwrap()
        .unwrap();
    let _ = tx.send(());
    w.await.unwrap().unwrap();
    let m = &got[0];
    assert_eq!(m.subject, "Reset your password for Test Site");
    assert_eq!(m.from, "noreply@example.test");
    assert!(
        m.text
            .contains("https://site.example/account/reset/confirm/?token=abc&x=1"),
        "{}",
        m.text
    );
    assert!(m.html.contains("token=abc&amp;x=1"), "{}", m.html);
    let info = jobs.job(id).await.unwrap().unwrap();
    assert_eq!(info.status, "done");
    assert_eq!(info.payload, serde_json::json!({}), "the link is not kept");
    assert_eq!(
        query_i64(
            &t.db,
            "SELECT count(*) FROM engine_jobs WHERE status <> 'done'"
        )
        .await
        .unwrap(),
        0
    );
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_an_smtp_server_mail_fails_and_is_retried() {
    let Some(t) = TestDb::create_postgres("without_an_smtp_server_mail_fails").await else {
        return;
    };
    migrate::apply(&t.db, &engine_jobs::migrations(t.db.backend()))
        .await
        .unwrap();
    let jobs = Jobs::new(t.db.clone());
    // Nothing listens on port 9 (discard) here: connection refused.
    let table: toml::Table =
        toml::from_str("from = 'x@example.test'\nsmtp_url = 'smtp://127.0.0.1:9'\nmax_attempts = 2\ntimeout_seconds = 2").unwrap();
    let mailer = Mailer::new(
        jobs.clone(),
        MailConfig::from_site(Some(&table)).unwrap(),
        &std::env::temp_dir(),
        "S",
        "https://s.example",
    )
    .unwrap();
    let id = mailer
        .send("a@example.test", "", "welcome", serde_json::json!({}))
        .await
        .unwrap();
    let mut h = Handlers::new();
    mailer.register(&mut h);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let w = tokio::spawn(engine_jobs::run(t.db.clone(), h, options(), async {
        let _ = rx.await;
    }));
    let start = std::time::Instant::now();
    loop {
        let j = jobs.job(id).await.unwrap().unwrap();
        if j.status == "dead" {
            assert_eq!(j.attempts, 2);
            assert!(j.last_error.unwrap().contains("SMTP"));
            assert_ne!(
                j.payload,
                serde_json::json!({}),
                "undelivered mail is kept for a retry"
            );
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "never died: {j:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = tx.send(());
    w.await.unwrap().unwrap();
    t.cleanup().await;
}
