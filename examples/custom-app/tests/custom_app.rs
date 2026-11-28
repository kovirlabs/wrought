//! The App crate example end to end: this crate's own binary (the engine
//! plus the site's handler, queries, job, hook and command) migrates,
//! builds and serves the site on a throwaway PostgreSQL database, and is
//! driven over HTTP.
//!
//! Needs ENGINE_TEST_DATABASE_URL (scripts/test-postgres.sh sets it);
//! skipped otherwise.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use engine_db::testing::TestDb;

const BIN: &str = env!("CARGO_BIN_EXE_custom-app");
const SITE_KEY: &str = "test site key: 0123456789abcdef0123456789abcdef";

/// The site's files (not the crate's sources) in a fresh folder.
fn site_copy() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("site");
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    for d in ["templates", "types", "migrations"] {
        copy_dir(&here.join(d), &site.join(d));
    }
    std::fs::copy(here.join("site.toml"), site.join("site.toml")).unwrap();
    (tmp, site)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn run(db: &str, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "3")
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "custom-app {args:?} failed:\n{text}");
    text
}

struct Server {
    child: Child,
    log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            let _ = std::io::stderr().write_all(format!("--- server log:\n{log}\n").as_bytes());
        }
    }
}

/// `(status, body)` of one request (HTTP/1.1, the connection closed after).
fn http(port: u16, method: &str, path: &str, json: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = json.unwrap_or("");
    let ct = if json.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\n{ct}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn the_binary_is_the_engine_plus_the_site_command() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    for cmd in ["serve", "build", "migrate", "doctor", "signups"] {
        assert!(help.contains(cmd), "`{cmd}` in --help:\n{help}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_query_job_hook_and_command_on_postgres() {
    let Some(t) = TestDb::create("custom_app").await else {
        return;
    };
    let url = t.url.clone();
    // The server stays up (held here) until the hook has been seen.
    let running = tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    // The publish hook logged the home page (it runs beside the publisher,
    // so possibly just after the page was written).
    let start = Instant::now();
    loop {
        let logged = engine_db::testing::query_i64(
            &t.db,
            "SELECT count(*) FROM app_publish_log WHERE path = '/'",
        )
        .await
        .unwrap();
        if logged >= 1 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no publish hook rows for /"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(running);
    t.cleanup().await;
}

/// Returns the running server (and its folder).
fn scenario(db: &str) -> (Server, tempfile::TempDir) {
    let (tmp, site) = site_copy();
    let s = site.to_str().unwrap();

    // The embedded migration is applied with the engine's and the site
    // folder's.
    let out = run(db, &["migrate", "--site", s]);
    assert!(out.contains("app/0001_signups"), "{out}");
    assert!(run(db, &["migrate", "--site", s, "--status"]).contains("site/0001_types_note"));

    // The named query renders in a build.
    run(db, &["build", "--site", s]);
    let index = site.join("public/index.html");
    let home = std::fs::read_to_string(&index).unwrap();
    assert!(home.contains("0 signups so far"), "{home}");
    assert!(home.contains("No signups yet."), "{home}");

    let port = free_port();
    let log = tmp.path().join("serve.log");
    let logf = std::fs::File::create(&log).unwrap();
    let child = Command::new(BIN)
        .args(["serve", "--site", s, "--port", &port.to_string()])
        .env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "4")
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .env("ENGINE_PUBLIC_URL", format!("http://localhost:{port}"))
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .unwrap();
    let server = Server { child, log };
    wait_for("the server", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
            && http(port, "GET", "/_engine/health", None).0 == 200
            && std::fs::read_to_string(&server.log)
                .unwrap_or_default()
                .contains("Site handlers: 2")
    });

    // The handler stores the signup and queues the job.
    let (st, body) = http(
        port,
        "POST",
        "/signups/",
        Some(r#"{"email": "Ann@Example.org"}"#),
    );
    assert_eq!(st, 201, "{body}");
    let id: i64 = body
        .trim_start_matches("{\"id\":")
        .trim_end_matches('}')
        .parse()
        .unwrap_or_else(|_| panic!("id in {body}"));
    let (st, body) = http(
        port,
        "POST",
        "/signups/",
        Some(r#"{"email": "ann@example.org"}"#),
    );
    assert_eq!(st, 409, "{body}");
    assert_eq!(
        http(port, "POST", "/signups/", Some(r#"{"email": "nope"}"#)).0,
        422
    );

    // The job ran: the handler's page (rendered with the site's templates)
    // says so.
    wait_for("the welcome job", || {
        let (st, body) = http(port, "GET", &format!("/signups/{id}/"), None);
        st == 200 && body.contains("ann@example.org: welcomed")
    });
    assert!(
        http(port, "GET", &format!("/signups/{id}/"), None)
            .1
            .contains("<title>")
    );
    assert_eq!(http(port, "GET", "/signups/999999/", None).0, 404);

    // The trigger fed the change log, the query's tag republished the home
    // page.
    wait_for("the home page republished", || {
        let home = std::fs::read_to_string(&index).unwrap_or_default();
        home.contains("ann@example.org (welcomed)") && home.contains("1 signup so far")
    });
    assert_eq!(http(port, "GET", "/", None).0, 200);

    // The site's command.
    let out = run(db, &["signups", "--site", s, "--limit", "5"]);
    assert!(out.contains("1 signup(s)"), "{out}");
    assert!(out.contains("ann@example.org"), "{out}");

    // Engine routes are untouched.
    assert_eq!(http(port, "GET", "/account/login/", None).0, 200);
    (server, tmp)
}
