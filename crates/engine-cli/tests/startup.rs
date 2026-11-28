//! Start on demand (DECISIONS.md, "Start on demand"): the real `engine
//! serve` started by socket activation (`systemd-socket-activate`, which
//! passes the listening socket as systemd does) and started normally; the
//! hosting admin's idle exit, on and off; SIGTERM while a request is in
//! flight. The start with the database down is in `outage.rs` (it needs a
//! database server the test may stop).
//!
//! Needs every feature (`--all-features`) and ENGINE_TEST_DATABASE_URL
//! (PostgreSQL); skipped otherwise.

#![cfg(all(feature = "forms", feature = "mail", feature = "workflow"))]

mod common;

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use engine_db::testing::TestDb;

/// A built copy of examples/small-app on a database of its own.
fn site(db: &str, dir: &Path) -> std::path::PathBuf {
    let site = dir.join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/small-app"),
        &site,
    );
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["content", "seed", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    site
}

fn command(db: &str, site: &Path, env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_engine"));
    c.env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "3")
        .env("ENGINE_SECRET_KEY", "0123456789abcdef0123456789abcdef")
        .env("ENGINE_SMTP_URL", "smtp://127.0.0.1:9")
        .env("ENGINE_FORM_SPOOL_DIR", site.join("../spool"))
        // Never the host's own file.
        .env("ENGINE_HOST_CONFIG", site.join("../no-host.toml"))
        .env_remove("ENGINE_IDLE_EXIT_SECONDS");
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

fn spawn(mut c: Command, log: &Path) -> Child {
    let f = std::fs::File::create(log).unwrap();
    c.stdout(Stdio::from(f.try_clone().unwrap()))
        .stderr(Stdio::from(f))
        .spawn()
        .unwrap()
}

fn sigterm(c: &Child) {
    let ok = Command::new("kill")
        .args(["-TERM", &c.id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

/// Waits up to `limit` for the process to exit; its status code.
fn wait_exit(c: &mut Child, limit: Duration) -> Option<i32> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if let Some(s) = c.try_wait().unwrap() {
            return Some(s.code().unwrap_or(-1));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn log_of(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn start_on_demand() {
    let Some(t) = TestDb::create("start_on_demand").await else {
        return;
    };
    let url = t.url.clone();
    tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    t.cleanup().await;
}

fn scenario(db: &str) {
    let dir = tempfile::tempdir().unwrap();
    let site = site(db, dir.path());
    let s = site.to_str().unwrap().to_string();

    // 1. Socket activation: the socket exists before the engine does; a
    // connection made at once is queued by the kernel and answered.
    if Command::new("systemd-socket-activate")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        let port = free_port();
        let c = command(db, &site, &[]);
        let engine_bin = c.get_program().to_owned();
        let envs: Vec<_> = c
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
            .collect();
        let mut a = Command::new("systemd-socket-activate");
        a.args(["-l", &format!("127.0.0.1:{port}")]);
        // It passes on only the variables named with `--setenv`.
        let mut set = Vec::new();
        for (k, v) in envs {
            if let Some(v) = v {
                set.push(format!(
                    "--setenv={}={}",
                    k.to_str().unwrap(),
                    v.to_str().unwrap()
                ));
            }
        }
        a.args(&set)
            .arg(&engine_bin)
            .args(["serve", "--site", &s, "--no-workers"]);
        let log = dir.path().join("activated.log");
        let mut child = Kill(spawn(a, &log));
        // systemd-socket-activate binds before it executes the engine.
        wait_for("the activation socket", || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        let mut b = Browser::new(port);
        assert_eq!(b.get("/").status, 200, "{}", log_of(&log));
        assert_eq!(b.get("/support/").status, 200, "{}", log_of(&log));
        assert!(
            log_of(&log).contains("socket passed by systemd"),
            "{}",
            log_of(&log)
        );
        sigterm(&child.0);
        assert_eq!(wait_exit(&mut child.0, Duration::from_secs(15)), Some(0));
    } else {
        println!("systemd-socket-activate not found: socket activation not tested");
    }

    // 2. A normal start (binds its own port), idle exit off: still running
    // after a quiet period longer than any test idle setting.
    let port = free_port();
    let mut c = command(db, &site, &[]);
    c.args([
        "serve",
        "--site",
        &s,
        "--no-workers",
        "--port",
        &port.to_string(),
    ]);
    let log = dir.path().join("normal.log");
    let mut child = Kill(spawn(c, &log));
    wait_for("engine serve", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let mut b = Browser::new(port);
    assert_eq!(b.get("/").status, 200);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "exited without an idle exit set:\n{}",
        log_of(&log)
    );

    // 3. SIGTERM while a request is in flight: the request is answered,
    // then the engine exits 0.
    let page = b.get("/support/");
    let body = format!(
        "_csrf={}&_page=%2Fsupport%2F&_t={}&_step=0&name=Ann&email=ann%40example.test\
         &topic=Bug&priority=High&message=in+flight&website=",
        url_encode(&hidden(&page.body, "_csrf")),
        url_encode(&hidden(&page.body, "_t"))
    );
    let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let cookies: Vec<String> = b.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
    write!(
        conn,
        "POST /_engine/forms/support/ HTTP/1.1\r\nHost: localhost:{port}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\
         Cookie: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        cookies.join("; ")
    )
    .unwrap();
    conn.write_all(&body.as_bytes()[..10]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    sigterm(&child.0);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "exited with a request in flight:\n{}",
        log_of(&log)
    );
    conn.write_all(&body.as_bytes()[10..]).unwrap();
    let mut resp = String::new();
    conn.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 "), "no answer: {resp:?}");
    assert!(!resp.starts_with("HTTP/1.1 5"), "{resp}");
    assert_eq!(wait_exit(&mut child.0, Duration::from_secs(15)), Some(0));

    // 4. Idle exit on (the hosting admin's environment): exits 0 after the
    // quiet period, not before.
    let port = free_port();
    let mut c = command(db, &site, &[("ENGINE_IDLE_EXIT_SECONDS", "2")]);
    c.args([
        "serve",
        "--site",
        &s,
        "--no-workers",
        "--port",
        &port.to_string(),
    ]);
    let log = dir.path().join("idle.log");
    let started = Instant::now();
    let mut child = Kill(spawn(c, &log));
    wait_for("engine serve", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let mut b = Browser::new(port);
    let mut last = Instant::now();
    for _ in 0..6 {
        assert_eq!(b.get("/").status, 200);
        last = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "exited while requests came in"
    );
    assert_eq!(wait_exit(&mut child.0, Duration::from_secs(10)), Some(0));
    assert!(
        last.elapsed() >= Duration::from_secs(2),
        "{:?}",
        last.elapsed()
    );
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(log_of(&log).contains("exiting"), "{}", log_of(&log));
}
