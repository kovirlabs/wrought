//! Security regression tests for the review finding on v0.2:
//! `examples/cms-site/templates/author.html` printed `page.bio` raw and put
//! `page.website` into an `href`. These tests build the real example site
//! against a throwaway database, plant hostile values (through the store
//! *and* directly in SQL, which skips save-time validation) and check what
//! reaches the published HTML.
//!
//! Skipped unless ENGINE_TEST_DATABASE_URL is set (see engine_db::testing).

use std::path::{Path, PathBuf};

use engine_content::store::{Changes, Key, SaveError};
use engine_core::builder::BuildOptions;
use engine_db::migrate;
use engine_db::testing::TestDb;
use engine_publish::site::{CmsSite, all_migrations};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == "public" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(&name));
        } else {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
}

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site")
}

const HOSTILE_MARKDOWN: &str = "Hi <script>alert('bio')</script> <img src=x onerror=alert(1)> \
    <a href=\"javascript:alert(2)\">a</a> [b](javascript:alert(3)) [c](data:text/html,<script>x</script>) \
    <svg onload=alert(4)></svg> <iframe src=\"https://evil.example\"></iframe> \
    <a href=\"JaVaScRiPt&#58;alert(5)\">d</a> **still bold**";

fn assert_clean(html: &str, what: &str) {
    let lower = html.to_ascii_lowercase();
    for bad in [
        "<script>alert",
        "onerror",
        "onload",
        "javascript:",
        "javascript&#58;",
        "data:text",
        "<iframe",
        "<svg",
    ] {
        assert!(!lower.contains(bad), "{what}: found `{bad}` in:\n{html}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn author_bio_and_website_cannot_carry_script() {
    let Some(t) = TestDb::create("author_bio_and_website_cannot_carry_script").await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(&example(), &site);
    migrate::apply(&t.db, &all_migrations(&site, t.backend()).unwrap())
        .await
        .unwrap();
    let cms = CmsSite::open(&site, t.db.clone(), tokio::runtime::Handle::current())
        .await
        .unwrap();
    let store = cms.store.clone();
    engine_content::seed::seed_dir(&store, &site, Some("test"))
        .await
        .unwrap();

    // 1. Save-time checks: a `url` field refuses script and data URLs.
    let ada = store
        .get("author", &Key::Slug("ada".into()), None)
        .await
        .unwrap()
        .unwrap();
    for bad in [
        "javascript:alert(1)",
        " JaVaScRiPt:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:x",
        "//evil.example/",
        "https://x\" onmouseover=\"alert(1)",
    ] {
        let err = store
            .update(
                "author",
                ada.id,
                ada.version,
                &Changes::default().set("website", bad),
                Some("test"),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SaveError::Invalid(_)), "{bad}: {err}");
    }

    // 2. Hostile Markdown through the store (Markdown is free text).
    let ada = store
        .update(
            "author",
            ada.id,
            ada.version,
            &Changes::default()
                .set("bio", HOSTILE_MARKDOWN)
                .set("name", "<b onclick=alert(6)>Ada</b>"),
            Some("test"),
        )
        .await
        .unwrap();
    // 3. Hostile values written straight into the database, past
    // validation: a script URL in the url field, script in a text field
    // shown by a listing component, and in a block.
    engine_db::query("UPDATE content_author SET website = 'javascript:alert(7)' WHERE id = $1")
        .bind(ada.id)
        .execute(&t.db)
        .await
        .unwrap();
    engine_db::query(
        "UPDATE content_article SET summary = '<script>alert(8)</script>', sections = $1",
    )
    .bind(serde_json::json!([{"type": "callout", "text": "<img src=x onerror=alert(9)>", "title": "<script>t</script>"}]))
    .execute(&t.db)
    .await
    .unwrap();
    // Templates may also list items through built-in components.
    std::fs::write(
        site.join("templates/articles.html"),
        "{% extends \"base.html\" %}{% block main %}<h1>{{ page.title }}</h1>\
         {{ component(\"page_list\", pages=list_items(\"article\")) }}{% endblock %}",
    )
    .unwrap();

    // Re-open so the edited template is read, then build (blocking).
    let cms = std::sync::Arc::new(
        CmsSite::open(&site, t.db.clone(), tokio::runtime::Handle::current())
            .await
            .unwrap(),
    );
    let report = tokio::task::spawn_blocking(move || cms.build(&BuildOptions::default()).unwrap())
        .await
        .unwrap();
    assert!(
        !report.warnings.iter().any(|w| w.contains("| safe")),
        "the example's templates use no `| safe`: {:?}",
        report.warnings
    );

    let read = |p: &str| std::fs::read_to_string(site.join("public").join(p)).unwrap();
    let author = read("authors/ada/index.html");
    assert_clean(&author, "author page");
    assert!(author.contains("<strong>still bold</strong>"), "{author}");
    assert!(
        author.contains("&lt;b onclick=alert(6)&gt;Ada&lt;/b&gt;"),
        "the name is text: {author}"
    );
    assert!(
        !author.contains(">Website</a>"),
        "an unsafe website is dropped at render time: {author}"
    );
    let article = read("articles/flattening-a-plane-sole/index.html");
    assert_clean(&article, "article page");
    assert!(
        article.contains("&lt;script&gt;t&lt;/script&gt;"),
        "{article}"
    );
    let listing = read("articles/index.html");
    assert_clean(&listing, "article listing");
    assert!(
        listing.contains("&lt;script&gt;alert(8)&lt;/script&gt;"),
        "text fields are escaped even in components: {listing}"
    );
    t.cleanup().await;
}
