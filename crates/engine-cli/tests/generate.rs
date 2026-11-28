//! Roadmap v0.8, "Generators" and `engine routes`: every generator runs in
//! a fresh site, and the result builds, lists its routes and (for the Rust
//! generators) compiles, passes clippy, rustfmt and its own generated
//! tests, and serves.
//!
//! The site generators (type, route, form, mailer) run against an app
//! preset site with the stock binary; with ENGINE_TEST_DATABASE_URL the
//! site is migrated, given an item and built, and `engine routes` shows the
//! cache tags the build recorded. TagCheck runs over the build when it is
//! installed (HAMMERWRIGHT_TAGCHECK or `tagcheck` on PATH; the full check
//! is scripts/check-a11y.sh, site `generated`).
//!
//! The app crate test makes a static site an app crate depending on this
//! checkout (`--engine-path`, features `jobs,mail` to keep the build
//! small), adds handlers, a job and a mailer, and runs `cargo fmt --check`,
//! `cargo clippy -D warnings` and `cargo test` in it, offline, with this
//! workspace's Cargo.lock and toolchain and its own target folder
//! (target/generators, kept between runs). With a database it also
//! migrates and serves the crate's binary and calls both handlers.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

/// `engine <args>` without a database; (success, stdout + stderr).
fn run(args: &[&str]) -> (bool, String) {
    let o = Command::new(ENGINE)
        .args(args)
        .env_remove("ENGINE_DATABASE_URL")
        .env("PATH", path_with_toolchain())
        .output()
        .unwrap();
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn ok(args: &[&str]) -> String {
    let (ok, out) = run(args);
    assert!(ok, "engine {args:?} failed:\n{out}");
    out
}

fn fails(args: &[&str]) -> String {
    let (ok, out) = run(args);
    assert!(!ok, "engine {args:?} should have failed:\n{out}");
    out
}

/// PATH with the toolchain this test was built with first (its cargo,
/// rustc, rustfmt and clippy; the generated crate has no
/// rust-toolchain.toml).
fn path_with_toolchain() -> std::ffi::OsString {
    let cargo = Path::new(env!("CARGO"));
    let mut dirs = vec![cargo.parent().unwrap().to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

fn routes_json(bin: &str, site: &Path) -> serde_json::Value {
    let o = Command::new(bin)
        .args(["routes", "--json", "--site", site.to_str().unwrap()])
        .env_remove("ENGINE_DATABASE_URL")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn route<'a>(list: &'a serde_json::Value, kind: &str, path: &str) -> &'a serde_json::Value {
    list["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == kind && r["path"] == path)
        .unwrap_or_else(|| panic!("no {kind} route {path} in {list:#}"))
}

/// `engine routes` on a static site: pages from content and site.toml,
/// tags after a build, the engine's endpoints of this build.
#[test]
fn routes_lists_pages_with_their_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("blog");
    let s = site.to_str().unwrap();
    ok(&["new", s, "--preset", "static"]);
    let before = routes_json(ENGINE, &site);
    let home = route(&before, "page", "/");
    assert_eq!(home["mode"], "build");
    assert_eq!(home["source"], "content/_index.md");
    assert!(
        before["notes"][0]
            .as_str()
            .unwrap()
            .contains("engine build"),
        "{before:#}"
    );
    ok(&["build", "--site", s]);
    let after = routes_json(ENGINE, &site);
    assert!(after["notes"].as_array().unwrap().is_empty(), "{after:#}");
    let tags = route(&after, "page", "/")["tags"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!tags.is_empty(), "{after:#}");
    // The table has the same lines.
    let table = ok(&["routes", "--site", s]);
    assert!(table.starts_with("METHOD"), "{table}");
    assert!(table.contains("content/_index.md"), "{table}");
    assert!(table.contains("/_engine/reload"), "{table}");
    // `generate route` adds a page that builds.
    ok(&["generate", "route", "/about/team/", "--site", s]);
    fails(&["generate", "route", "/about/team/", "--site", s]);
    fails(&["generate", "route", "/admin/x/", "--site", s]);
    fails(&["generate", "route", "/notes/{slug}/", "--site", s]);
    ok(&["build", "--site", s]);
    assert!(site.join("public/about/team/index.html").is_file());
    let r = routes_json(ENGINE, &site);
    assert_eq!(
        route(&r, "page", "/about/team/")["template"],
        "about_team.html"
    );
    // Handlers need an app crate: a clear refusal, nothing written.
    let msg = fails(&["generate", "handler", "hello", "--site", s]);
    assert!(msg.contains("engine generate app"), "{msg}");
    assert!(!site.join("src").exists());
}

/// Dry runs write nothing; files that exist are refused unless --force; a
/// failed check puts everything back.
#[test]
fn generators_refuse_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("blog");
    let s = site.to_str().unwrap();
    ok(&["new", s, "--preset", "static"]);
    let toml = std::fs::read_to_string(site.join("site.toml")).unwrap();
    let out = ok(&["generate", "mailer", "reminder", "--site", s, "--dry-run"]);
    assert!(
        out.contains("would create  templates/mail/reminder.txt"),
        "{out}"
    );
    assert!(!site.join("templates/mail").exists());
    ok(&["generate", "mailer", "reminder", "--site", s]);
    std::fs::write(site.join("templates/mail/reminder.txt"), "mine").unwrap();
    let msg = fails(&["generate", "mailer", "reminder", "--site", s]);
    assert!(msg.contains("--force"), "{msg}");
    assert_eq!(read(&site.join("templates/mail/reminder.txt")), "mine");
    ok(&["generate", "mailer", "reminder", "--site", s, "--force"]);
    assert_ne!(read(&site.join("templates/mail/reminder.txt")), "mine");
    // Refused before anything is written.
    fails(&[
        "generate",
        "route",
        "/x/",
        "--template",
        "../x.html",
        "--site",
        s,
    ]);
    assert_eq!(
        std::fs::read_to_string(site.join("site.toml")).unwrap(),
        toml
    );
    fails(&["generate", "type", "Bad-Name", "--site", s]);
    fails(&["generate", "job", "mail.send", "--site", s]);
}

/// Type, route, form and mailer in an app preset site: the files load,
/// the type gets its migration, and with a database the site builds with
/// an item of the new type.
#[cfg(feature = "app")]
#[test]
fn site_generators_build() {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("app");
    let s = site.to_str().unwrap();
    ok(&["new", s, "--preset", "app"]);
    let out = ok(&[
        "generate",
        "type",
        "event",
        "--site",
        s,
        "starts:date!",
        "format:select:talk|workshop",
        "summary:markdown",
        "link:url",
        "seats:integer",
        "online:boolean",
    ]);
    assert!(out.contains("note: added a `title` field"), "{out}");
    let migration = std::fs::read_dir(site.join("migrations/postgres"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|n| n.ends_with("_types_event.sql"))
        .expect("the type's migration");
    assert!(read(&site.join("migrations/postgres").join(&migration)).contains("content_event"));
    // In sync: `types sync --check` finds nothing left to do.
    ok(&["types", "sync", "--check", "--site", s]);
    // The same type again is refused (its routes and files exist).
    fails(&["generate", "type", "event", "--site", s]);
    // A type the registry refuses after it was written (a relation to no
    // type): every file is put back.
    let toml = read(&site.join("site.toml"));
    let msg = fails(&[
        "generate",
        "type",
        "talk",
        "speaker:relation:nobody",
        "--site",
        s,
    ]);
    assert!(msg.contains("nothing was changed"), "{msg}");
    assert_eq!(read(&site.join("site.toml")), toml);
    assert!(!site.join("types/talk.toml").exists());
    assert!(!site.join("templates/talks.html").exists());
    ok(&[
        "generate",
        "route",
        "/about/team/",
        "--title",
        "Our team",
        "--site",
        s,
    ]);
    ok(&[
        "generate",
        "form",
        "rsvp",
        "email:email!",
        "guests:number",
        "--site",
        s,
    ]);
    fails(&["generate", "form", "bad", "pick:select", "--site", s]);
    ok(&["generate", "mailer", "reminder", "when:string", "--site", s]);

    let list = routes_json(ENGINE, &site);
    let item = route(&list, "pattern", "/events/{slug}/");
    assert_eq!(item["mode"], "publish");
    assert_eq!(item["tags"][0], "i:event:*");
    assert_eq!(route(&list, "page", "/rsvp/")["mode"], "request");
    assert_eq!(
        route(&list, "form", "/_engine/forms/rsvp/")["source"],
        "forms/rsvp.toml"
    );
    route(&list, "engine", "/account/login/");
    route(&list, "engine", "/admin/");
    route(&list, "engine", "/api/v1/");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some(db) = rt.block_on(engine_db::testing::TestDb::create("generate_site")) else {
        eprintln!("skipped the build: ENGINE_TEST_DATABASE_URL is not set");
        return;
    };
    let url = db.url.clone();
    engine(&url, &["migrate", "--site", s], None);
    engine(
        &url,
        &[
            "content",
            "create",
            "event",
            "--site",
            s,
            "--set",
            "title=Forge night",
            "--set",
            "starts=2026-12-04",
            "--set",
            "format=talk",
            "--set",
            "summary=Bring *iron*.",
            "--set",
            "link=https://example.org/forge",
            "--set",
            "seats=40",
            "--set",
            "online=false",
            "--publish",
        ],
        None,
    );
    engine(&url, &["build", "--site", s], None);
    let page = read(&site.join("public/events/forge-night/index.html"));
    assert!(page.contains("<h1>Forge night</h1>"), "{page}");
    assert!(page.contains("<em>iron</em>"), "{page}");
    assert!(page.contains("Seats: 40"), "{page}");
    let list_page = read(&site.join("public/events/index.html"));
    assert!(
        list_page.contains("href=\"/events/forge-night/\""),
        "{list_page}"
    );
    assert!(site.join("public/about/team/index.html").is_file());
    let after = routes_json(ENGINE, &site);
    let tags = route(&after, "page", "/events/")["tags"].to_string();
    assert!(tags.contains("c:event"), "{after:#}");
    if tagcheck_installed() {
        let out = engine(&url, &["check", "--site", s], None);
        eprintln!("{out}");
    }
    rt.block_on(db.cleanup());
}

fn tagcheck_installed() -> bool {
    std::env::var_os("HAMMERWRIGHT_TAGCHECK").is_some()
        || std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("tagcheck").is_file()))
}

/// The workspace root (this crate is crates/engine-cli).
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn cargo(site: &Path, args: &[&str]) {
    let o = Command::new(env!("CARGO"))
        .args(args)
        .current_dir(site)
        .env("PATH", path_with_toolchain())
        .env("CARGO_TARGET_DIR", workspace().join("target/generators"))
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("ENGINE_DATABASE_URL")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "cargo {args:?} in the generated crate failed:\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// `generate app`, then handlers, a job and a mailer: the crate is clean
/// for rustfmt and clippy, its generated tests pass, and it serves.
#[cfg(feature = "app")]
#[test]
fn generated_app_crate_compiles_and_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("forge-site");
    let s = site.to_str().unwrap();
    ok(&["new", s, "--preset", "static"]);
    let engine_cli = workspace().join("crates/engine-cli");
    ok(&[
        "generate",
        "app",
        "--site",
        s,
        "--engine-path",
        engine_cli.to_str().unwrap(),
        "--features",
        "jobs,mail",
    ]);
    fails(&["generate", "app", "--site", s]);
    ok(&["generate", "handler", "hello", "--site", s]);
    ok(&[
        "generate",
        "handler",
        "signup",
        "--method",
        "post",
        "--path",
        "/signups/",
        "--site",
        s,
    ]);
    fails(&[
        "generate",
        "handler",
        "again",
        "--path",
        "/signups/",
        "--site",
        s,
    ]);
    fails(&[
        "generate",
        "handler",
        "x",
        "--path",
        "/admin/x/",
        "--site",
        s,
    ]);
    ok(&[
        "generate",
        "job",
        "report.daily",
        "user_id:i64",
        "note:string",
        "--site",
        s,
    ]);
    ok(&[
        "generate",
        "mailer",
        "welcome_back",
        "name:string",
        "--site",
        s,
    ]);
    let lib = read(&site.join("src/lib.rs"));
    for want in [
        "pub mod handlers;",
        "pub mod jobs;",
        "pub mod mailers;",
        ".job(jobs::report_daily::KIND, jobs::report_daily::run)",
        "handlers::hello::handle",
    ] {
        assert!(lib.contains(want), "{want} in\n{lib}");
    }
    // The stock binary points at the crate's own for handlers.
    let stock = routes_json(ENGINE, &site);
    assert!(
        stock["notes"].to_string().contains("cargo run -- routes"),
        "{stock:#}"
    );

    std::fs::copy(workspace().join("Cargo.lock"), site.join("Cargo.lock")).unwrap();
    cargo(&site, &["fmt", "--check"]);
    cargo(
        &site,
        &[
            "clippy",
            "--offline",
            "--all-targets",
            "-q",
            "--",
            "-D",
            "warnings",
        ],
    );
    cargo(&site, &["test", "--offline", "-q"]);

    let bin = workspace().join("target/generators/debug/forge-site");
    let list = routes_json(bin.to_str().unwrap(), &site);
    assert_eq!(route(&list, "handler", "/hello/")["methods"][0], "GET");
    assert_eq!(route(&list, "handler", "/signups/")["methods"][0], "POST");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some(db) = rt.block_on(engine_db::testing::TestDb::create("generate_app")) else {
        eprintln!("skipped serving the app: ENGINE_TEST_DATABASE_URL is not set");
        return;
    };
    let url = db.url.clone();
    let env = |c: &mut Command| {
        c.env("ENGINE_DATABASE_URL", &url)
            .env("ENGINE_DATABASE_MAX_CONNECTIONS", "3")
            .env("ENGINE_SECRET_KEY", SITE_KEY);
    };
    let mut c = Command::new(&bin);
    c.args(["migrate", "--site", s]);
    env(&mut c);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut c = Command::new(&bin);
    c.args(["build", "--site", s]);
    env(&mut c);
    assert!(c.output().unwrap().status.success());

    let port = free_port();
    let log = tmp.path().join("serve.log");
    let logf = std::fs::File::create(&log).unwrap();
    let mut c = Command::new(&bin);
    c.args(["serve", "--site", s, "--port", &port.to_string()])
        .env("ENGINE_PUBLIC_URL", format!("http://localhost:{port}"))
        .stdout(logf.try_clone().unwrap())
        .stderr(logf);
    env(&mut c);
    let server = Server {
        child: c.spawn().unwrap(),
        log: log.clone(),
    };
    wait_for("the app to serve", || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let mut b = Browser::new(port);
    let r = b.get("/hello/");
    assert_eq!(r.status, 200, "{}\n{}", r.body, server.log());
    assert!(r.body.contains("<h1>Hello</h1>"), "{}", r.body);
    let r = b.request_bytes(
        "POST",
        "/signups/",
        Some((
            br#"{"email":"ada@example.org"}"#.to_vec(),
            "application/json".into(),
        )),
        &[("Origin", &format!("http://localhost:{port}"))],
    );
    assert_eq!(r.status, 202, "{}\n{}", r.body, server.log());
    assert!(r.body.contains("ada@example.org"), "{}", r.body);
    drop(server);
    rt.block_on(db.cleanup());
}
