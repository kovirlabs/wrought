//! `engine dev` serves the whole app (request pages, accounts, forms, jobs,
//! live regions) with the developer tools, and `engine serve` of the same
//! site has none of them: the error page with the template's line and the
//! request's queries, the N+1 warning (terminal, overlay, request page),
//! the 404 with near routes, the explained live-region refusal, mail
//! captured instead of sent and the mail template previews.
//!
//! Needs the `app` features (`--all-features`) and ENGINE_TEST_DATABASE_URL
//! (scripts/test-postgres.sh); skipped otherwise. No SMTP server is given:
//! under `engine dev` nothing would reach one anyway.

#![cfg(all(
    feature = "forms",
    feature = "mail",
    feature = "publish",
    feature = "realtime"
))]

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use engine_db::testing::TestDb;

const PW: &str = "ann's password, long enough";

#[tokio::test(flavor = "multi_thread")]
async fn dev_serves_the_app_with_tools_and_serve_has_none() {
    let Some(t) = TestDb::create("dev_server").await else {
        return;
    };
    let url = t.url.clone();
    tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    t.cleanup().await;
}

/// `engine dev` on a free port, its output in `log`.
fn dev(db: &str, site: &Path, log: &Path) -> (Server, u16) {
    let port = free_port();
    let logf = std::fs::File::create(log).unwrap();
    let child = Command::new(ENGINE)
        .args([
            "dev",
            "--site",
            site.to_str().unwrap(),
            "--port",
            &port.to_string(),
            "--no-check",
            "--n-plus-one",
            "4",
        ])
        .env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "4")
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .env("ENGINE_PUBLIC_URL", format!("http://localhost:{port}"))
        .env_remove("ENGINE_SMTP_URL")
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .unwrap();
    let server = Server {
        child,
        log: log.to_path_buf(),
    };
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "engine dev did not start:\n{}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = Browser::new(port).get("/_engine/health");
    (server, port)
}

fn scenario(db: &str) {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/small-app"),
        &site,
    );
    // A page whose template fails on its third line, and one that runs the
    // same query six times.
    let mut toml = read(&site.join("site.toml"));
    toml.push_str(
        "\n[[routes]]\npath = \"/broken/\"\ntemplate = \"broken.html\"\nmode = \"request\"\n\
         title = \"Broken\"\n\n[[routes]]\npath = \"/many/\"\ntemplate = \"many.html\"\n\
         mode = \"request\"\ntitle = \"Many\"\n",
    );
    std::fs::write(site.join("site.toml"), toml).unwrap();
    std::fs::write(
        site.join("templates/broken.html"),
        "{% extends \"layout.html\" %}{% block main %}\n<p>Before.</p>\n{{ section_pages() }}\n{% endblock %}\n",
    )
    .unwrap();
    std::fs::write(
        site.join("templates/many.html"),
        "{% extends \"layout.html\" %}{% block main %}<ul>{% for i in range(6) %}\
         {% for u in list_items(\"update\") %}<li>{{ u.title }}</li>{% endfor %}{% endfor %}</ul>\
         {% endblock %}\n",
    )
    .unwrap();
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["content", "seed", "--site", s], None);
    let ann = "ann@dev-tools.test";
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            ann,
            "--name",
            "Ann Example",
            "--password-stdin",
        ],
        Some(&format!("{PW}\n")),
    );

    {
        let (server, port) = dev(db, &site, &dir.path().join("dev.log"));
        let mut b = Browser::new(port);
        let html = [("Accept", "text/html")];

        // The whole app: a request page with a form (accounts) is served.
        let page = b.get("/reset/");
        assert_eq!(page.status, 200, "{}", page.body);
        assert!(page.body.contains("Hammerwright dev tools"), "overlay");
        assert!(page.body.contains("/_engine/reload"), "live reload");

        // A template error: the chain, the file and line with its source,
        // the request and its queries.
        let r = b.request("GET", "/broken/", None, &html);
        assert_eq!(r.status, 500);
        for want in [
            "Error chain",
            "Template <code>broken.html</code>, line 3",
            "section_pages()",
            "<tr class=\"hl\"><th scope=\"row\">3</th>",
            "<h2>Request</h2>",
            "<h2>Queries:",
            "engine dev</code> only",
        ] {
            assert!(r.body.contains(want), "missing {want:?} in:\n{}", r.body);
        }
        // A client that asked for JSON gets the chain as text.
        let r = b.request("GET", "/broken/", None, &[("Accept", "application/json")]);
        assert_eq!(r.status, 500);
        assert!(r.body.contains("section_pages"), "{}", r.body);

        // N+1: six runs of one statement; the terminal, the overlay and
        // the request's page say so, with the call site.
        let r = b.request("GET", "/many/", None, &html);
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(r.body.contains("likely N+1"), "overlay: {}", r.body);
        let id = r
            .body
            .split("/_engine/dev/requests/")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("link to the request's page")
            .to_string();
        let p = b.get(&format!("/_engine/dev/requests/{id}"));
        assert_eq!(p.status, 200);
        assert!(p.body.contains("Likely N+1"), "{}", p.body);
        assert!(
            p.body.contains("ran <strong>6 times</strong>"),
            "{}",
            p.body
        );
        assert!(p.body.contains(".rs:"), "call site: {}", p.body);
        wait_for("the N+1 warning in the terminal", || {
            server.log().contains("likely N+1")
        });
        let index = b.get("/_engine/dev/");
        assert!(index.body.contains("/many/"), "{}", index.body);

        // A 404 lists the nearest routes.
        let r = b.request("GET", "/updtes/", None, &html);
        assert_eq!(r.status, 404);
        assert!(r.body.contains("Did you mean"), "{}", r.body);
        assert!(r.body.contains("href=\"/updates/\""), "{}", r.body);

        // A refused live tag is explained.
        let r = b.get("/_engine/live?tags=c:secret,c:update");
        assert_eq!(r.status, 403);
        assert!(
            r.body.contains("c:secret: refused: not public"),
            "{}",
            r.body
        );
        assert!(r.body.contains("c:update: public"), "{}", r.body);
        assert!(r.body.contains("The rule"), "{}", r.body);

        // Mail is captured, not sent: a password reset.
        let page = b.get("/reset/");
        let r = b.post(
            "/account/reset/",
            &[("_csrf", &hidden(&page.body, "_csrf")), ("email", ann)],
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let mut list = String::new();
        wait_for("the reset mail in /_engine/dev/mail/", || {
            list = b.get("/_engine/dev/mail/").body;
            list.contains("Reset your password")
        });
        assert!(list.contains(ann), "{list}");
        let mid = list
            .split("href=\"/_engine/dev/mail/")
            .filter_map(|s| s.split('"').next())
            .find(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
            .expect("a captured message")
            .to_string();
        let m = b.get(&format!("/_engine/dev/mail/{mid}"));
        assert_eq!(m.status, 200);
        assert!(
            m.body.contains("/account/reset/confirm/?token="),
            "{}",
            m.body
        );
        assert!(
            m.body.contains("<th scope=\"row\">Subject</th>"),
            "{}",
            m.body
        );
        let h = b.get(&format!("/_engine/dev/mail/{mid}/html"));
        assert_eq!(h.status, 200);
        assert_eq!(h.header("content-security-policy"), Some("sandbox"));
        assert!(server.log().contains("Mail captured (not sent)"));
        // Every template previews with its sample data, nothing sent.
        let p = b.get("/_engine/dev/mail/templates/password_reset");
        assert_eq!(p.status, 200);
        assert!(p.body.contains("Reset your password"), "{}", p.body);
        assert!(
            list.contains("/_engine/dev/mail/templates/form_receipt"),
            "{list}"
        );
    }

    // `engine serve`: the same site answers exactly as before, without any
    // of it.
    let (_server, port) = serve(db, &site, &dir.path().join("serve.log"), &[]);
    let mut b = Browser::new(port);
    let html = [("Accept", "text/html")];
    let r = b.request("GET", "/broken/", None, &html);
    assert_eq!(r.status, 500);
    assert_eq!(r.body, "This page could not be rendered.\n");
    let r = b.request("GET", "/many/", None, &html);
    assert_eq!(r.status, 200);
    assert!(!r.body.contains("Hammerwright dev tools"));
    assert!(!r.body.contains("/_engine/reload"));
    let r = b.request("GET", "/updtes/", None, &html);
    assert_eq!(r.status, 404);
    assert!(!r.body.contains("Did you mean"));
    let r = b.get("/_engine/live?tags=c:secret");
    assert_eq!(r.status, 403);
    assert_eq!(
        r.body,
        "Not a public tag: follow it through one of the site's own handlers.\n"
    );
    for p in [
        "/_engine/dev/",
        "/_engine/dev/mail/",
        "/_engine/dev/requests/1",
    ] {
        let r = b.request("GET", p, None, &html);
        assert_eq!(r.status, 404, "{p}");
        assert!(!r.body.contains("engine dev"), "{p}: {}", r.body);
    }
}
