//! Live regions end to end (DECISIONS.md, "Real-time"): the real `engine`
//! binary serving the cms example on a throwaway database, with a live
//! region on the article list. A raw HTTP client follows `c:article` on
//! `/_engine/live`; an edit through `engine content set` reaches it as an
//! event once the list is republished; a message another process sends
//! (a NOTIFY on `engine_realtime`) reaches it too; tags that are not public
//! are refused; a client that stops reading is dropped instead of growing
//! the server's memory; `Last-Event-ID` resumes; the idle exit ends open
//! streams with `bye` instead of waiting for them, and the hosting admin's
//! `live_keeps_awake` keeps the engine up while one is open.
//!
//! Needs `realtime`, `publish` (`--all-features`) and
//! ENGINE_TEST_DATABASE_URL (scripts/test-postgres.sh); skipped otherwise.

#![cfg(all(feature = "realtime", feature = "publish"))]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use engine_db::testing::TestDb;

/// The article list gets a live region following every article.
const REGION: &str = r#"{% set count %}<p>{{ listing.items | length }} articles: {% for a in listing.items %}{{ a.title }}; {% endfor %}</p>{% endset %}
{{ component("live_region", id="latest", tags=["c:article"], body=count, label="Latest articles") }}"#;

fn site(dir: &Path) -> std::path::PathBuf {
    let site = dir.join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site"),
        &site,
    );
    let t = site.join("templates/articles.html");
    let text = std::fs::read_to_string(&t).unwrap().replacen(
        "{% if listing.items %}",
        &format!("{REGION}\n{{% if listing.items %}}"),
        1,
    );
    std::fs::write(&t, text).unwrap();
    site
}

/// An open event stream: the response head is read; `text` collects what
/// arrived since.
struct Sse {
    s: TcpStream,
    text: String,
}

impl Sse {
    fn open(port: u16, query: &str, headers: &[(&str, &str)]) -> Result<Sse, String> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut req = format!(
            "GET /_engine/live?{query} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n"
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match s.read(&mut b) {
                Ok(1) => head.push(b[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        if !head.starts_with("HTTP/1.1 200") {
            let mut rest = String::new();
            let _ = s.read_to_string(&mut rest);
            return Err(format!("{head}{rest}"));
        }
        assert!(head.contains("text/event-stream"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("cache-control: no-store"),
            "{head}"
        );
        Ok(Sse {
            s,
            text: String::new(),
        })
    }

    /// Read until `needle` arrives (within `within`).
    fn wait(&mut self, needle: &str, within: Duration) -> &str {
        let start = Instant::now();
        let mut buf = [0u8; 8192];
        while !self.text.contains(needle) {
            assert!(
                start.elapsed() < within,
                "no `{needle}` within {within:?}; got:\n{}",
                self.text
            );
            self.s
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            match self.s.read(&mut buf) {
                Ok(0) => panic!("stream ended before `{needle}`:\n{}", self.text),
                Ok(n) => self.text.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => {}
            }
        }
        &self.text
    }

    /// The id of the last `event: live` seen.
    fn last_id(&self) -> String {
        let mut id = String::new();
        let mut lines = self.text.lines().peekable();
        while let Some(l) = lines.next() {
            if let Some(i) = l.strip_prefix("id: ")
                && lines.peek() == Some(&"event: live")
            {
                id = i.to_string();
            }
        }
        id
    }
}

fn exec(db: &str, sql: &str) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        engine_db::testing::exec_sql(&d, sql).await.unwrap();
        d.close().await;
    });
}

fn health(port: u16) -> serde_json::Value {
    let r = Browser::new(port).get("/_engine/health");
    serde_json::from_str(&r.body).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn live_regions_follow_the_change_feed() {
    let Some(t) = TestDb::create("realtime").await else {
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
    let site = site(dir.path());
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["content", "seed", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    let page = read(&site.join("public/articles/index.html"));
    assert!(
        page.contains(
            r#"<div id="latest" data-live="c:article" role="region" aria-label="Latest articles">"#
        ),
        "{page}"
    );
    assert!(
        page.contains("/_engine/components/live_region-"),
        "the module is loaded"
    );
    // A small per-subscriber buffer, so the slow client below is dropped
    // after a few megabytes rather than many.
    let toml = std::fs::read_to_string(site.join("site.toml")).unwrap();
    std::fs::write(
        site.join("site.toml"),
        format!("{toml}\n[limits]\nlive_buffer_kb = 16\n"),
    )
    .unwrap();
    let log = dir.path().join("serve.log");
    let (server, port) = serve(db, &site, &log, &[]);

    // Tags that are not public are refused; nonsense is a bad request.
    for (q, code) in [
        ("tags=c:engine_users", "403"),
        ("tags=i:media:1", "403"),
        ("tags=c:article,i:media:1", "403"),
        ("tags=nonsense", "400"),
        ("", "400"),
    ] {
        let e = Sse::open(port, q, &[]).err().unwrap_or_default();
        assert!(e.starts_with(&format!("HTTP/1.1 {code}")), "{q}: {e}");
    }
    let h = health(port);
    assert_eq!(h["live"]["subscribers"], 0, "{h}");
    assert!(h["live"]["limit"].as_u64().unwrap() >= 256, "{h}");

    // An edit, once published, reaches a subscriber of its type.
    let mut sse = Sse::open(port, "tags=c:article", &[]).unwrap();
    sse.wait("retry: 3000", Duration::from_secs(5));
    wait_for("the listener", || health(port)["live"]["listening"] == true);
    let started = Instant::now();
    engine(
        db,
        &[
            "content",
            "set",
            "article",
            "1",
            "--site",
            s,
            "--set",
            "title=Flattening a plane sole, again",
        ],
        None,
    );
    sse.wait(
        r#"data: {"kind":"change","tag":"c:article"}"#,
        Duration::from_secs(15),
    );
    let took = started.elapsed();
    // The event comes after the list was written: the region fetches the
    // new title.
    let page = read(&site.join("public/articles/index.html"));
    assert!(page.contains("Flattening a plane sole, again"), "{page}");
    println!("edit to event: {took:?}");
    let first = sse.last_id();
    assert!(!first.is_empty());

    // Another process's message (its NOTIFY), payload and all.
    exec(
        db,
        r#"SELECT pg_notify('engine_realtime', '{"o":"elsewhere","k":"message","t":["c:article","c:author"],"p":{"n":7}}')"#,
    );
    sse.wait(r#""payload":{"n":7}"#, Duration::from_secs(10));
    assert!(!sse.text.contains("c:author"), "only the tags it follows");

    // A reconnect with the last id it had gets what it missed since.
    let mut again = Sse::open(port, "tags=c:article", &[("Last-Event-ID", &first)]).unwrap();
    again.wait(r#""payload":{"n":7}"#, Duration::from_secs(5));
    assert!(!again.text.contains("event: reset"), "{}", again.text);
    // An id the hub cannot vouch for: fetch everything again.
    let mut old = Sse::open(port, "tags=c:article&last=abc-1", &[]).unwrap();
    old.wait("event: reset", Duration::from_secs(5));
    drop((again, old));

    // A client that stops reading is cut off once the socket buffers and
    // its 16 KiB are full; the server does not keep queueing for it.
    let mut slow = Sse::open(port, "tags=c:author", &[]).unwrap();
    slow.wait("retry: 3000", Duration::from_secs(5));
    let pad = "x".repeat(3900);
    for batch in 0..4 {
        exec(
            db,
            &format!(
                "SELECT pg_notify('engine_realtime', json_build_object('o', 'elsewhere', 'k', \
                 'message', 't', json_build_array('c:author'), 'p', '{pad}' || i)::text) \
                 FROM generate_series({}, {}) AS i",
                batch * 1000,
                batch * 1000 + 999
            ),
        );
        if health(port)["live"]["dropped"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
    }
    wait_for("the slow client to be dropped", || {
        health(port)["live"]["dropped"].as_u64().unwrap_or(0) >= 1
    });
    // The healthy subscriber still gets events meanwhile.
    exec(
        db,
        r#"SELECT pg_notify('engine_realtime', '{"o":"elsewhere","k":"message","t":["c:article"],"p":"still here"}')"#,
    );
    sse.wait("still here", Duration::from_secs(10));
    // What the slow one finally reads ends: it was disconnected.
    slow.s
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut rest = Vec::new();
    let _ = slow.s.read_to_end(&mut rest);
    assert!(
        rest.len() < 32 << 20,
        "bounded: {} bytes after the drop",
        rest.len()
    );
    drop(slow);
    wait_for("the slow client to be gone", || {
        health(port)["live"]["subscribers"] == 1
    });
    drop(sse);
    wait_for("no subscribers", || {
        health(port)["live"]["subscribers"] == 0
    });
    drop(server);

    // Idle exit: an open stream does not keep the engine up; it is told
    // `bye` (idle) and the process exits.
    let log2 = dir.path().join("idle.log");
    let (mut idle, port) = serve(db, &site, &log2, &[("ENGINE_IDLE_EXIT_SECONDS", "2")]);
    let mut sse = Sse::open(port, "tags=c:article", &[]).unwrap();
    sse.wait("event: bye\ndata: {\"idle\":true}", Duration::from_secs(20));
    let start = Instant::now();
    loop {
        if let Some(st) = idle.child.try_wait().unwrap() {
            assert!(st.success(), "{}", idle.log());
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "still running:\n{}",
            idle.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop((sse, idle));

    // The hosting admin's `live_keeps_awake`: an open stream is use.
    let host = dir.path().join("host.toml");
    std::fs::write(&host, "[defaults]\nlive_keeps_awake = true\n").unwrap();
    let log3 = dir.path().join("awake.log");
    let (mut awake, port) = serve(
        db,
        &site,
        &log3,
        &[
            ("ENGINE_IDLE_EXIT_SECONDS", "2"),
            ("ENGINE_HOST_CONFIG", host.to_str().unwrap()),
        ],
    );
    let mut sse = Sse::open(port, "tags=c:article", &[]).unwrap();
    sse.wait("retry: 3000", Duration::from_secs(5));
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        awake.child.try_wait().unwrap().is_none(),
        "an open stream keeps it up:\n{}",
        awake.log()
    );
    drop(sse);
    let start = Instant::now();
    while awake.child.try_wait().unwrap().is_none() {
        assert!(
            // The server notices the closed stream at the next heartbeat
            // at the latest (25 s).
            start.elapsed() < Duration::from_secs(45),
            "idle once the stream closed:\n{}",
            awake.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
