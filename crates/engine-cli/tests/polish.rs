//! Acceptance test for roadmap v0.5 ("polish"): media, search, the JSON
//! API and form file uploads, through the real `engine` binary and the
//! `examples/cms-site` example, on the test database.
//!
//! Needs every feature (`--all-features`) and ENGINE_TEST_DATABASE_URL;
//! skipped otherwise.

#![cfg(all(
    feature = "admin",
    feature = "media",
    feature = "search",
    feature = "api",
    feature = "forms"
))]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use engine_db::testing::TestDb;

const ADMIN: &str = "admin@example.org";
const ADMIN_PW: &str = "admin password, long enough";
const WRITER: &str = "writer@example.org";
const WRITER_PW: &str = "writer password, long enough";

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site")
}

fn sample(name: &str) -> Vec<u8> {
    std::fs::read(example().join("media-samples").join(name)).unwrap()
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn json(r: &Resp) -> serde_json::Value {
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", r.body))
}

/// The example site plus what the test needs: a 1 MB upload limit, a low
/// API rate limit, and a form with a file field on a request page.
fn site(dir: &Path) -> PathBuf {
    let site = dir.join("site");
    copy_dir(&example(), &site);
    let toml = std::fs::read_to_string(site.join("site.toml")).unwrap();
    let toml = toml.replace("max_mb = 10", "max_mb = 1");
    let toml = format!(
        "{toml}\n[api]\nrequests_per_minute = 40\n\n[[routes]]\npath = \"/apply/\"\n\
         template = \"apply.html\"\nmode = \"request\"\ntitle = \"Apply\"\n"
    );
    std::fs::write(site.join("site.toml"), toml).unwrap();
    std::fs::create_dir_all(site.join("forms")).unwrap();
    std::fs::write(
        site.join("forms/application.toml"),
        "title = \"Apply\"\n[spam]\nmin_seconds = 0\n\
         [[fields]]\nname = \"name\"\nrequired = true\n\
         [[fields]]\nname = \"portfolio\"\ntype = \"file\"\naccept = [\"image\"]\nrequired = true\n",
    )
    .unwrap();
    std::fs::write(
        site.join("templates/apply.html"),
        "{% extends \"base.html\" %}{% block main %}<h1>{{ page.title }}</h1>\
         {{ component(\"form\", name=\"application\", heading=false) }}{% endblock %}",
    )
    .unwrap();
    site
}

#[tokio::test(flavor = "multi_thread")]
async fn media_search_api_and_uploads() {
    let Some(t) = TestDb::create("v05_polish").await else {
        return;
    };
    let db = t.url.clone();
    let dir = tempfile::tempdir().unwrap();
    let site = site(dir.path());
    let t_db = t.db.clone();
    tokio::task::spawn_blocking(move || run(&db, &site, t_db))
        .await
        .unwrap();
    t.cleanup().await;
}

fn run(db: &str, site: &Path, tdb: engine_db::Db) {
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    // Media first, so the seeded articles' covers (#1, #2) exist.
    let out = engine(
        db,
        &[
            "media",
            "add",
            "--site",
            s,
            example()
                .join("media-samples/harbour-at-dusk.jpg")
                .to_str()
                .unwrap(),
            "--alt",
            "A harbour at dusk",
        ],
        None,
    );
    assert!(
        out.contains("Added #1 /media/1/harbour-at-dusk.jpg"),
        "{out}"
    );
    engine(
        db,
        &[
            "media",
            "add",
            "--site",
            s,
            example()
                .join("media-samples/forge-yard.jpg")
                .to_str()
                .unwrap(),
        ],
        None,
    );
    engine(db, &["content", "seed", "--site", s], None);
    let out = engine(db, &["build", "--site", s], None);
    assert!(out.contains("Search index:"), "{out}");
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            ADMIN,
            "--password-stdin",
        ],
        Some(&format!("{ADMIN_PW}\n")),
    );
    // A role that may write articles, but only read their summary and not
    // see their cover at all.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(engine_db::testing::exec_sql(
        &tdb,
        r#"INSERT INTO engine_roles (name, label, grants) VALUES ('writer', 'Writer',
           '{"types": {"article": {"view": true, "create": true, "edit": true,
             "fields": {"summary": "read", "cover": "none"}}}}')"#,
    ))
    .unwrap();
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            WRITER,
            "--role",
            "writer",
            "--password-stdin",
        ],
        Some(&format!("{WRITER_PW}\n")),
    );

    let out = site.join("public");
    let home = read(&out.join("index.html"));
    assert!(
        home.contains("role=\"search\""),
        "search box on the home page"
    );
    assert!(home.contains("data-weight="), "tag cloud on the home page");
    let article = read(&out.join("articles/why-wrought-iron-lasts/index.html"));
    assert!(
        article.contains("srcset=\"/media/1/harbour-at-dusk-320.jpg 320w"),
        "responsive cover: {article}"
    );
    assert!(article.contains("alt=\"A harbour at dusk\""));
    assert!(article.contains("More on these topics") || !article.contains("related-title"));

    let (server, port) = serve(db, site, &site.join("serve.log"), &[]);
    let mut anon = Browser::new(port);

    // --- Media: served safely, metadata gone, variants there ----------------
    let img = anon.get("/media/1/harbour-at-dusk.jpg");
    assert_eq!(img.status, 200, "{}", server.log());
    assert_eq!(img.header("content-type"), Some("image/jpeg"));
    assert_eq!(img.header("x-content-type-options"), Some("nosniff"));
    assert!(
        img.header("content-security-policy")
            .unwrap()
            .contains("sandbox")
    );
    assert!(
        has(&sample("harbour-at-dusk.jpg"), b"ExampleCam"),
        "the sample has EXIF"
    );
    assert!(
        !has(&img.bytes, b"ExampleCam") && !has(&img.bytes, b"GPS"),
        "EXIF stripped"
    );
    assert_eq!(anon.get("/media/1/harbour-at-dusk-640.jpg").status, 200);
    for bad in [
        "/media/1/..%2F..%2Fsite.toml",
        "/media/1/../../site.toml",
        "/media/1/x.svg",
        "/media/abc/harbour-at-dusk.jpg",
        "/media/1/Harbour-At-Dusk.jpg",
    ] {
        let r = anon.get(bad);
        assert!(matches!(r.status, 403 | 404), "{bad}: {}", r.status);
        assert!(!r.body.contains("[site]"), "{bad} leaked a file");
    }

    // --- The admin's library: uploads checked by content ---------------------
    let mut admin = Browser::new(port);
    assert_eq!(admin.login(ADMIN, ADMIN_PW).status, 303);
    let lib = admin.get("/admin/media/");
    assert_eq!(lib.status, 200, "{}", lib.body);
    let csrf = hidden(&lib.body, "_csrf");
    let photo = sample("forge-yard.jpg");
    let r = admin.multipart(
        "/admin/media/",
        &[("_csrf", &csrf), ("alt", "The forge yard")],
        &[("file", "../../etc/My Photo.php.JPG", &photo)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        r.location().starts_with("/admin/media/3/"),
        "{}",
        r.location()
    );
    let item = admin.get("/admin/media/3/");
    assert!(
        item.body.contains("my-photo-php.jpg"),
        "safe name: {}",
        item.body
    );
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'><script>alert(2)</script></svg>";
    let r = admin.multipart(
        "/admin/media/",
        &[("_csrf", &csrf)],
        &[("file", "cat.png", svg)],
    );
    assert_eq!(r.status, 415, "{}", r.body);
    assert!(r.body.contains("SVG files are not accepted"), "{}", r.body);
    let html = b"<!doctype html><script>alert(1)</script>";
    let r = admin.multipart(
        "/admin/media/",
        &[("_csrf", &csrf)],
        &[("file", "photo.jpg", html)],
    );
    assert_eq!(r.status, 415);
    let big = vec![0xFF_u8; 2 * 1024 * 1024];
    let r = admin.multipart(
        "/admin/media/",
        &[("_csrf", &csrf)],
        &[("file", "big.jpg", &big)],
    );
    assert_eq!(r.status, 413, "{}", r.body);
    let r = admin.multipart("/admin/media/", &[], &[("file", "a.jpg", &photo)]);
    assert_eq!(r.status, 403, "uploads need the CSRF token");
    assert_eq!(anon.get("/media/3/my-photo-php.jpg").status, 200);

    // --- Search on the server, without JavaScript ----------------------------
    let r = anon.get("/search/?q=iron");
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.body.contains("/articles/why-wrought-iron-lasts/"),
        "{}",
        r.body
    );
    assert!(r.body.contains("<mark>"), "{}", r.body);
    let r = anon.get("/search/?q=%3Cscript%3Ealert(1)%3C%2Fscript%3E");
    assert_eq!(r.status, 200);
    assert!(!r.body.contains("<script>alert(1)"), "the query is escaped");
    let r = anon.get("/search/?q=%22unbalanced%20NEAR(");
    assert_eq!(r.status, 200, "query syntax cannot break the page");

    // --- The JSON API ---------------------------------------------------------
    let token = |args: &[&str]| -> String {
        let mut a = vec!["api", "token", "create", "--site", s];
        a.extend_from_slice(args);
        engine(db, &a, None)
            .lines()
            .find(|l| l.starts_with("wr_"))
            .expect("a token")
            .to_string()
    };
    let rw = token(&[
        "--email", ADMIN, "--name", "rw", "--scope", "read", "--scope", "write",
    ]);
    let ro = token(&["--email", ADMIN, "--name", "ro", "--scope", "read"]);
    let wr = token(&[
        "--email",
        WRITER,
        "--name",
        "writer",
        "--scope",
        "write:article",
    ]);
    let mut api = Browser::new(port);
    let r = api.get("/api/v1/");
    assert_eq!(r.status, 401);
    assert_eq!(json(&r)["error"]["code"], "unauthorized");
    assert!(r.header("www-authenticate").is_some());
    let r = api.api("GET", "/api/v1/", &rw, None, &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let r = api.api("GET", "/api/v1/content/article/?per_page=2", &rw, None, &[]);
    assert_eq!(r.status, 200);
    let page = json(&r);
    assert_eq!(page["data"].as_array().unwrap().len(), 2);
    assert!(page["total"].as_i64().unwrap() >= 3);
    assert!(r.header("link").unwrap().contains("rel=\"next\""));
    let r = api.api(
        "GET",
        "/api/v1/content/article/why-wrought-iron-lasts",
        &rw,
        None,
        &[],
    );
    assert_eq!(r.status, 200);
    let a = json(&r);
    let v = a["version"].as_i64().unwrap();
    assert_eq!(r.header("etag"), Some(format!("\"{v}\"").as_str()));
    assert_eq!(a["fields"]["cover"], 1);
    let path = "/api/v1/content/article/why-wrought-iron-lasts";
    let body = r#"{"fields": {"title": "Why wrought iron endures"}}"#;
    let r = api.api("PATCH", path, &rw, Some(body), &[]);
    assert_eq!(r.status, 428, "a write must name its version: {}", r.body);
    assert_eq!(json(&r)["error"]["code"], "version_required");
    let ifm = format!("\"{v}\"");
    let r = api.api("PATCH", path, &rw, Some(body), &[("If-Match", &ifm)]);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(json(&r)["version"], v + 1);
    let r = api.api("PATCH", path, &rw, Some(body), &[("If-Match", &ifm)]);
    assert_eq!(r.status, 409, "stale version");
    assert_eq!(json(&r)["error"]["current_version"], v + 1);
    let r = api.api("PATCH", path, &ro, Some(body), &[("If-Match", "\"9\"")]);
    assert_eq!(r.status, 403, "a read-only token cannot write: {}", r.body);
    // Per-field access: the writer cannot see the cover, nor write the
    // summary.
    let r = api.api("GET", path, &wr, None, &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let w = json(&r);
    assert!(w["fields"].get("cover").is_none(), "hidden field: {w}");
    assert!(w["fields"].get("summary").is_some());
    let vw = format!("\"{}\"", w["version"]);
    let r = api.api(
        "PATCH",
        path,
        &wr,
        Some(r#"{"fields": {"summary": "changed"}}"#),
        &[("If-Match", &vw)],
    );
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(json(&r)["error"]["code"], "field_forbidden");
    assert_eq!(json(&r)["error"]["fields"][0], "summary");
    // Create and delete.
    let r = api.api(
        "POST",
        "/api/v1/content/article/",
        &rw,
        Some(
            r#"{"fields": {"title": "Made by the API", "body": "Hello."}, "status": "published"}"#,
        ),
        &[],
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let id = json(&r)["id"].as_i64().unwrap();
    assert_eq!(r.location(), format!("/api/v1/content/article/{id}"));
    let r = api.api(
        "POST",
        "/api/v1/content/article/",
        &rw,
        Some(r#"{"fields": {}}"#),
        &[],
    );
    assert_eq!(r.status, 422);
    assert!(json(&r)["error"]["fields"]["title"].is_string());
    let r = api.api(
        "DELETE",
        &format!("/api/v1/content/article/{id}?version=1"),
        &rw,
        None,
        &[],
    );
    assert_eq!(r.status, 204, "{}", r.body);
    // The edit was published and re-indexed through the change feed.
    wait_for("the edited title in search", || {
        anon.get("/search/?q=endures")
            .body
            .contains("why-wrought-iron-lasts")
    });
    // Revoked tokens stop working at once.
    engine(db, &["api", "token", "revoke", "--site", s, "1"], None);
    assert_eq!(api.api("GET", "/api/v1/", &rw, None, &[]).status, 401);
    // Rate limit per token (40 a minute in this site).
    let mut limited = None;
    for _ in 0..45 {
        let r = api.api("GET", "/api/v1/", &ro, None, &[]);
        if r.status == 429 {
            limited = Some(r);
            break;
        }
    }
    let r = limited.expect("a 429 within 45 requests");
    assert_eq!(json(&r)["error"]["code"], "rate_limited");
    assert!(r.header("retry-after").is_some());

    // --- Form file uploads -------------------------------------------------------
    let mut visitor = Browser::new(port);
    let page = visitor.get("/apply/");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("enctype=\"multipart/form-data\""));
    assert!(page.body.contains("type=\"file\""));
    let fields = |p: &Resp| {
        vec![
            ("_csrf".to_string(), hidden(&p.body, "_csrf")),
            ("_page".to_string(), "/apply/".to_string()),
            ("_t".to_string(), hidden(&p.body, "_t")),
            ("name".to_string(), "Ann".to_string()),
        ]
    };
    let f = fields(&page);
    let f: Vec<(&str, &str)> = f.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let r = visitor.multipart(
        "/_engine/forms/application/",
        &f,
        &[("portfolio", "me.svg", svg)],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("not an accepted kind of file"),
        "{}",
        r.body
    );
    let r = visitor.multipart(
        "/_engine/forms/application/",
        &f,
        &[("portfolio", "work.jpg", &photo)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let stored: String = rt
        .block_on(engine_db::testing::query_text(
            &tdb,
            "SELECT CAST(data AS TEXT) FROM engine_form_submissions WHERE form = 'application'",
        ))
        .unwrap();
    let data: serde_json::Value = serde_json::from_str(&stored).unwrap();
    let mid = data["portfolio"]["media_id"].as_i64().expect("a media id");
    assert_eq!(data["portfolio"]["name"], "work.jpg");
    // Private: not public, downloadable from the admin only.
    assert_eq!(anon.get(&format!("/media/{mid}/work.jpg")).status, 404);
    let r = admin.get(&format!("/admin/media/{mid}/file"));
    assert_eq!(r.status, 200);
    assert!(
        r.header("content-disposition")
            .unwrap()
            .starts_with("attachment")
    );
    let mut writer = Browser::new(port);
    writer.login(WRITER, WRITER_PW);
    assert_eq!(
        writer.get(&format!("/admin/media/{mid}/file")).status,
        403,
        "attachments are for people who read submissions"
    );
    drop(server);
}
