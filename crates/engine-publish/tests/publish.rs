//! Acceptance test for roadmap v0.2: an edit rewrites only the affected
//! pages. Runs the real pipeline (types sync, migrations, build, tag index,
//! Postgres NOTIFY listener, atomic writes) against a throwaway database.
//!
//! Skipped unless ENGINE_TEST_DATABASE_URL is set (see engine_db::testing).

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use engine_content::schema::{self, SyncOptions};
use engine_content::store::{Changes, Key, Status};
use engine_content::{Registry, Store};
use engine_core::builder::BuildOptions;
use engine_db::migrate;
use engine_db::testing::TestDb;
use engine_publish::site::{CmsSite, all_migrations};
use engine_publish::{PublishReport, Publisher, listener};
use serde_json::json;
use tokio::sync::broadcast;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const FILES: &[(&str, &str)] = &[
    (
        "site.toml",
        r#"
[site]
name = "Publish Test"
base_url = "https://publish.example"

[render]
default = "publish"

[[routes]]
path = "/articles/"
template = "articles.html"
title = "Articles"

[[routes]]
path = "/articles/{slug}/"
type = "article"
template = "article.html"

[[routes]]
path = "/topics/{term}/"
taxonomy = "topics"
template = "topic.html"

[[routes]]
path = "/colophon/"
template = "colophon.html"
mode = "build"
title = "Colophon"
"#,
    ),
    (
        "content/_index.md",
        "+++\ntitle = 'Home'\ntemplate = 'home.html'\n+++\nWelcome.\n",
    ),
    (
        "content/about.md",
        "+++\ntitle = 'About'\n+++\nAbout this site.\n",
    ),
    (
        "types/article.toml",
        r#"
[[fields]]
name = "title"
type = "text"
required = true
[[fields]]
name = "body"
type = "markdown"
[[fields]]
name = "author"
type = "relation"
to = "author"
[[fields]]
name = "topics"
type = "terms"
vocabulary = "topics"
"#,
    ),
    (
        "types/author.toml",
        "title_field = 'name'\n[[fields]]\nname = 'name'\ntype = 'text'\nrequired = true\n",
    ),
    (
        "types/settings.toml",
        "singleton = true\n[[fields]]\nname = 'tagline'\ntype = 'text'\n",
    ),
    (
        "templates/home.html",
        r#"{% extends "base.html" %}{% block main %}<h1>{{ page.title }}</h1>
{% set s = get_singleton("settings") %}<p>{{ s.tagline }}</p>
<ul>{% for a in list_items("article", limit=10) %}<li><a href="{{ a.url }}">{{ a.title }}</a></li>{% endfor %}</ul>
{% endblock %}"#,
    ),
    (
        "templates/articles.html",
        r#"{% extends "base.html" %}{% block main %}<h1>Articles</h1>
<ul>{% for a in list_items("article") %}<li><a href="{{ a.url }}">{{ a.title }}</a></li>{% endfor %}</ul>
{% endblock %}"#,
    ),
    (
        "templates/article.html",
        r#"{% extends "base.html" %}{% block main %}<article><h1>{{ page.title }}</h1>
{% set by = get_item("author", page.author) %}{% if by %}<p>By {{ by.name }}</p>{% endif %}
{{ page.body | safe }}
<ul>{% for t in page.topics %}<li><a href="{{ t.url }}">{{ t.name }}</a></li>{% endfor %}</ul>
</article>{% endblock %}"#,
    ),
    (
        "templates/topic.html",
        r#"{% extends "base.html" %}{% block main %}<h1>{{ page.name }}</h1>
<ul>{% for a in list_items("article", term=page.slug) %}<li>{{ a.title }}</li>{% endfor %}</ul>
{% endblock %}"#,
    ),
    (
        "templates/colophon.html",
        r#"{% extends "base.html" %}{% block main %}<h1>Colophon</h1>
<p>{{ list_items("article") | length }} articles at build time.</p>{% endblock %}"#,
    ),
];

#[derive(Debug, Clone, PartialEq)]
struct FileState {
    sha: String,
    mtime: SystemTime,
    inode: u64,
}

fn snapshot(root: &Path) -> BTreeMap<String, FileState> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let meta = std::fs::metadata(&p).unwrap();
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.insert(
                    rel,
                    FileState {
                        sha: engine_publish::publisher::sha256_hex(&std::fs::read(&p).unwrap()),
                        mtime: meta.modified().unwrap(),
                        inode: meta.ino(),
                    },
                );
            }
        }
    }
    out
}

/// Files that differ between two snapshots (changed, added or removed).
fn touched(a: &BTreeMap<String, FileState>, b: &BTreeMap<String, FileState>) -> BTreeSet<String> {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

async fn next_report(rx: &mut broadcast::Receiver<PublishReport>) -> PublishReport {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("no publish within 15 s")
        .expect("publisher gone")
}

struct Fixture {
    _dir: tempfile::TempDir,
    site: PathBuf,
    out: PathBuf,
    store: Store,
    cms: Arc<CmsSite>,
}

async fn fixture(t: &TestDb) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    for (p, text) in FILES {
        write(&site, p, text);
    }
    let reg = Registry::load(&site).unwrap();
    schema::sync(&site, &reg, SyncOptions::default()).unwrap();
    migrate::apply(&t.db, &all_migrations(&site, t.backend()).unwrap())
        .await
        .unwrap();
    let cms = CmsSite::open(&site, t.db.clone(), tokio::runtime::Handle::current())
        .await
        .unwrap();
    let store = cms.store.clone();
    Fixture {
        out: site.join("public"),
        _dir: dir,
        site,
        store,
        cms: Arc::new(cms),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_rewrites_only_the_affected_pages() {
    let Some(t) = TestDb::create("an_edit_rewrites_only_the_affected_pages").await else {
        return;
    };
    let fx = fixture(&t).await;
    let s = &fx.store;

    let published = |name: &str| {
        Changes::default()
            .set("name", name)
            .status(Status::Published)
    };
    let ada = s.create("author", &published("Ada"), None).await.unwrap();
    let bo = s.create("author", &published("Bo"), None).await.unwrap();
    s.create(
        "settings",
        &Changes::default().set("tagline", "First tagline"),
        None,
    )
    .await
    .unwrap();
    let mut ids = BTreeMap::new();
    for (title, author, topics) in [
        ("Alpha", ada.id, json!(["Rust"])),
        ("Beta", ada.id, json!(["Rust", "Web"])),
        ("Gamma", bo.id, json!(["Web"])),
    ] {
        let a = s
            .create(
                "article",
                &Changes::default()
                    .set("title", title)
                    .set("body", format!("Body of {title}."))
                    .set("author", author)
                    .set("topics", topics)
                    .status(Status::Published),
                None,
            )
            .await
            .unwrap();
        ids.insert(title, a);
    }

    // engine build: every route, plus the tag index.
    let cms = fx.cms.clone();
    let report = blocking(move || cms.build(&BuildOptions::default()))
        .await
        .unwrap();
    let paths: Vec<&str> = report.routes.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/",
            "/about/",
            "/articles/",
            "/articles/alpha/",
            "/articles/beta/",
            "/articles/gamma/",
            "/colophon/",
            "/topics/rust/",
            "/topics/web/"
        ]
    );
    let beta_page = std::fs::read_to_string(fx.out.join("articles/beta/index.html")).unwrap();
    assert!(beta_page.contains("<p>By Ada</p>"), "{beta_page}");
    assert!(beta_page.contains("href=\"/topics/web/\""), "{beta_page}");
    let index = fx.cms.index();
    let beta_tags = index.tags_of("/articles/beta/").await.unwrap();
    let beta_id = ids["Beta"].id;
    assert!(
        beta_tags.contains(&format!("i:article:{beta_id}")),
        "{beta_tags:?}"
    );
    assert!(
        beta_tags.contains(&format!("i:author:{}", ada.id)),
        "{beta_tags:?}"
    );
    assert!(
        index.tags_of("/colophon/").await.unwrap().is_empty(),
        "build routes are not indexed"
    );

    // engine serve: publisher plus the NOTIFY listener.
    let cms = fx.cms.clone();
    let publisher: Arc<Publisher> = Arc::new(blocking(move || cms.publisher()).await.unwrap());
    let mut rx = publisher.subscribe();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let listen = tokio::spawn(listener::listen(t.db.clone(), publisher.clone(), async {
        let _ = stop_rx.await;
    }));
    // Give LISTEN a moment to register before the first edit.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 1. Edit one article's body.
    let before = snapshot(&fx.out);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let beta = ids["Beta"].clone();
    s.update(
        "article",
        beta.id,
        beta.version,
        &Changes::default().set("body", "A **new** body for Beta."),
        Some("test"),
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    let expected = set(&[
        "/",
        "/articles/",
        "/articles/beta/",
        "/topics/rust/",
        "/topics/web/",
    ]);
    assert_eq!(
        r.rendered.iter().cloned().collect::<BTreeSet<_>>(),
        expected,
        "{}",
        r.summary()
    );
    assert!(
        r.deleted.is_empty() && r.errors.is_empty(),
        "{}",
        r.summary()
    );
    let after = snapshot(&fx.out);
    assert_eq!(
        touched(&before, &after),
        set(&[
            "index.html",
            "articles/index.html",
            "articles/beta/index.html",
            "topics/rust/index.html",
            "topics/web/index.html"
        ]),
        "exactly the pages carrying i:article:{beta_id} or c:article were rewritten"
    );
    for untouched in [
        "articles/alpha/index.html",
        "articles/gamma/index.html",
        "about/index.html",
        "colophon/index.html",
        "404.html",
        "sitemap.xml",
    ] {
        assert_eq!(
            before[untouched], after[untouched],
            "{untouched} must not be rewritten"
        );
    }
    assert!(
        after["articles/beta/index.html"].inode != before["articles/beta/index.html"].inode,
        "files are replaced by rename, not rewritten in place"
    );
    let beta_page = std::fs::read_to_string(fx.out.join("articles/beta/index.html")).unwrap();
    assert!(
        beta_page.contains("A <strong>new</strong> body"),
        "{beta_page}"
    );

    // 2. Rename an author: only the pages that showed that author.
    let before = snapshot(&fx.out);
    s.update(
        "author",
        bo.id,
        bo.version,
        &Changes::default().set("name", "Bo Diddley"),
        None,
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    assert_eq!(r.rendered, ["/articles/gamma/"], "{}", r.summary());
    assert_eq!(
        touched(&before, &snapshot(&fx.out)),
        set(&["articles/gamma/index.html"])
    );

    // 3. A singleton: only pages that read it.
    let settings = s.singleton("settings").await.unwrap().unwrap();
    s.update(
        "settings",
        settings.id,
        settings.version,
        &Changes::default().set("tagline", "Second tagline"),
        None,
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    assert_eq!(r.rendered, ["/"], "{}", r.summary());

    // 4. An edit made directly in SQL (as from psql) is published too.
    let before = snapshot(&fx.out);
    engine_db::query("UPDATE content_article SET title = 'Alpha (edited in SQL)' WHERE id = $1")
        .bind(ids["Alpha"].id)
        .execute(&t.db)
        .await
        .unwrap();
    let r = next_report(&mut rx).await;
    assert!(
        r.rendered.contains(&"/articles/alpha/".to_string()),
        "{}",
        r.summary()
    );
    assert!(
        !r.rendered.contains(&"/articles/gamma/".to_string()),
        "{}",
        r.summary()
    );
    assert!(!touched(&before, &snapshot(&fx.out)).contains("articles/gamma/index.html"));

    // 5. Changing a slug moves the page: old file deleted, new one written.
    let gamma = s
        .get("article", &Key::Slug("gamma".into()), None)
        .await
        .unwrap()
        .unwrap();
    s.update(
        "article",
        gamma.id,
        gamma.version,
        &Changes {
            slug: Some("gamma-ray".into()),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    assert_eq!(r.deleted, ["/articles/gamma/"], "{}", r.summary());
    assert!(
        r.rendered.contains(&"/articles/gamma-ray/".to_string()),
        "{}",
        r.summary()
    );
    assert!(!fx.out.join("articles/gamma").exists());
    assert!(fx.out.join("articles/gamma-ray/index.html").is_file());

    // 6. Unpublishing removes the page and the listings drop it.
    let alpha = s
        .get("article", &Key::Id(ids["Alpha"].id), None)
        .await
        .unwrap()
        .unwrap();
    s.update(
        "article",
        alpha.id,
        alpha.version,
        &Changes::default().status(Status::Draft),
        None,
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    assert_eq!(r.deleted, ["/articles/alpha/"], "{}", r.summary());
    let listing = std::fs::read_to_string(fx.out.join("articles/index.html")).unwrap();
    assert!(!listing.contains("Alpha"), "{listing}");

    // 7. A new article appears with its page and in the listings.
    s.create(
        "article",
        &Changes::default()
            .set("title", "Delta")
            .set("author", ada.id)
            .status(Status::Published),
        None,
    )
    .await
    .unwrap();
    let r = next_report(&mut rx).await;
    assert!(
        r.rendered.contains(&"/articles/delta/".to_string()),
        "{}",
        r.summary()
    );
    assert!(
        r.rendered.contains(&"/articles/".to_string()),
        "{}",
        r.summary()
    );

    // The build route was never touched by publishing.
    let colophon = std::fs::read_to_string(fx.out.join("colophon/index.html")).unwrap();
    assert!(colophon.contains("3 articles at build time"), "{colophon}");

    // Render-on-miss: a deleted publish file comes back; unknown paths and
    // build routes do not render.
    std::fs::remove_file(fx.out.join("articles/beta/index.html")).unwrap();
    let p = publisher.clone();
    assert!(
        blocking(move || p.render_missing("/articles/beta/"))
            .await
            .unwrap()
    );
    let again = std::fs::read_to_string(fx.out.join("articles/beta/index.html")).unwrap();
    assert!(again.contains("A <strong>new</strong> body"), "{again}");
    for path in [
        "/articles/nope/",
        "/articles/../etc/",
        "/colophon/",
        "/x/y/z/",
    ] {
        let p = publisher.clone();
        let path2 = path.to_string();
        assert!(
            !blocking(move || p.render_missing(&path2)).await.unwrap(),
            "{path}"
        );
    }
    let _ = next_report(&mut rx).await; // the miss render's report

    // rebuild --tag: only the tagged pages.
    let p = publisher.clone();
    let tag = engine_core::tags::CacheTag::item("author", ada.id.to_string());
    let r = blocking(move || p.rebuild_tags(&[tag])).await.unwrap();
    let got: BTreeSet<String> = r.rendered.into_iter().collect();
    assert_eq!(got, set(&["/articles/beta/", "/articles/delta/"]));

    let _ = stop_tx.send(());
    listen.await.unwrap().unwrap();
    drop(publisher);
    let _ = fx.site;
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missed_notifications_are_caught_up_from_the_change_log() {
    let Some(t) = TestDb::create("missed_notifications_are_caught_up").await else {
        return;
    };
    let fx = fixture(&t).await;
    let s = &fx.store;
    let a = s
        .create(
            "article",
            &Changes::default()
                .set("title", "One")
                .status(Status::Published),
            None,
        )
        .await
        .unwrap();
    let cms = fx.cms.clone();
    blocking(move || cms.build(&BuildOptions::default()))
        .await
        .unwrap();

    // Edit while no listener runs ("engine serve" is down).
    s.update(
        "article",
        a.id,
        a.version,
        &Changes::default().set("title", "One, edited"),
        None,
    )
    .await
    .unwrap();
    let cms = fx.cms.clone();
    let publisher = Arc::new(blocking(move || cms.publisher()).await.unwrap());
    let mut rx = publisher.subscribe();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let listen = tokio::spawn(listener::listen(t.db.clone(), publisher.clone(), async {
        let _ = stop_rx.await;
    }));
    let r = next_report(&mut rx).await;
    assert!(
        r.rendered.contains(&"/articles/one/".to_string()),
        "{}",
        r.summary()
    );
    let page = std::fs::read_to_string(fx.out.join("articles/one/index.html")).unwrap();
    assert!(page.contains("One, edited"), "{page}");
    // Processed changes are pruned.
    let left: i64 = engine_db::query("SELECT count(*) FROM engine_changes")
        .scalar(&t.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let _ = stop_tx.send(());
    listen.await.unwrap().unwrap();
    t.cleanup().await;
}

/// Regression test for the v0.5 flake: a `LISTEN` connection used to be
/// taken out of the shared pool for good (two listeners in `engine serve`),
/// so requests and publish batches queued for what was left. Listeners now
/// have connections of their own: with both listening, every connection of
/// the shared pool can still be held at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listeners_do_not_take_connections_from_the_pool() {
    let Some(t) = TestDb::create_postgres("listeners_keep_the_pool_whole").await else {
        return;
    };
    let mut publisher = t.db.pg_listener().await.unwrap();
    publisher.listen("engine_changes").await.unwrap();
    let mut jobs = t.db.pg_listener().await.unwrap();
    jobs.listen("engine_jobs").await.unwrap();
    let size = t.db.postgres().unwrap().options().get_max_connections();
    let mut held = Vec::new();
    for i in 0..size {
        let c = tokio::time::timeout(Duration::from_secs(3), t.db.acquire())
            .await
            .unwrap_or_else(|_| panic!("connection {} of {size} not available", i + 1))
            .unwrap();
        held.push(c);
    }
    drop(held);
    drop((publisher, jobs));
    t.cleanup().await;
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == "public" || name == "data" {
            continue;
        }
        let dest = to.join(&name);
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// The bulk preload of `engine build` answers `get_item`, `list_items`,
/// `related_items`, list routes and singletons from memory. It must give
/// exactly the pages and cache tags the per-page queries give: the
/// example CMS site (related items, topics, paginated lists, settings) is
/// built both ways and compared byte for byte, with several threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preloaded_build_matches_the_per_page_queries() {
    let Some(t) = TestDb::create("preloaded_build_matches").await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site"),
        &site,
    );
    migrate::apply(&t.db, &all_migrations(&site, t.backend()).unwrap())
        .await
        .unwrap();
    let cms = Arc::new(
        CmsSite::open(&site, t.db.clone(), tokio::runtime::Handle::current())
            .await
            .unwrap(),
    );
    engine_content::seed::seed_dir(&cms.store, &site, Some("test"))
        .await
        .unwrap();
    let mut outs = Vec::new();
    for (preload, name) in [(false, "queried"), (true, "preloaded")] {
        cms.content.set_preload(preload);
        let out = dir.path().join(name);
        let c = cms.clone();
        let o = out.clone();
        let report = blocking(move || {
            c.build(&BuildOptions {
                output: Some(o),
                jobs: Some(4),
                ..Default::default()
            })
        })
        .await
        .unwrap();
        let tags: Vec<(String, Vec<String>)> = report
            .routes
            .iter()
            .map(|r| {
                let mut t = r.tags.clone();
                t.sort();
                (r.path.clone(), t)
            })
            .collect();
        outs.push((read_tree(&out), tags));
    }
    let (a, b) = (&outs[0], &outs[1]);
    assert!(a.0.len() > 10, "the example has pages");
    assert_eq!(
        a.0.keys().collect::<Vec<_>>(),
        b.0.keys().collect::<Vec<_>>()
    );
    for (k, v) in &a.0 {
        if k.ends_with(".engine-manifest.json") {
            continue;
        }
        assert!(
            b.0[k] == *v,
            "{k} differs:\n{}\n---\n{}",
            String::from_utf8_lossy(v),
            String::from_utf8_lossy(&b.0[k])
        );
    }
    assert_eq!(a.1, b.1, "the same cache tags per page");
    t.cleanup().await;
}
