//! Search against the test database (Postgres or SQLite; skipped without
//! ENGINE_TEST_DATABASE_URL).

use std::path::Path;
use std::sync::Arc;

use engine_content::schema::{self, SyncOptions};
use engine_content::store::{Changes, Status};
use engine_content::{ContentSite, Registry, Store};
use engine_db::migrate;
use engine_db::testing::TestDb;
use engine_search::{SearchConfig, SearchIndex};
use serde_json::json;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn published_items_are_found_ranked_and_highlighted() {
    let Some(t) = TestDb::create("search_items").await else {
        return;
    };
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "site.toml",
        "[site]\nname='T'\n[[routes]]\npath='/posts/{slug}/'\ntype='post'\ntemplate='post.html'\n",
    );
    write(d.path(), "templates/post.html", "{{ page.title }}");
    write(
        d.path(),
        "types/post.toml",
        "title_field='title'\n[[fields]]\nname='title'\ntype='text'\nrequired=true\n\
         [[fields]]\nname='body'\ntype='markdown'\n[[fields]]\nname='topics'\ntype='terms'\nvocabulary='topics'\n",
    );
    let reg = Registry::load(d.path()).unwrap();
    schema::sync(d.path(), &reg, SyncOptions::default()).unwrap();
    let b = t.backend();
    let mut all = engine_content::migrations(b);
    all.extend(engine_search::migrations(b));
    all.extend(migrate::site_migrations(d.path(), b).unwrap());
    migrate::apply(&t.db, &all).await.unwrap();
    let store = Store::new(t.db.clone(), Arc::new(reg));
    let cfg = engine_core::builder::load_config(d.path()).unwrap().0;
    let site = ContentSite::new(store.clone(), tokio::runtime::Handle::current(), &cfg).unwrap();
    let index = SearchIndex::new(t.db.clone(), site, SearchConfig::default());

    let mk = |title: &str, body: &str, status: Status| {
        Changes::default()
            .set("title", title)
            .set("body", body)
            .set("topics", json!(["Iron"]))
            .status(status)
    };
    let a = store
        .create(
            "post",
            &mk(
                "Wrought iron gates",
                "Forged by hand in the <b>smithy</b>.",
                Status::Published,
            ),
            None,
        )
        .await
        .unwrap();
    store
        .create(
            "post",
            &mk(
                "Garden notes",
                "The gate to the garden is iron, and old.",
                Status::Published,
            ),
            None,
        )
        .await
        .unwrap();
    store
        .create(
            "post",
            &mk("Secret iron draft", "Not for the public.", Status::Draft),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        index.reindex_all().await.unwrap(),
        2,
        "drafts are not indexed"
    );

    // Stemming: "gate" finds "gates" and "gate"; the title match ranks first.
    let r = index.search("gate", None, 10, 1).await.unwrap();
    assert_eq!(r.total, 2, "{r:?}");
    assert_eq!(r.hits[0].title, "Wrought iron gates");
    assert_eq!(r.hits[0].url.as_deref(), Some("/posts/wrought-iron-gates/"));
    // Snippets are escaped text with marks, never the stored markup.
    let all_snips: String = r.hits.iter().map(|h| h.snippet.clone()).collect();
    assert!(all_snips.contains("<mark>"), "{all_snips}");
    assert!(!all_snips.contains("<b>"), "{all_snips}");
    // Terms are searchable; drafts are not.
    assert_eq!(index.search("secret", None, 10, 1).await.unwrap().total, 0);
    // Query syntax is not an attack surface.
    for q in ["\"", "a OR", "NEAR(", "title:x*", "' OR 1=1 --", "-", "*"] {
        index.search(q, None, 10, 1).await.unwrap();
    }
    // Paging.
    let p2 = index.search("iron", None, 1, 2).await.unwrap();
    assert_eq!((p2.total, p2.pages, p2.hits.len()), (2, 2, 1));
    // An edit, then a reindex of that item; unpublishing removes it.
    store
        .update(
            "post",
            a.id,
            a.version,
            &Changes::default().set("title", "Railings"),
            None,
        )
        .await
        .unwrap();
    index.reindex_item("post", a.id).await.unwrap();
    assert_eq!(
        index.search("railings", None, 10, 1).await.unwrap().total,
        1
    );
    let cur = store
        .get("post", &engine_content::Key::Id(a.id), None)
        .await
        .unwrap()
        .unwrap();
    store
        .update(
            "post",
            a.id,
            cur.version,
            &Changes::default().status(Status::Draft),
            None,
        )
        .await
        .unwrap();
    index.reindex_item("post", a.id).await.unwrap();
    assert_eq!(
        index.search("railings", None, 10, 1).await.unwrap().total,
        0
    );
    assert_eq!(index.len().await.unwrap(), 1);
    t.cleanup().await;
}
