//! Acceptance test for roadmap v0.3: two editors saving the same item get
//! a conflict, not an overwrite. Runs the real `engine` binary
//! (`engine migrate`, `content seed`, `build`, `user create`, `serve`)
//! against a throwaway database and drives the admin over HTTP with
//! separate cookie jars, like two browsers. Also covers sign-in, cookies,
//! CSRF, per-field rights, revisions, rate limits, password reset, TOTP,
//! `request` mode and paginated lists.
//!
//! Needs the `cms` features (`--all-features`) and ENGINE_TEST_DATABASE_URL;
//! skipped otherwise.

#![cfg(all(feature = "admin", feature = "roles", feature = "publish"))]

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use engine_db::testing::TestDb;

const ADMIN_PW: &str = "admin password, long enough";
const ED_PW: &str = "editor password, long enough";

#[tokio::test(flavor = "multi_thread")]
async fn two_editors_get_a_conflict_not_an_overwrite() {
    let Some(t) = TestDb::create("two_editors_get_a_conflict").await else {
        return;
    };
    let url = t.url.clone();
    // The whole scenario blocks (processes, sockets).
    tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    t.cleanup().await;
}

fn scenario(db: &str) {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site"),
        &site,
    );
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["content", "seed", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    let out = engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            "admin@example.org",
            "--name",
            "Admin",
            "--password-stdin",
        ],
        Some(&format!("{ADMIN_PW}\n")),
    );
    assert!(
        out.contains("roles: admin"),
        "first account is an administrator: {out}"
    );
    for who in ["ed1", "ed2"] {
        engine(
            db,
            &[
                "user",
                "create",
                "--site",
                s,
                "--email",
                &format!("{who}@example.org"),
                "--role",
                "editor",
                "--password-stdin",
            ],
            Some(&format!("{ED_PW}\n")),
        );
    }
    // No password given: an unusable one and a link to choose one.
    let out = engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            "later@example.org",
        ],
        None,
    );
    assert!(out.contains("/account/reset/confirm/?token="), "{out}");

    let port = free_port();
    let log = dir.path().join("serve.log");
    let logf = std::fs::File::create(&log).unwrap();
    let child = Command::new(ENGINE)
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
    wait_for("engine serve", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let public = site.join("public");

    // --- Signed out: no admin, no account page --------------------------
    let mut anon = Browser::new(port);
    let r = anon.get("/admin/");
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/account/login/?next=/admin/");
    let r = anon.get("/admin/content/article/");
    assert_eq!(r.status, 303, "every admin page needs a session");
    let r = anon.post("/admin/content/article/new/", &[("title", "x")]);
    assert!(r.status == 303 || r.status == 403, "{}", r.status);
    let r = anon.get("/account/");
    assert_eq!(r.status, 303, "requires = auth");
    assert!(r.location().starts_with("/account/login/?next=/account/"));
    assert!(
        !public.join("account/index.html").exists(),
        "request routes are never written"
    );
    // Published pages are files; anonymous visitors get them as files.
    let r = anon.get("/articles/");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("cache-control"), Some("no-cache"));

    // --- Sign-in, cookie flags --------------------------------------------
    let mut admin = Browser::new(port);
    let r = admin.login("admin@example.org", ADMIN_PW);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/admin/");
    let session_cookie = admin
        .set_cookies
        .iter()
        .find(|c| c.starts_with("__Host-wrought_session="))
        .expect("session cookie")
        .clone();
    for flag in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(
            session_cookie.contains(flag),
            "{flag} missing: {session_cookie}"
        );
    }
    assert!(!session_cookie.contains("Domain="), "{session_cookie}");
    let r = admin.get("/admin/");
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Signed in as Admin"), "{}", r.body);
    for h in ["content-security-policy", "x-frame-options"] {
        assert!(r.header(h).is_some(), "{h}");
    }
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert!(
        r.body.contains("/_engine/ui/htmx-2.0.11.min.js"),
        "htmx is served locally"
    );
    let htmx = admin.get("/_engine/ui/htmx-2.0.11.min.js");
    assert_eq!(htmx.status, 200);
    assert!(htmx.body.contains("htmx"));

    // Signed in, a published page is rendered for the request (personal).
    let r = admin.get("/articles/");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("cache-control"), Some("private, no-store"));
    let r = admin.get("/account/");
    assert_eq!(r.status, 200);
    assert!(r.body.contains("admin@example.org"), "{}", r.body);

    // --- CSRF ---------------------------------------------------------------
    let r = admin.post(
        "/admin/content/article/new/",
        &[("title", "No token"), ("body", "x")],
    );
    assert_eq!(r.status, 403, "a POST without a CSRF token is refused");
    let page = admin.get("/admin/content/article/new/");
    let csrf = hidden(&page.body, "_csrf");
    let r = admin.post(
        "/admin/content/article/new/",
        &[
            ("_csrf", "forged-token-forged-token-forged-token-xxx"),
            ("title", "Bad token"),
            ("body", "x"),
        ],
    );
    assert_eq!(r.status, 403);
    let r = admin.post_with(
        "/admin/content/article/new/",
        &[("_csrf", &csrf), ("title", "Cross site"), ("body", "x")],
        &[("Origin", "https://evil.example")],
    );
    assert_eq!(r.status, 403, "cross-origin POST");

    // --- Create through the admin; the page is published -----------------
    let r = admin.post(
        "/admin/content/article/new/",
        &[
            ("_csrf", &csrf),
            ("title", "Made in the admin"),
            ("summary", "Written in a browser."),
            ("body", "First *draft* of the text."),
            ("author", ""),
            ("topics", "Admin, Testing"),
            ("_status", "published"),
            ("_action", "save"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let edit = r.location().split('?').next().unwrap().to_string();
    assert!(edit.starts_with("/admin/content/article/"), "{edit}");
    let page_file = public.join("articles/made-in-the-admin/index.html");
    wait_for("the new article's page", || {
        read(&page_file).contains("Made in the admin")
    });
    assert!(read(&page_file).contains("<em>draft</em>"));

    // Edit it: the published page is rewritten.
    let form = admin.get(&edit);
    assert_eq!(form.status, 200);
    let v1 = hidden(&form.body, "_version");
    let r = admin.post(
        &edit,
        &[
            ("_csrf", &csrf),
            ("_version", &v1),
            ("title", "Made in the admin, edited"),
            ("body", "Second text."),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    wait_for("the rewritten page", || {
        read(&page_file).contains("Made in the admin, edited")
    });

    // --- The acceptance test: two editors, one item -----------------------
    let mut ed1 = Browser::new(port);
    let mut ed2 = Browser::new(port);
    assert_eq!(ed1.login("ed1@example.org", ED_PW).status, 303);
    assert_eq!(ed2.login("ed2@example.org", ED_PW).status, 303);
    let f1 = ed1.get(&edit);
    let f2 = ed2.get(&edit);
    let (c1, c2) = (hidden(&f1.body, "_csrf"), hidden(&f2.body, "_csrf"));
    let (v_ed1, v_ed2) = (hidden(&f1.body, "_version"), hidden(&f2.body, "_version"));
    assert_eq!(v_ed1, v_ed2, "both start from the same version");

    let r = ed1.post(
        &edit,
        &[
            ("_csrf", &c1),
            ("_version", &v_ed1),
            ("title", "Title by editor one"),
            ("body", "Body by editor one."),
        ],
    );
    assert_eq!(r.status, 303, "first save wins: {}", r.body);

    let r = ed2.post(
        &edit,
        &[
            ("_csrf", &c2),
            ("_version", &v_ed2),
            ("title", "Title by editor two"),
            ("body", "Body by editor two."),
        ],
    );
    assert_eq!(r.status, 409, "second save is a conflict, not an overwrite");
    let html = &r.body;
    assert!(html.contains("Your changes were not saved"), "{html}");
    assert!(html.contains("ed1@example.org"), "says who saved: {html}");
    assert!(
        html.contains("Title by editor one"),
        "shows the current version: {html}"
    );
    assert!(
        html.contains("value=\"Title by editor two\""),
        "keeps editor two's changes in the form: {html}"
    );
    assert!(html.contains("Body by editor two."), "{html}");
    assert!(html.contains("Save my version"), "{html}");
    let v_now = hidden(html, "_version");
    assert_ne!(v_now, v_ed2, "the form now carries the current version");
    // Nothing was overwritten.
    wait_for("editor one's title on the site", || {
        read(&page_file).contains("Title by editor one")
    });
    let current = ed2.get(&edit);
    assert!(current.body.contains("value=\"Title by editor one\""));

    // Editor two decides to replace it, knowingly.
    let r = ed2.post(
        &edit,
        &[
            ("_csrf", &c2),
            ("_version", &v_now),
            ("title", "Title by editor two"),
            ("body", "Body by editor two."),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    wait_for("editor two's title on the site", || {
        read(&page_file).contains("Title by editor two")
    });
    // Replaying the old form again is still a conflict.
    let r = ed1.post(
        &edit,
        &[("_csrf", &c1), ("_version", &v_ed1), ("title", "Stale")],
    );
    assert_eq!(r.status, 409);

    // A delete from a stale page is refused too.
    let r = ed1.post(
        &format!("{edit}delete/"),
        &[("_csrf", &c1), ("_version", &v_ed1)],
    );
    assert_eq!(r.status, 409);

    // --- Revisions: view and restore --------------------------------------
    let hist = admin.get(&format!("{edit}revisions/"));
    assert_eq!(hist.status, 200);
    for who in ["admin@example.org", "ed1@example.org", "ed2@example.org"] {
        assert!(hist.body.contains(who), "{who} in history: {}", hist.body);
    }
    let rev = admin.get(&format!("{edit}revisions/3/"));
    assert_eq!(rev.status, 200);
    assert!(rev.body.contains("Title by editor one"), "{}", rev.body);
    let cur_v = hidden(&rev.body, "_version");
    let r = admin.post(
        &format!("{edit}revisions/3/restore/"),
        &[("_csrf", &csrf), ("_version", &cur_v)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    wait_for("the restored title", || {
        read(&page_file).contains("Title by editor one")
    });

    // --- Block trees as JSON (for the visual editor) ------------------------
    let api = edit.replacen("/admin/content/", "/admin/api/items/", 1);
    let r = admin.get(&api);
    assert_eq!(r.status, 200, "{}", r.body);
    let doc: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    let v = doc["version"].as_i64().unwrap();
    assert!(
        doc["block_fields"].to_string().contains("sections"),
        "{doc}"
    );
    let blocks = r#"[{"type": "quote", "text": "Saved from **JSON**", "cite": "Tester"}]"#;
    let put = |b: &mut Browser, version: i64, csrf: Option<&str>| {
        let mut h = vec![("Content-Type", "application/json")];
        if let Some(c) = csrf {
            h.push(("X-CSRF-Token", c));
        }
        b.request(
            "PUT",
            &format!("{api}blocks/sections/"),
            Some(format!(r#"{{"version": {version}, "blocks": {blocks}}}"#)),
            &h,
        )
    };
    assert_eq!(
        put(&mut admin, v, None).status,
        403,
        "no CSRF token, no save"
    );
    assert_eq!(anon.get(&api).status, 401);
    let r = put(&mut admin, v - 1, Some(&csrf));
    assert_eq!(r.status, 409, "a stale version is a conflict: {}", r.body);
    assert!(
        r.body.contains(&format!("\"current_version\":{v}")),
        "{}",
        r.body
    );
    let r = put(&mut admin, v, Some(&csrf));
    assert_eq!(r.status, 200, "{}", r.body);
    wait_for("the block saved as JSON on the site", || {
        read(&page_file).contains("Saved from <strong>JSON</strong>")
    });

    // --- Roles and per-field access ----------------------------------------
    let r = admin.post(
        "/admin/roles/new/",
        &[
            ("_csrf", &csrf),
            ("name", "contributor"),
            ("label", "Contributor"),
            ("own.article", "true"),
            ("t.article.view", "true"),
            ("t.article.edit", "true"),
            ("t.article.create", "true"),
            ("f.article.author", "read"),
            ("f.article.sections", "none"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let r = admin.post(
        "/admin/users/new/",
        &[
            ("_csrf", &csrf),
            ("email", "cont@example.org"),
            ("name", "Cont"),
            ("password", ED_PW),
            ("roles", "contributor"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let mut cont = Browser::new(port);
    assert_eq!(cont.login("cont@example.org", ED_PW).status, 303);
    let f = cont.get(&edit);
    assert_eq!(f.status, 200);
    let cc = hidden(&f.body, "_csrf");
    let cv = hidden(&f.body, "_version");
    assert!(
        !f.body.contains("name=\"author\""),
        "author is read-only: no input name"
    );
    assert!(f.body.contains("id=\"f-author\""), "but it is shown");
    assert!(
        !f.body.contains("f-sections"),
        "sections are hidden entirely"
    );
    assert!(!f.body.contains("name=\"_status\""), "no publishing");
    for crafted in [
        vec![("title", "Sneaky"), ("author", "1")],
        vec![
            ("title", "Sneaky"),
            ("sections.0.type", "quote"),
            ("sections.0.text", "x"),
        ],
        vec![("title", "Sneaky"), ("_status", "draft")],
    ] {
        let mut form = vec![("_csrf", cc.as_str()), ("_version", cv.as_str())];
        form.extend(crafted.iter().copied());
        let r = cont.post(&edit, &form);
        assert_eq!(
            r.status, 403,
            "crafted POST {crafted:?} is refused: {}",
            r.body
        );
    }
    // Nothing changed.
    let check = admin.get(&edit);
    assert!(!check.body.contains("Sneaky"));
    assert_eq!(hidden(&check.body, "_version"), cv);
    // Fields it may write work.
    let r = cont.post(
        &edit,
        &[
            ("_csrf", &cc),
            ("_version", &cv),
            ("title", "Contributor title"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(cont.get("/admin/users/").status, 403);
    assert_eq!(cont.get("/admin/content/author/").status, 403);
    assert_eq!(cont.get("/admin/content/author/1/").status, 403);
    // Hidden fields stay hidden in the history too.
    let rev = cont.get(&format!("{edit}revisions/1/"));
    assert_eq!(rev.status, 200);
    assert!(!rev.body.contains("Sections"), "{}", rev.body);

    // --- Pagination of database lists ---------------------------------------
    for i in 0..3 {
        let r = admin.post(
            "/admin/content/article/new/",
            &[
                ("_csrf", &csrf),
                ("title", &format!("Filler {i}")),
                ("body", "x"),
                ("_status", "published"),
            ],
        );
        assert_eq!(r.status, 303);
    }
    wait_for("a second list page", || {
        public.join("articles/page/2/index.html").is_file()
    });
    let r = anon.get("/articles/page/2/");
    assert_eq!(r.status, 200);
    assert!(r.body.contains("aria-label=\"Pagination\""), "{}", r.body);

    // --- Login rate limit -----------------------------------------------------
    let mut attacker = Browser::new(port);
    for _ in 0..5 {
        assert_eq!(
            attacker.login("ed1@example.org", "wrong password!!").status,
            401
        );
    }
    let r = attacker.login("ed1@example.org", ED_PW);
    assert_eq!(
        r.status, 429,
        "even the right password waits after 5 failures"
    );
    assert!(r.body.contains("Too many failed attempts"));

    // --- Password reset -------------------------------------------------------
    let mut forgetful = Browser::new(port);
    let page = forgetful.get("/account/reset/");
    let c = hidden(&page.body, "_csrf");
    let r = forgetful.post(
        "/account/reset/",
        &[("_csrf", &c), ("email", "ed2@example.org")],
    );
    assert_eq!(r.status, 200);
    let same = forgetful.post(
        "/account/reset/",
        &[("_csrf", &c), ("email", "nobody@example.org")],
    );
    assert_eq!(same.body, r.body, "no account enumeration");
    let mut link = String::new();
    wait_for("the reset link in the log", || {
        link = server
            .log()
            .lines()
            .filter(|l| {
                l.contains("ed2@example.org") && l.contains("/account/reset/confirm/?token=")
            })
            .filter_map(|l| l.split_whitespace().last().map(str::to_string))
            .next_back()
            .unwrap_or_default();
        !link.is_empty()
    });
    let path = link
        .trim_start_matches(&format!("http://localhost:{port}"))
        .to_string();
    let page = forgetful.get(&path);
    assert_eq!(page.status, 200, "{}", page.body);
    let c = hidden(&page.body, "_csrf");
    let token = hidden(&page.body, "token");
    let r = forgetful.post(
        "/account/reset/confirm/",
        &[
            ("_csrf", &c),
            ("token", &token),
            ("password", "short"),
            ("password_again", "short"),
        ],
    );
    assert_eq!(r.status, 422, "policy");
    let new_pw = "a brand new passphrase";
    let r = forgetful.post(
        "/account/reset/confirm/",
        &[
            ("_csrf", &c),
            ("token", &token),
            ("password", new_pw),
            ("password_again", new_pw),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(forgetful.get(&path).status, 400, "the link works once");
    assert_eq!(
        ed2.get("/admin/").status,
        303,
        "a reset signs the account out everywhere"
    );
    assert_eq!(
        Browser::new(port).login("ed2@example.org", ED_PW).status,
        401
    );
    assert_eq!(
        Browser::new(port).login("ed2@example.org", new_pw).status,
        303
    );

    // --- Two-factor sign-in ------------------------------------------------------
    let prof = admin.get("/admin/profile/");
    let secret = hidden(&prof.body, "secret");
    let code = engine_auth::totp::code_at(&secret, engine_auth::totp::current_step()).unwrap();
    let r = admin.post(
        "/admin/profile/totp/",
        &[("_csrf", &csrf), ("secret", &secret), ("code", &code)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let mut second = Browser::new(port);
    let r = second.login("admin@example.org", ADMIN_PW);
    assert_eq!(r.status, 303);
    assert!(
        r.location().starts_with("/account/totp/"),
        "{}",
        r.location()
    );
    assert_eq!(
        second.get("/admin/").status,
        303,
        "half signed in is not signed in"
    );
    let page = second.get(r.location());
    let tc = hidden(&page.body, "_csrf");
    let r = second.post(
        "/account/totp/",
        &[("_csrf", &tc), ("code", "000000"), ("next", "/admin/")],
    );
    assert_eq!(r.status, 401);
    // The code used to switch it on cannot be replayed; the next one works.
    let next = engine_auth::totp::code_at(&secret, engine_auth::totp::current_step() + 1).unwrap();
    let r = second.post(
        "/account/totp/",
        &[("_csrf", &tc), ("code", &next), ("next", "/admin/")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(second.get("/admin/").status, 200);

    // --- Sign out ------------------------------------------------------------------
    let r = admin.post("/account/logout/", &[("_csrf", &csrf)]);
    assert_eq!(r.status, 303);
    assert!(admin.set_cookies.last().unwrap().contains("Max-Age=0"));
    assert_eq!(admin.get("/admin/").status, 303);

    drop(server);
}
