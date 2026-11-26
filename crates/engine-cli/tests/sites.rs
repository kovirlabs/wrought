//! Integration tests: build the repository's own sites, and scaffold a new
//! site with the real `engine` binary and build it.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine_core::builder::{BuildOptions, build_site};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn builds_the_docs_site() {
    let out = tempfile::tempdir().unwrap();
    let output = out.path().join("public");
    let report = build_site(
        &repo().join("docs"),
        &BuildOptions {
            output: Some(output.clone()),
            drafts: false,
        },
    )
    .unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    for page in [
        "index.html",
        "docs/index.html",
        "docs/getting-started/index.html",
        "docs/site-config/index.html",
        "docs/content/index.html",
        "docs/render-dial/index.html",
        "docs/templates/index.html",
        "docs/styling/index.html",
        "docs/cli/index.html",
        "docs/deployment/index.html",
        "docs/content-types/index.html",
        "docs/publishing/index.html",
        "docs/admin/index.html",
        "components/index.html",
        "404.html",
        "sitemap.xml",
        "_engine/tagbrush-2.0.1.min.css",
        "_engine/tagaction-0.2.0.min.js",
        "_engine/theme.css",
        "_engine/licenses/tagbrush-LICENSE.txt",
    ] {
        assert!(output.join(page).is_file(), "missing {page}");
    }

    let render_dial = read(&output.join("docs/render-dial/index.html"));
    assert!(render_dial.contains("<h1>The render dial</h1>"));
    assert!(render_dial.contains(r#"<a href="/docs/render-dial/" aria-current="page">"#));
    assert!(
        render_dial.contains(r##"<a href="#cache-tags">"##),
        "toc missing"
    );
    assert!(render_dial.contains(r#"rel="next""#));
    // Built-in markup is classless; only Markdown code blocks carry
    // `language-*` classes (which TagCheck treats as generated).
    assert_eq!(
        render_dial.matches("class=\"").count(),
        render_dial.matches("class=\"language-").count(),
        "built-in markup is classless"
    );

    let theme = read(&output.join("_engine/theme.css"));
    assert!(theme.contains("--color-primary: #9a3412;"), "{theme}");

    // Template-only route from [[routes]].
    let gallery = read(&output.join("components/index.html"));
    assert!(gallery.contains("<h1>Component gallery</h1>"));
    assert!(gallery.contains("<details name=\"faq\">"));

    // Cache tags recorded per route.
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&output.join(".engine-manifest.json"))).unwrap();
    let cli = manifest["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "/docs/cli/")
        .unwrap();
    let tags: Vec<&str> = cli["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"i:docs:docs/cli"), "{tags:?}");
    assert!(tags.contains(&"c:docs"), "{tags:?}");
    assert!(tags.contains(&"s:site"), "{tags:?}");
}

#[test]
fn builds_the_static_blog_example() {
    let out = tempfile::tempdir().unwrap();
    let output = out.path().join("site");
    let report = build_site(
        &repo().join("examples/static-blog"),
        &BuildOptions {
            output: Some(output.clone()),
            drafts: false,
        },
    )
    .unwrap();
    let paths: Vec<&str> = report.routes.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"/blog/page/2/"), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p.contains("draft-ideas")),
        "drafts are skipped"
    );

    let post = read(&output.join("blog/sharpening-a-plane-iron/index.html"));
    // The site's own post_meta component overrides the built-in one.
    assert!(post.contains("Posted <time datetime=\"2026-11-20\">20 November 2026</time>"));
    assert!(post.contains("<table>"));
    assert!(post.contains(r#"<nav aria-label="Breadcrumb">"#));
    let blog = read(&output.join("blog/index.html"));
    assert!(blog.contains(r#"<nav aria-label="Pagination">"#));
    assert!(output.join("favicon.svg").is_file());

    // Drafts appear with --drafts.
    let with_drafts = build_site(
        &repo().join("examples/static-blog"),
        &BuildOptions {
            output: Some(out.path().join("drafts")),
            drafts: true,
        },
    )
    .unwrap();
    assert!(
        with_drafts
            .routes
            .iter()
            .any(|r| r.path == "/blog/draft-ideas/")
    );
}

#[test]
fn engine_new_scaffolds_a_site_that_builds() {
    let tmp = tempfile::tempdir().unwrap();
    let site = tmp.path().join("hello-site");
    let engine = env!("CARGO_BIN_EXE_engine");

    let new = Command::new(engine)
        .args(["new", site.to_str().unwrap(), "--preset", "static"])
        .output()
        .unwrap();
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );

    let build = Command::new(engine)
        .args(["build", "--site", site.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let home = read(&site.join("public/index.html"));
    assert!(home.contains("<title>hello-site</title>"), "{home}");

    let cms = Command::new(engine)
        .args([
            "new",
            tmp.path().join("c").to_str().unwrap(),
            "--preset",
            "cms",
        ])
        .output()
        .unwrap();
    if cfg!(feature = "cms") {
        assert!(
            cms.status.success(),
            "{}",
            String::from_utf8_lossy(&cms.stderr)
        );
        assert!(tmp.path().join("c/types/article.toml").is_file());
    } else {
        assert!(!cms.status.success());
        assert!(String::from_utf8_lossy(&cms.stderr).contains("--features cms"));
    }
}

#[test]
fn database_commands_need_a_cms_build() {
    let out = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(["migrate", "--site", "."])
        .output()
        .unwrap();
    if cfg!(feature = "content") {
        // Present: fails for another reason (no site.toml here).
        assert!(!String::from_utf8_lossy(&out.stderr).contains("not in this build"));
    } else {
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("not in this build") && err.contains("--features cms"),
            "{err}"
        );
    }
}

/// Building the cms example without a database (or with a static engine)
/// fails with a message saying what is missing.
#[test]
fn cms_example_without_a_database_fails_clearly() {
    let out = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args([
            "build",
            "--site",
            repo().join("examples/cms-site").to_str().unwrap(),
        ])
        .env_remove("ENGINE_DATABASE_URL")
        .env("ENGINE_DATABASE_URL", "")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    if cfg!(feature = "content") {
        assert!(err.contains("ENGINE_DATABASE_URL"), "{err}");
    } else {
        // A static engine rejects the site's publish mode first.
        assert!(err.contains("not in this build"), "{err}");
    }
}

#[cfg(not(feature = "publish"))]
#[test]
fn publish_mode_gives_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("site.toml"),
        "[site]\nname='x'\n[render]\ndefault='publish'\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(["build", "--site", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not in this build"), "{err}");
}

/// Features not compiled in have no routes: a static engine answers
/// nothing under /admin/ or /account/ (no login page to probe) and refuses
/// every POST.
#[cfg(not(feature = "auth"))]
#[test]
fn a_static_engine_has_no_account_or_admin_routes() {
    use std::io::{Read, Write};
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let st = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args([
            "build",
            "--site",
            repo().join("examples/static-blog").to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args([
            "serve",
            "--dir",
            out.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let get = |req: &str| -> String {
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                s.write_all(req.as_bytes()).unwrap();
                let mut r = String::new();
                s.read_to_string(&mut r).unwrap();
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("server did not start");
    };
    let admin = get("GET /admin/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let login = get("GET /account/login/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let post = get(
        "POST /account/login/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
    );
    let htmx =
        get("GET /_engine/ui/htmx-2.0.11.min.js HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let _ = child.kill();
    let _ = child.wait();
    assert!(admin.starts_with("HTTP/1.1 404"), "{admin}");
    assert!(login.starts_with("HTTP/1.1 404"), "{login}");
    assert!(post.starts_with("HTTP/1.1 405"), "{post}");
    assert!(htmx.starts_with("HTTP/1.1 404"), "{htmx}");
}
