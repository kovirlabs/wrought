//! Surviving a sleeping or restarting database (DECISIONS.md, "Survive a
//! sleeping database"): the real `engine serve` against a PostgreSQL
//! container of its own that the test stops, kills and starts.
//!
//! Covers: a restart in the middle of traffic (no `500`, no hang, pages
//! rendered per request work again without a restart of the engine);
//! published pages served throughout; a per-request page with a published
//! copy served from it, one without answering `503` with `Retry-After`;
//! form posts during the outage spooled (0600 files) and written to the
//! database in order, exactly once, with their mail queued once, when it is
//! back; the engine killed between spooling and replay; a replay repeated
//! (a crash after commit, before the file was removed); a full spool
//! refused with a clear message; `/_engine/health`.
//!
//! Needs ENGINE_TEST_PGSLEEP_URL and ENGINE_TEST_PGSLEEP_CONTAINER (a
//! throwaway `wrought-test-pgsleep*` container: scripts/check-db-outage.sh
//! starts and removes it). Skipped otherwise. Never touches any other
//! container.

#![cfg(all(feature = "forms", feature = "mail", feature = "workflow"))]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::*;

fn docker(args: &[&str]) {
    let out = Command::new("docker").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scalar(db: &str, sql: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        let n = engine_db::testing::query_i64(&d, sql).await.unwrap();
        d.close().await;
        n
    })
}

/// Wait until the database answers a query again.
fn wait_db(db: &str) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let start = Instant::now();
    loop {
        let ok = rt.block_on(async {
            let mut c = engine_db::DbConfig::new(db.to_string());
            c.max_connections = 1;
            match engine_db::Db::connect(&c).await {
                Ok(d) => {
                    let r = engine_db::testing::query_i64(&d, "SELECT 1::bigint").await;
                    d.close().await;
                    r.is_ok()
                }
                Err(_) => false,
            }
        });
        if ok {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "database not back"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn spool_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|x| x == "json"));
    v.sort();
    v
}

struct Form {
    csrf: String,
    stamp: String,
}

fn post(b: &mut Browser, f: &Form, message: &str) -> Resp {
    post_from(b, f, message, None)
}

/// A post from `client` (through the front door: `X-Forwarded-For`).
fn post_from(b: &mut Browser, f: &Form, message: &str, client: Option<&str>) -> Resp {
    let fwd: Vec<(&str, &str)> = client.map(|c| ("X-Forwarded-For", c)).into_iter().collect();
    b.post_with(
        "/_engine/forms/support/",
        &[
            ("_csrf", &f.csrf),
            ("_page", "/support/"),
            ("_t", &f.stamp),
            ("_step", "0"),
            ("name", "Ann Example"),
            ("email", "ann@example.test"),
            ("topic", "Bug"),
            ("priority", "High"),
            ("message", message),
            ("website", ""),
        ],
        &fwd,
    )
}

fn count_msg(db: &str, message: &str) -> i64 {
    scalar(
        db,
        &format!(
            "SELECT count(*) FROM engine_form_submissions WHERE data->>'message' = '{message}'"
        ),
    )
}

fn mail_jobs(db: &str) -> i64 {
    scalar(
        db,
        "SELECT count(*) FROM engine_jobs WHERE kind = 'mail.send'",
    )
}

#[test]
fn database_outage_and_form_spool() {
    let (Ok(db), Ok(container)) = (
        std::env::var("ENGINE_TEST_PGSLEEP_URL"),
        std::env::var("ENGINE_TEST_PGSLEEP_CONTAINER"),
    ) else {
        return;
    };
    assert!(
        container.starts_with("wrought-test-pgsleep"),
        "refusing to stop a container not made for this test: {container}"
    );
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/small-app"),
        &site,
    );
    // A page rendered per request from the database, with no published
    // copy; and a spool of 1 MiB.
    std::fs::write(
        site.join("templates/latest.html"),
        "{% extends \"layout.html\" %}{% block main %}<ul>{% for u in list_items(\"update\", limit=3) %}<li>{{ u.title }}</li>{% endfor %}</ul>{% endblock %}",
    )
    .unwrap();
    // Behind a proxy, so the test can post as different clients.
    let toml = std::fs::read_to_string(site.join("site.toml"))
        .unwrap()
        .replace("[auth]\n", "[auth]\nbehind_proxy = true\n");
    std::fs::write(
        site.join("site.toml"),
        format!(
            "{toml}\n[[routes]]\npath = \"/latest/\"\ntemplate = \"latest.html\"\nmode = \"request\"\ntitle = \"Latest\"\n\n[limits]\nform_spool_mb = 1\n"
        ),
    )
    .unwrap();
    let s = site.to_str().unwrap();
    engine(&db, &["migrate", "--site", s], None);
    engine(&db, &["content", "seed", "--site", s], None);
    engine(&db, &["build", "--site", s], None);
    let spool = dir.path().join("spool");
    let sp = spool.to_str().unwrap().to_string();
    // Mail is queued (no workers here), never sent.
    let env = [
        ("ENGINE_FORM_SPOOL_DIR", sp.as_str()),
        ("ENGINE_SMTP_URL", "smtp://127.0.0.1:9"),
    ];
    let start_engine = |log: &str| {
        let port = free_port();
        let logf = dir.path().join(log);
        let logf2 = std::fs::File::create(&logf).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_engine"))
            .args([
                "serve",
                "--site",
                s,
                "--port",
                &port.to_string(),
                "--no-workers",
            ])
            .env("ENGINE_DATABASE_URL", &db)
            .env("ENGINE_DATABASE_MAX_CONNECTIONS", "4")
            .env("ENGINE_SECRET_KEY", "0123456789abcdef0123456789abcdef")
            .env("ENGINE_PUBLIC_URL", format!("http://localhost:{port}"))
            .envs(env)
            .stdout(logf2.try_clone().unwrap())
            .stderr(logf2)
            .spawn()
            .unwrap();
        let server = Server { child, log: logf };
        wait_for("engine serve", || {
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        (server, port)
    };
    let (mut server, port) = start_engine("serve-1.log");
    let mut b = Browser::new(port);
    assert_eq!(b.get("/latest/").status, 200);
    let page = b.get("/support/");
    assert_eq!(page.status, 200);
    let form = Form {
        csrf: hidden(&page.body, "_csrf"),
        stamp: hidden(&page.body, "_t"),
    };
    std::thread::sleep(Duration::from_millis(2100)); // the time trap
    assert_eq!(post(&mut b, &form, "before the outage").status, 303);
    assert_eq!(count_msg(&db, "before the outage"), 1);
    assert_eq!(mail_jobs(&db), 2, "receipt and notice queued");

    // 1. A restart in the middle of traffic.
    let stop = Arc::new(AtomicBool::new(false));
    let traffic = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut b = Browser::new(port);
            let mut seen: Vec<(String, u16, Duration)> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                for p in ["/updates/", "/latest/", "/?from=test"] {
                    let t = Instant::now();
                    let r = b.get(p);
                    seen.push((p.to_string(), r.status, t.elapsed()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            seen
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    docker(&["restart", "-t", "2", &container]);
    wait_db(&db);
    // Pages rendered per request work again with no engine restart.
    let back = Instant::now();
    while b.get("/latest/").status != 200 {
        assert!(back.elapsed() < Duration::from_secs(15), "not recovered");
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "per-request page back {:?} after the database",
        back.elapsed()
    );
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    let seen = traffic.join().unwrap();
    assert!(seen.len() > 30, "traffic ran: {}", seen.len());
    for (p, st, took) in &seen {
        assert!(*took < Duration::from_secs(5), "{p} took {took:?}");
        match p.as_str() {
            "/updates/" | "/?from=test" => assert_eq!(*st, 200, "{p}: published copy"),
            _ => assert!(*st == 200 || *st == 503, "{p}: {st}"),
        }
    }

    // 2. Posts while the database is stopped: spooled, answered as usual.
    docker(&["stop", "-t", "2", &container]);
    let r = b.get("/latest/");
    assert_eq!(r.status, 503, "no published copy: 503");
    assert!(r.header("retry-after").is_some());
    assert_eq!(
        b.get("/updates/").status,
        200,
        "published pages still served"
    );
    assert_eq!(
        b.get("/?from=test").status,
        200,
        "published copy of a per-request render"
    );
    let h = b.get("/_engine/health");
    assert_eq!(h.status, 503, "{}", h.body);
    assert!(h.body.contains("\"up\":false"), "{}", h.body);
    for m in ["outage one", "outage two", "outage three"] {
        let r = post(&mut b, &form, m);
        assert_eq!(r.status, 303, "{m}: {}", r.body);
    }
    let files = spool_files(&spool);
    assert_eq!(files.len(), 3);
    for f in &files {
        assert_eq!(
            std::fs::metadata(f).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    docker(&["start", &container]);
    wait_db(&db);
    wait_for("the spool replayed", || spool_files(&spool).is_empty());
    for m in ["outage one", "outage two", "outage three"] {
        assert_eq!(count_msg(&db, m), 1, "{m} stored once");
    }
    // In order.
    let ordered = scalar(
        &db,
        "SELECT count(*) FROM (SELECT data->>'message' AS m, id FROM engine_form_submissions
            WHERE data->>'message' LIKE 'outage %') s
         WHERE (m = 'outage one' AND id < ALL (SELECT id FROM engine_form_submissions WHERE data->>'message' IN ('outage two','outage three')))
            OR (m = 'outage two' AND id < (SELECT id FROM engine_form_submissions WHERE data->>'message' = 'outage three'))",
    );
    assert_eq!(ordered, 2, "replayed in the order they arrived");
    assert_eq!(mail_jobs(&db), 8, "mail queued once per replayed post");
    let h = b.get("/_engine/health");
    assert_eq!(h.status, 200, "{}", h.body);
    assert!(h.body.contains("\"posts\":0"), "{}", h.body);

    // 3. The engine killed between spooling and replay.
    docker(&["stop", "-t", "2", &container]);
    assert_eq!(post(&mut b, &form, "before the crash").status, 303);
    assert_eq!(spool_files(&spool).len(), 1);
    let kept = spool_files(&spool)[0].clone();
    let copy = std::fs::read(&kept).unwrap();
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    docker(&["start", &container]);
    wait_db(&db);
    let (s2, port2) = start_engine("serve-2.log");
    server = s2;
    wait_for("the spool replayed after a restart", || {
        spool_files(&spool).is_empty()
    });
    assert_eq!(count_msg(&db, "before the crash"), 1);
    assert_eq!(mail_jobs(&db), 10);

    // 4. The same post replayed again (a crash after the commit, before
    // the file was removed): found claimed, removed, nothing doubled. And a
    // post the database refuses (a NUL character jsonb does not take): set
    // aside after a few tries, not in the way of the posts after it.
    std::fs::write(&kept, &copy).unwrap();
    std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o600)).unwrap();
    let poison_name = "00000000000000000000-0123456789abcdef0123456789abcdef.json";
    let poison = String::from_utf8(copy.clone())
        .unwrap()
        .replace("before the crash", "poison\\u0000post")
        .replace(
            &kept
                .file_name()
                .unwrap()
                .to_string_lossy()
                .split(['-', '.'])
                .nth(1)
                .unwrap()
                .to_string(),
            "0123456789abcdef0123456789abcdef",
        );
    std::fs::write(spool.join(poison_name), poison).unwrap();
    std::fs::set_permissions(
        spool.join(poison_name),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    drop(server);
    let (s3, port3) = start_engine("serve-3.log");
    server = s3;
    let _ = port2;
    // The refused post is tried MAX_TRIES times, 5 s apart.
    let t = Instant::now();
    while !spool_files(&spool).is_empty() {
        assert!(t.elapsed() < Duration::from_secs(45), "{}", server.log());
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(count_msg(&db, "before the crash"), 1, "not stored twice");
    assert_eq!(mail_jobs(&db), 10, "no mail queued twice");
    let q = spool.join("quarantine").join(poison_name);
    assert!(q.is_file(), "set aside: {}", server.log());
    assert_eq!(
        std::fs::metadata(&q).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(server.log().contains("set aside in"), "logged");
    let mut b = Browser::new(port3);
    let h = b.get("/_engine/health");
    assert!(h.body.contains("\"quarantined\":1"), "{}", h.body);
    // The next post goes straight to the database.
    let page = b.get("/support/");
    let form = Form {
        csrf: hidden(&page.body, "_csrf"),
        stamp: hidden(&page.body, "_t"),
    };
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(post(&mut b, &form, "after the poison").status, 303);
    assert_eq!(count_msg(&db, "after the poison"), 1, "not held up");
    assert!(spool_files(&spool).is_empty());

    // 5. The health endpoint: no detail through the front door, and
    // `?check` cannot be used to hammer the database.
    let probes = |b: &mut Browser| -> i64 {
        let h = b.get("/_engine/health");
        let i = h
            .body
            .find("\"probes\":")
            .expect("local callers see the detail")
            + 9;
        h.body[i..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let before = probes(&mut b);
    for _ in 0..30 {
        let h = b.request(
            "GET",
            "/_engine/health?check",
            None,
            &[("X-Forwarded-For", "203.0.113.5")],
        );
        assert_eq!(h.status, 200);
        assert_eq!(h.body, "{\"status\":\"ok\"}", "status only, no detail");
    }
    let after = probes(&mut b);
    assert!(
        after - before <= 1,
        "30 checks made {} probes",
        after - before
    );

    // 6. One client flooding during an outage is bounded (the hourly limit
    // counted in memory, its share of the spool), and posts counted again
    // at replay: a client that used most of its hour before the outage
    // gets its later posts stored without mail.
    let small = "a message of a reasonable length";
    for i in 0..15 {
        let r = post_from(
            &mut b,
            &form,
            &format!("y live {i} {small}"),
            Some("198.51.100.20"),
        );
        assert_eq!(r.status, 303);
    }
    let jobs_before = mail_jobs(&db);
    docker(&["stop", "-t", "2", &container]);
    let mut ok = 0;
    let last = loop {
        let r = post_from(
            &mut b,
            &form,
            &format!("y outage {ok} {small}"),
            Some("198.51.100.20"),
        );
        if r.status != 303 {
            break r;
        }
        ok += 1;
        assert!(ok <= 20, "the hourly limit holds");
    };
    assert_eq!(ok, 20, "20 an hour, counted in memory");
    assert_eq!(last.status, 429, "{}", last.body);
    // A big flood from one client stops at its share of the spool.
    let long = "x".repeat(3900);
    let mut big = 0;
    let refused = loop {
        let r = post_from(
            &mut b,
            &form,
            &format!("z{big:04}{long}"),
            Some("198.51.100.30"),
        );
        if r.status != 303 {
            break r;
        }
        big += 1;
        assert!(big <= 20);
    };
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.header("retry-after").is_some());
    assert!(
        refused.body.contains("not accepting more submissions"),
        "{}",
        refused.body
    );
    assert!(big < 17, "one client holds at most 64 KiB: {big} posts");
    // Others still get in.
    assert_eq!(
        post_from(&mut b, &form, "another client", Some("198.51.100.31")).status,
        303
    );
    let used: u64 = spool_files(&spool)
        .iter()
        .map(|f| std::fs::metadata(f).unwrap().len())
        .sum();
    assert!(used <= 1 << 20, "bounded: {used}");
    docker(&["start", &container]);
    wait_db(&db);
    wait_for("the spool replayed", || spool_files(&spool).is_empty());
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM engine_form_submissions WHERE data->>'message' LIKE 'y outage %'"
        ),
        20,
        "every post kept"
    );
    // 15 live + 5 replayed under the limit get mail (two each); the other
    // 15 replayed are stored without; plus `big` + 1 from the others.
    assert_eq!(
        mail_jobs(&db) - jobs_before,
        2 * (5 + big + 1),
        "mail within the hourly limit"
    );
    println!("one client's share: {big} posts of ~4 KB");
    drop(server);
}

/// Listen first (DECISIONS.md, "Start on demand"): the engine started while
/// its database is stopped serves published pages at once, answers pages
/// that need the database with the outage answer (published copy, or `503`
/// with `Retry-After`), and once the database is started connects, checks
/// its migrations and serves everything, with no restart.
#[test]
fn start_with_the_database_down_then_up() {
    let (Ok(db), Ok(container)) = (
        std::env::var("ENGINE_TEST_PGSLEEP_URL"),
        std::env::var("ENGINE_TEST_PGSLEEP_CONTAINER"),
    ) else {
        return;
    };
    assert!(
        container.starts_with("wrought-test-pgsleep"),
        "refusing to stop a container not made for this test: {container}"
    );
    // A database of its own on the same server.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let name = "hammerwright_start_down";
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.clone());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        engine_db::testing::exec_sql(&d, &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await
            .unwrap();
        engine_db::testing::exec_sql(&d, &format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        d.close().await;
    });
    let url = engine_db::testing::with_database(&db, name);
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
    engine(&url, &["migrate", "--site", s], None);
    engine(&url, &["content", "seed", "--site", s], None);
    engine(&url, &["build", "--site", s], None);
    let spool = dir.path().join("spool");
    let spawn = |log: &str| {
        let port = free_port();
        let logf = dir.path().join(log);
        let f = std::fs::File::create(&logf).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_engine"))
            .args([
                "serve",
                "--site",
                s,
                "--port",
                &port.to_string(),
                "--no-workers",
            ])
            .env("ENGINE_DATABASE_URL", &url)
            .env("ENGINE_SECRET_KEY", "0123456789abcdef0123456789abcdef")
            .env("ENGINE_FORM_SPOOL_DIR", &spool)
            .env("ENGINE_SMTP_URL", "smtp://127.0.0.1:9")
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

    // Once with the database up: the visitor gets the form, and the engine
    // keeps the forms' stamp key in the spool's folder for the next start.
    let (first, port) = spawn("serve-up.log");
    let mut b = Browser::new(port);
    let page = b.get("/support/");
    assert_eq!(page.status, 200, "{}", first.log());
    let form = Form {
        csrf: hidden(&page.body, "_csrf"),
        stamp: hidden(&page.body, "_t"),
    };
    drop(first);
    let key = spool.join("stamp.key");
    assert_eq!(
        std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );

    docker(&["stop", "-t", "5", &container]);
    let t0 = Instant::now();
    let (server, port) = spawn("serve-down.log");
    b.port = port;
    let home = b.get("/");
    let first = t0.elapsed();
    assert_eq!(home.status, 200, "{}", server.log());
    println!("published page {first:?} after the start, database stopped");
    assert!(first < Duration::from_secs(5), "{first:?}");
    // The published copy of a page asked for with a query.
    assert_eq!(b.get("/?from=test").status, 200);
    // A page rendered per request with no published copy: 503 at once.
    let t = Instant::now();
    let latest = b.get("/latest/");
    assert_eq!(latest.status, 503, "{}", server.log());
    assert_eq!(latest.header("retry-after"), Some("30"));
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(server.log().contains("not reachable"), "{}", server.log());

    // Form posts while the database is unreachable at the start go to the
    // spool like any other outage post (DECISIONS.md, "Spool form posts
    // during start"): validated against the site's form files, stamps
    // checked, the hourly limit counted in memory.
    while t0.elapsed() < Duration::from_millis(2100) {
        std::thread::sleep(Duration::from_millis(100)); // the time trap
    }
    for i in 0..20 {
        let r = post(&mut b, &form, &format!("start message number {i}"));
        assert_eq!(r.status, 303, "post {i}: {}\n{}", r.body, server.log());
        assert_eq!(r.location(), "/support/?sent=support");
    }
    assert_eq!(spool_files(&spool).len(), 20);
    for f in spool_files(&spool) {
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let r = post(&mut b, &form, "start message over the limit");
    assert_eq!(r.status, 429, "{}", r.body);
    // Invalid posts are refused as usual, and spool nothing.
    let bad = b.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", &form.csrf),
            ("_page", "/support/"),
            ("_t", &form.stamp),
            ("name", "Ann Example"),
            ("email", "not an address"),
            ("message", "start invalid"),
        ],
    );
    assert_eq!(bad.status, 422, "{}", bad.body);
    let forged = b.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", &form.csrf),
            ("_page", "/support/"),
            ("_t", "1.0123"),
            ("message", "start forged"),
        ],
    );
    assert_eq!(forged.status, 422, "{}", forged.body);
    // Not in the files: perhaps an admin-made form, defined in the
    // database: 503, not 404. And a post without the CSRF cookie's token
    // (a signed-in visitor's session cannot be read yet): 503.
    let other = b.post("/_engine/forms/made-in-admin/", &[("_csrf", &form.csrf)]);
    assert_eq!(other.status, 503);
    assert_eq!(other.header("retry-after"), Some("30"));
    let no_token = b.post(
        "/_engine/forms/support/",
        &[("_csrf", "x"), ("message", "m")],
    );
    assert_eq!(no_token.status, 503);
    assert_eq!(spool_files(&spool).len(), 20);

    docker(&["start", &container]);
    wait_db(&url);
    let back = Instant::now();
    while b.get("/latest/").status != 200 {
        assert!(
            back.elapsed() < Duration::from_secs(20),
            "not started after the database came up:\n{}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "per-request page {:?} after the database answered",
        back.elapsed()
    );
    assert_eq!(b.get("/support/").status, 200);
    // The start's posts are written once each, in order, with their mail
    // queued once (receipt and notice).
    let back = Instant::now();
    while !spool_files(&spool).is_empty() {
        assert!(back.elapsed() < Duration::from_secs(20), "{}", server.log());
        std::thread::sleep(Duration::from_millis(100));
    }
    for i in 0..20 {
        assert_eq!(
            count_msg(&url, &format!("start message number {i}")),
            1,
            "post {i}"
        );
    }
    assert_eq!(
        scalar(
            &url,
            "SELECT count(*) FROM (SELECT id, data->>'message' AS m, \
             row_number() OVER (ORDER BY id) AS n FROM engine_form_submissions \
             WHERE data->>'message' LIKE 'start message number %') s \
             WHERE m <> 'start message number ' || (n - 1)::text"
        ),
        0,
        "replayed in order"
    );
    assert_eq!(mail_jobs(&url), 40);
    drop(server);
}
