//! Store tests against the test database (Postgres or SQLite, chosen by
//! ENGINE_TEST_DATABASE_URL; skipped when unset, see engine_db::testing).

use std::path::Path;
use std::sync::Arc;

use engine_content::schema::{self, SyncOptions};
use engine_content::store::{Changes, Key, ListQuery, SaveError, Status, Store};
use engine_content::{ContentSite, Registry};
use engine_db::migrate;
use engine_db::testing::TestDb;
use serde_json::json;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const SITE: &[(&str, &str)] = &[
    (
        "site.toml",
        "[site]\nname='T'\n[[routes]]\npath='/posts/{slug}/'\ntype='post'\ntemplate='post.html'\n[[routes]]\npath='/topics/{term}/'\ntaxonomy='topics'\ntemplate='topic.html'\n",
    ),
    (
        "types/person.toml",
        "title_field='name'\n[[fields]]\nname='name'\ntype='text'\nrequired=true\n",
    ),
    (
        "types/post.toml",
        r#"
[[fields]]
name = "title"
type = "text"
required = true
[[fields]]
name = "body"
type = "markdown"
[[fields]]
name = "link"
type = "url"
[[fields]]
name = "day"
type = "date"
[[fields]]
name = "rank"
type = "integer"
[[fields]]
name = "by"
type = "relation"
to = "person"
[[fields]]
name = "topics"
type = "terms"
vocabulary = "topics"
[[fields]]
name = "parts"
type = "blocks"
"#,
    ),
    (
        "types/blocks/callout.toml",
        "[[fields]]\nname='text'\ntype='markdown'\nrequired=true\n[[fields]]\nname='href'\ntype='url'\n",
    ),
    (
        "types/settings.toml",
        "singleton=true\n[[fields]]\nname='tagline'\ntype='text'\n",
    ),
];

async fn setup(t: &TestDb) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    for (p, text) in SITE {
        write(d.path(), p, text);
    }
    let reg = Registry::load(d.path()).unwrap();
    schema::sync(d.path(), &reg, SyncOptions::default()).unwrap();
    let b = t.backend();
    let mut all = engine_content::migrations(b);
    all.extend(migrate::site_migrations(d.path(), b).unwrap());
    migrate::apply(&t.db, &all).await.unwrap();
    (d, Store::new(t.db.clone(), Arc::new(reg)))
}

async fn count(t: &TestDb, sql: &str) -> i64 {
    engine_db::query(sql).scalar(&t.db).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_update_conflict_revisions_terms() {
    let Some(t) = TestDb::create("create_update_conflict_revisions_terms").await else {
        return;
    };
    let (_d, store) = setup(&t).await;

    let ada = store
        .create(
            "person",
            &Changes::default().set("name", "Ada"),
            Some("seed"),
        )
        .await
        .unwrap();
    assert_eq!(ada.slug, "ada");

    let post = store
        .create(
            "post",
            &Changes::default()
                .set("title", "Hello, World")
                .set("body", "# Hi\n\n<script>alert(1)</script>**ok**")
                .set("day", "2026-11-20")
                .set("rank", "3")
                .set("by", "ada")
                .set("topics", json!(["Rust", "Web"]))
                .set(
                    "parts",
                    json!([{"type": "callout", "text": "*x*", "href": "javascript:evil()"}]),
                )
                .status(Status::Published),
            Some("alice"),
        )
        .await;
    // The callout's javascript: link is refused at save time.
    let err = post.unwrap_err();
    assert!(matches!(err, SaveError::Invalid(_)), "{err}");
    assert!(err.to_string().contains("parts"), "{err}");

    let post = store
        .create(
            "post",
            &Changes::default()
                .set("title", "Hello, World")
                .set("body", "# Hi\n\n<script>alert(1)</script>**ok**")
                .set("day", "2026-11-20")
                .set("rank", "3")
                .set("by", "ada")
                .set("topics", json!(["Rust", "Web"]))
                .set("parts", json!([{"type": "callout", "text": "*x*"}]))
                .status(Status::Published),
            Some("alice"),
        )
        .await
        .unwrap();
    assert_eq!(post.slug, "hello-world");
    assert_eq!(post.version, 1);
    assert_eq!(post.fields["day"], "2026-11-20");
    assert_eq!(post.fields["rank"], 3);
    assert_eq!(post.fields["by"], ada.id);
    assert_eq!(post.fields["topics"].as_array().unwrap().len(), 2);
    assert!(post.published_at.is_some());

    // Same title: unique slug.
    let twin = store
        .create(
            "post",
            &Changes::default().set("title", "Hello world"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(twin.slug, "hello-world-2");
    assert_eq!(twin.status, Status::Draft);

    // Required, unknown fields, bad relation.
    let err = store
        .create(
            "post",
            &Changes::default().set("nope", 1).set("by", "nobody"),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`title` is required") && err.contains("`nope`"),
        "{err}"
    );
    let err = store
        .create(
            "post",
            &Changes::default().set("title", "x").set("by", "nobody"),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("refers to no person"), "{err}");

    // Update with the right version; then a stale one conflicts.
    let v2 = store
        .update(
            "post",
            post.id,
            1,
            &Changes::default().set("title", "Hello again"),
            Some("bob"),
        )
        .await
        .unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(v2.slug, "hello-world", "slugs stay stable");
    let stale = store
        .update(
            "post",
            post.id,
            1,
            &Changes::default().set("title", "Lost update"),
            Some("carol"),
        )
        .await
        .unwrap_err();
    match &stale {
        SaveError::Conflict {
            expected, current, ..
        } => assert_eq!((*expected, *current), (1, 2)),
        other => panic!("expected a conflict, got {other}"),
    }
    let now = store
        .get("post", &Key::Id(post.id), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        now.fields["title"], "Hello again",
        "the stale save changed nothing"
    );

    // Revisions: one per save, in order, with authors.
    let revs = store.revisions("post", post.id).await.unwrap();
    let vs: Vec<(i32, &str, Option<&str>)> = revs
        .iter()
        .map(|r| (r.version, r.op.as_str(), r.author.as_deref()))
        .collect();
    assert_eq!(
        vs,
        [(2, "update", Some("bob")), (1, "create", Some("alice"))]
    );
    assert_eq!(revs[0].data["fields"]["title"], "Hello again");

    // A save whose revision cannot be written rolls back entirely: make the
    // next revision row collide, then try to save.
    engine_db::query(
        "INSERT INTO engine_revisions (content_type, item_id, version, op, data) \
         VALUES ('post', $1, 3, 'update', '{}')",
    )
    .bind(post.id)
    .execute(&t.db)
    .await
    .unwrap();
    assert!(
        store
            .update(
                "post",
                post.id,
                2,
                &Changes::default().set("title", "Doomed"),
                None
            )
            .await
            .is_err()
    );
    let after = store
        .get("post", &Key::Id(post.id), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.version, after.fields["title"].as_str()),
        (2, Some("Hello again"))
    );

    // Terms and term filters.
    let topics = store.terms("topics").await.unwrap();
    let names: Vec<&str> = topics.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Rust", "Web"]);
    let rust = store
        .list(
            "post",
            &ListQuery {
                term: Some("rust".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rust.len(), 1);
    assert!(
        store
            .list(
                "post",
                &ListQuery {
                    term: Some("topics/nothing".into()),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .is_empty()
    );
    // Published only by default; drafts with status None.
    assert_eq!(
        store
            .list("post", &ListQuery::default())
            .await
            .unwrap()
            .len(),
        1
    );
    let all = store
        .list(
            "post",
            &ListQuery {
                status: None,
                order: "title".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].fields["title"], "Hello again");
    assert!(
        store
            .list(
                "post",
                &ListQuery {
                    order: "parts".into(),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );

    // Singletons.
    store
        .create("settings", &Changes::default().set("tagline", "Hi"), None)
        .await
        .unwrap();
    assert!(
        store
            .create("settings", &Changes::default(), None)
            .await
            .is_err()
    );
    assert_eq!(
        store.singleton("settings").await.unwrap().unwrap().slug,
        "settings"
    );

    // Deleting checks the version and keeps a final revision.
    assert!(matches!(
        store.delete("post", twin.id, Some(7), None).await,
        Err(SaveError::Conflict { .. })
    ));
    store
        .delete("post", twin.id, Some(1), Some("dan"))
        .await
        .unwrap();
    assert!(
        store
            .get("post", &Key::Id(twin.id), None)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.revisions("post", twin.id).await.unwrap()[0].op,
        "delete"
    );

    // Every change landed in the change log (via triggers).
    let changes = count(
        &t,
        "SELECT count(*) FROM engine_changes WHERE content_type = 'post'",
    )
    .await;
    assert!(changes >= 4, "{changes}");
    let term_changes = count(
        &t,
        "SELECT count(*) FROM engine_changes WHERE content_type = 'topics'",
    )
    .await;
    assert_eq!(term_changes, 2);

    // The database refuses unsafe slugs even from raw SQL.
    assert!(
        engine_db::query("UPDATE content_post SET slug = '../x' WHERE id = $1")
            .bind(post.id)
            .execute(&t.db)
            .await
            .is_err()
    );

    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn template_values_are_sanitised_even_after_raw_sql_edits() {
    let Some(t) = TestDb::create("template_values_are_sanitised").await else {
        return;
    };
    let (d, store) = setup(&t).await;
    let post = store
        .create(
            "post",
            &Changes::default()
                .set("title", "<b>T</b>")
                .set("body", "ok")
                .set("parts", json!([{"type": "callout", "text": "fine"}]))
                .set("topics", "Rust")
                .status(Status::Published),
            None,
        )
        .await
        .unwrap();
    // Someone edits the row directly in SQL with hostile values.
    engine_db::query(
        "UPDATE content_post SET body = $1, link = 'javascript:alert(1)', \
         parts = $2 WHERE id = $3",
    )
    .bind("<img src=x onerror=alert(1)>[x](javascript:alert(2))<script>alert(3)</script>")
    .bind(
        json!([{"type": "callout", "text": "<script>x</script>", "href": "javascript:y"},
                 {"type": "../../etc/passwd", "text": "?"}]),
    )
    .bind(post.id)
    .execute(&t.db)
    .await
    .unwrap();

    let cfg = engine_core::builder::load_config(d.path()).unwrap().0;
    let rt = tokio::runtime::Handle::current();
    let site = ContentSite::new(store.clone(), rt, &cfg).unwrap();
    let item = store
        .get("post", &Key::Id(post.id), None)
        .await
        .unwrap()
        .unwrap();
    let ty = store.registry().get("post").unwrap();
    // What templates get: markdown is safe-marked HTML, plain text is not.
    let tv = site.item_value(ty, &item);
    assert!(tv.get_attr("body").unwrap().is_safe());
    assert!(!tv.get_attr("title").unwrap().is_safe());
    let v = site.item_json(ty, &item);
    let body = v["body"].as_str().unwrap().to_ascii_lowercase();
    assert!(
        !body.contains("onerror") && !body.contains("javascript:") && !body.contains("<script"),
        "{body}"
    );
    assert!(v["link"].is_null());
    let parts = v["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1, "unknown block types are dropped");
    assert!(!parts[0]["text"].as_str().unwrap().contains("<script"));
    assert!(parts[0]["href"].is_null());
    assert_eq!(
        v["title"], "<b>T</b>",
        "plain text stays text; templates escape it"
    );
    assert_eq!(v["url"], "/posts/b-t-b/");
    assert_eq!(v["topics"][0]["url"], "/topics/rust/");
    t.cleanup().await;
}

/// A template function whose query finds the database unavailable fails
/// the render with an error that still says so (DECISIONS.md, "Fix
/// intermittent outage failures"): the request path answers `503` or the
/// published copy from that, not from the health flag (which can already
/// say "up" again). Forced here with a closed pool, which the health flag
/// never sees.
#[tokio::test(flavor = "multi_thread")]
async fn a_template_function_keeps_the_database_error() {
    use engine_core::SiteExtension as _;
    let Some(t) = TestDb::create("template_function_keeps_db_error").await else {
        return;
    };
    let (d, store) = setup(&t).await;
    let cfg = engine_core::builder::load_config(d.path()).unwrap().0;
    let site = ContentSite::new(store.clone(), tokio::runtime::Handle::current(), &cfg).unwrap();
    let mut env = minijinja::Environment::new();
    site.extend_env(&mut env);
    env.add_template(
        "home.html",
        "<ul>{% for u in list_items(\"post\", limit=3) %}<li>{{ u.title }}</li>{% endfor %}</ul>",
    )
    .unwrap();
    let render = move |env: minijinja::Environment<'static>| {
        tokio::task::spawn_blocking(move || {
            env.get_template("home.html")
                .and_then(|t| t.render(()))
                .map_err(|e| {
                    engine_core::templates::render_error(e).context("rendering / with home.html")
                })
        })
    };
    let ok = render(env.clone()).await.unwrap();
    assert!(ok.is_ok(), "{ok:?}");
    assert!(!t.db.health().is_down());

    t.db.close().await;
    let e = render(env).await.unwrap().unwrap_err();
    assert!(
        engine_db::health::is_unavailable_any(&e),
        "not recognised as unavailable: {e:#}"
    );
    assert!(
        !t.db.health().is_down(),
        "the flag alone would have said 500"
    );
    // The message still names the template and the cause.
    let msg = format!("{e:#}");
    assert!(msg.contains("home.html") && msg.contains("closed"), "{msg}");
    t.cleanup().await;
}
