//! A sleeping PostgreSQL (DECISIONS.md, "Sleeping Postgres watchdog"): the
//! real `engine serve` with the hosting admin's sleep settings, against a
//! PostgreSQL container of its own that the engine stops and starts through
//! the shipped hook (`deploy/hammerwright-db-hook`, `docker:` target).
//!
//! Covers: put to sleep after the idle period (a clean shutdown: exit 0,
//! never killed); no wake while idle with job workers, the publish listener
//! and the search check running; published pages served while asleep; a
//! page rendered per request wakes it (timed: the wake budget); a form post
//! while asleep is spooled at once and written after the wake it starts; an
//! engine started while the database sleeps leaves it asleep until a
//! request needs it; a wake that fails gives the outage answer and shows in
//! the loopback health detail and `engine doctor`.
//!
//! Needs ENGINE_TEST_PGSLEEP_URL, ENGINE_TEST_PGSLEEP_CONTAINER (a throwaway
//! `wrought-test-pgsleep*` container) and ENGINE_TEST_DB_HOOK:
//! scripts/check-db-sleep.sh sets them. Skipped otherwise.

#![cfg(all(feature = "forms", feature = "jobs", feature = "workflow"))]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::*;

fn docker_out(args: &[&str]) -> String {
    let out = Command::new("docker").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn running(container: &str) -> bool {
    docker_out(&["inspect", "-f", "{{.State.Running}}", container]) == "true"
}

fn started_at(container: &str) -> String {
    docker_out(&["inspect", "-f", "{{.State.StartedAt}}", container])
}

fn wait_asleep(container: &str, server: &Server, within: Duration) -> Duration {
    let t = Instant::now();
    while running(container) {
        assert!(
            t.elapsed() < within,
            "not put to sleep within {within:?}:\n{}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // A clean (fast) shutdown: PostgreSQL exits 0; a kill would be 137.
    assert_eq!(
        docker_out(&["inspect", "-f", "{{.State.ExitCode}}", container]),
        "0"
    );
    t.elapsed()
}

/// A number from the database, or `None` while it does not answer (it may
/// be starting).
fn scalar(db: &str, sql: &str) -> Option<i64> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.ok()?;
        let n = engine_db::testing::query_i64(&d, sql).await.ok();
        d.close().await;
        n
    })
}

/// The loopback health detail.
fn health(b: &mut Browser) -> serde_json::Value {
    let r = b.get("/_engine/health");
    serde_json::from_str(&r.body).unwrap_or_else(|_| panic!("{}", r.body))
}

fn median(v: &mut [u128]) -> u128 {
    v.sort();
    v[v.len() / 2]
}

#[test]
fn sleeping_database() {
    let (Ok(db), Ok(container), Ok(hook)) = (
        std::env::var("ENGINE_TEST_PGSLEEP_URL"),
        std::env::var("ENGINE_TEST_PGSLEEP_CONTAINER"),
        std::env::var("ENGINE_TEST_DB_HOOK"),
    ) else {
        return;
    };
    assert!(
        container.starts_with("wrought-test-pgsleep"),
        "refusing to stop a container not made for this test: {container}"
    );
    let budget: u128 = std::env::var("HAMMERWRIGHT_DBSLEEP_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/small-app"),
        &site,
    );
    std::fs::write(
        site.join("templates/latest.html"),
        "{% extends \"layout.html\" %}{% block main %}<ul>{% for u in list_items(\"update\", limit=3) %}<li>{{ u.title }}</li>{% endfor %}</ul>{% endblock %}",
    )
    .unwrap();
    let toml = std::fs::read_to_string(site.join("site.toml")).unwrap();
    std::fs::write(
        site.join("site.toml"),
        format!(
            "{toml}\n[[routes]]\npath = \"/latest/\"\ntemplate = \"latest.html\"\nmode = \"request\"\ntitle = \"Latest\"\n"
        ),
    )
    .unwrap();
    let s = site.to_str().unwrap();
    engine(&db, &["migrate", "--site", s], None);
    engine(&db, &["content", "seed", "--site", s], None);
    engine(&db, &["build", "--site", s], None);

    // The hosting admin's file: the test site sleeps after 2 s unused; the
    // `broken` one has a wake command that fails.
    let target = format!("docker:{container}");
    let host = dir.path().join("host.toml");
    std::fs::write(
        &host,
        format!(
            "[sites.t]\ndb_sleep_seconds = 2\ndb_wake_command = [\"{hook}\", \"wake\", \"{target}\"]\n\
             db_sleep_command = [\"{hook}\", \"sleep\", \"{target}\"]\n\
             [sites.broken]\ndb_sleep_seconds = 2\ndb_wake_command = [\"false\"]\n\
             db_sleep_command = [\"{hook}\", \"sleep\", \"{target}\"]\n"
        ),
    )
    .unwrap();
    let spool = dir.path().join("spool");
    let spawn = |id: &str, log: &str| {
        let port = free_port();
        let logf = dir.path().join(log);
        let f = std::fs::File::create(&logf).unwrap();
        // Job workers on (their polling must not wake the database); no
        // SMTP server, so mail waits in the queue unhandled.
        let child = Command::new(env!("CARGO_BIN_EXE_engine"))
            .args(["serve", "--site", s, "--port", &port.to_string()])
            .env("ENGINE_DATABASE_URL", &db)
            .env("ENGINE_SECRET_KEY", "0123456789abcdef0123456789abcdef")
            .env("ENGINE_FORM_SPOOL_DIR", &spool)
            .env("ENGINE_HOST_CONFIG", &host)
            .env("ENGINE_SITE_ID", id)
            .env_remove("ENGINE_SMTP_URL")
            .stdout(f.try_clone().unwrap())
            .stderr(f)
            .spawn()
            .unwrap();
        let server = Server { child, log: logf };
        wait_for("engine serve", || {
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        (server, port)
    };

    let (server, port) = spawn("t", "serve-t.log");
    let mut b = Browser::new(port);
    let t0 = Instant::now();
    assert_eq!(b.get("/latest/").status, 200, "{}", server.log());
    let page = b.get("/support/");
    assert_eq!(page.status, 200);
    let csrf = hidden(&page.body, "_csrf");
    let stamp = hidden(&page.body, "_t");
    let mem = docker_out(&[
        "stats",
        "--no-stream",
        "--format",
        "{{.MemUsage}}",
        &container,
    ]);
    println!("postgres memory while awake and idle: {mem}");

    // 1. Put to sleep after the idle period, cleanly.
    wait_asleep(&container, &server, Duration::from_secs(30));
    println!(
        "put to sleep {:.1} s after the last use",
        t0.elapsed().as_secs_f64()
    );
    let h = health(&mut b);
    assert_eq!(h["status"], "ok", "asleep is not degraded: {h}");
    assert_eq!(h["database"]["sleep"]["asleep"], true, "{h}");
    assert!(server.log().contains("put to sleep"), "{}", server.log());

    // 2. Idle with job workers, the publish listener and the search check
    // running: no wake at all.
    let since = started_at(&container);
    std::thread::sleep(Duration::from_secs(20));
    assert!(!running(&container), "woken while idle:\n{}", server.log());
    assert_eq!(started_at(&container), since);
    assert_eq!(health(&mut b)["database"]["sleep"]["wakes"], 0);
    println!("idle 20 s with workers running: 0 wakes");

    // 3. Published pages while asleep, without waking it.
    let t = Instant::now();
    assert_eq!(b.get("/").status, 200);
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!running(&container));

    // 4. A page rendered per request wakes it: asleep to the first page.
    let mut wakes = Vec::new();
    for i in 0..5 {
        if i > 0 {
            wait_asleep(&container, &server, Duration::from_secs(30));
        }
        let t = Instant::now();
        let r = b.get("/latest/");
        let ms = t.elapsed().as_millis();
        assert_eq!(r.status, 200, "{}\n{}", r.body, server.log());
        println!("wake: {ms} ms (asleep to the first per-request page)");
        wakes.push(ms);
    }
    let med = median(&mut wakes);
    println!("wake median: {med} ms over {} wakes", wakes.len());
    assert!(
        med <= budget,
        "wake median {med} ms over the budget {budget} ms"
    );

    // 5. A form post while asleep: spooled at once, written after the wake
    // it starts.
    wait_asleep(&container, &server, Duration::from_secs(30));
    let t = Instant::now();
    let r = b.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", &csrf),
            ("_page", "/support/"),
            ("_t", &stamp),
            ("name", "Ann Example"),
            ("email", "ann@example.test"),
            ("topic", "Bug"),
            ("priority", "High"),
            ("message", "posted while the database slept"),
            ("website", ""),
        ],
    );
    assert_eq!(r.status, 303, "{}\n{}", r.body, server.log());
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    let t = Instant::now();
    loop {
        if running(&container)
            && scalar(
                &db,
                "SELECT count(*) FROM engine_form_submissions WHERE data->>'message' = \
                 'posted while the database slept'",
            ) == Some(1)
        {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "{}", server.log());
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("spooled post written {:?} after it was sent", t.elapsed());
    wait_asleep(&container, &server, Duration::from_secs(30));
    drop(server);

    // 6. An engine started while the database sleeps (the wake timer, a
    // published page) leaves it asleep; a request that needs it wakes it.
    let state = site.join(".engine/db-sleep.json");
    assert!(state.is_file(), "the sleep is recorded");
    let since = started_at(&container);
    let (server, port) = spawn("t", "serve-t2.log");
    b.port = port;
    assert_eq!(b.get("/").status, 200);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !running(&container),
        "woken by the start:\n{}",
        server.log()
    );
    assert_eq!(started_at(&container), since);
    assert!(server.log().contains("is asleep"), "{}", server.log());
    let t = Instant::now();
    let r = b.get("/latest/");
    assert_eq!(r.status, 200, "{}\n{}", r.body, server.log());
    println!(
        "engine started with the database asleep: first per-request page after {} ms",
        t.elapsed().as_millis()
    );
    wait_asleep(&container, &server, Duration::from_secs(30));
    drop(server);

    // 7. A wake that fails: the outage answer, logged, in the loopback
    // health detail and in `engine doctor`.
    docker_out(&["start", &container]);
    let t = Instant::now();
    let (server, port) = spawn("broken", "serve-broken.log");
    b.port = port;
    while b.get("/latest/").status != 200 {
        assert!(t.elapsed() < Duration::from_secs(30), "{}", server.log());
        std::thread::sleep(Duration::from_millis(200));
    }
    wait_asleep(&container, &server, Duration::from_secs(30));
    let t = Instant::now();
    let r = b.get("/latest/");
    assert_eq!(r.status, 503, "{}", server.log());
    assert_eq!(r.header("retry-after"), Some("30"));
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    let h = health(&mut b);
    assert_eq!(h["database"]["sleep"]["wake_failures"], 1, "{h}");
    assert!(
        h["database"]["last_error"]
            .as_str()
            .is_some_and(|e| e.contains("wake failed")),
        "{h}"
    );
    assert!(server.log().contains("wake failed"), "{}", server.log());
    let t = Instant::now();
    loop {
        let out = engine_env(
            &db,
            &["doctor", "--site", s],
            None,
            &[
                ("ENGINE_HOST_CONFIG", host.to_str().unwrap()),
                ("ENGINE_SITE_ID", "broken"),
            ],
        );
        if out.contains("LAST WAKE FAILED") {
            assert!(out.contains("asleep for"), "{out}");
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(15), "{out}");
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(server);
    docker_out(&["start", &container]);
}
