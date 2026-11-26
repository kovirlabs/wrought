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
    assert!(!cms.status.success());
    assert!(String::from_utf8_lossy(&cms.stderr).contains("not available yet"));
}

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
