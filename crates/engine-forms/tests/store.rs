//! Admin forms and submissions against PostgreSQL (skipped without
//! ENGINE_TEST_DATABASE_URL).

use engine_db::migrate;
use engine_db::testing::TestDb;
use engine_forms::{FormDef, FormStore, Forms, SaveError, Source};

#[tokio::test(flavor = "multi_thread")]
async fn admin_forms_are_version_checked_and_files_win() {
    let Some(t) = TestDb::create("admin_forms_are_version_checked").await else {
        return;
    };
    migrate::apply(&t.db, &engine_forms::migrations(t.db.backend()))
        .await
        .unwrap();
    let site = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(site.path().join("forms")).unwrap();
    std::fs::write(
        site.path().join("forms/feedback.toml"),
        "title = 'From the file'\n[[fields]]\nname = 'text'\n",
    )
    .unwrap();
    let store = FormStore::new(t.db.clone());
    let def = FormDef::parse_toml("survey", "title = 'Survey'\n[[fields]]\nname = 'q'\n").unwrap();
    store.create_form("survey", &def.to_json()).await.unwrap();
    assert!(matches!(
        store.create_form("survey", &def.to_json()).await,
        Err(SaveError::Exists(_))
    ));
    // Two admins edit version 1; the second save is a conflict.
    assert_eq!(
        store
            .update_form("survey", 1, &def.to_json())
            .await
            .unwrap(),
        2
    );
    assert!(matches!(
        store.update_form("survey", 1, &def.to_json()).await,
        Err(SaveError::Conflict {
            expected: 1,
            current: 2
        })
    ));
    assert!(matches!(
        store.delete_form("survey", 1).await,
        Err(SaveError::Conflict { .. })
    ));
    // The site's file wins over an admin form of the same name; built-ins
    // exist without files.
    store.create_form("feedback", &def.to_json()).await.unwrap();
    let forms = Forms::open(
        site.path(),
        Some(t.db.clone()),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    let f = forms.get("feedback").await.unwrap().unwrap();
    assert_eq!(
        (f.def.title.as_str(), f.source),
        ("From the file", Source::File)
    );
    assert_eq!(
        forms.get("survey").await.unwrap().unwrap().source,
        Source::Admin
    );
    assert!(forms.get("contact").await.unwrap().is_some(), "built in");
    assert!(forms.get("nope").await.unwrap().is_none());
    let names: Vec<String> = forms
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    let mut expected = vec!["contact", "feedback", "newsletter", "survey"];
    if cfg!(feature = "media") {
        // The built-in upload form (file_upload component) needs media.
        expected.push("upload");
    }
    assert_eq!(names, expected);
    // Time-trap stamps are signed with a key kept in the database.
    let stamp = forms.stamp("survey");
    assert!(forms.stamp_age("survey", &stamp).unwrap() < 1.0);
    assert!(
        forms.stamp_age("other", &stamp).is_none(),
        "bound to the form"
    );
    assert!(forms.stamp_age("survey", "1.00").is_none());
    let again = Forms::open(
        site.path(),
        Some(t.db.clone()),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    assert!(
        again.stamp_age("survey", &stamp).is_some(),
        "same key after a restart"
    );
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unique_values_are_stored_once() {
    let Some(t) = TestDb::create("unique_values_are_stored_once").await else {
        return;
    };
    migrate::apply(&t.db, &engine_forms::migrations(t.db.backend()))
        .await
        .unwrap();
    let store = FormStore::new(t.db.clone());
    let data = serde_json::json!({ "email": "a@example.test" });
    let first = store
        .insert(
            "newsletter",
            &data,
            Some("a@example.test"),
            None,
            Some("127.0.0.1"),
            "test",
        )
        .await
        .unwrap();
    assert!(first.is_some());
    let dup = store
        .insert(
            "newsletter",
            &data,
            Some("a@example.test"),
            None,
            None,
            "test",
        )
        .await
        .unwrap();
    assert_eq!(dup, None);
    // Other forms and non-unique forms are unaffected.
    assert!(
        store
            .insert("other", &data, Some("a@example.test"), None, None, "")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .insert("other", &data, None, Some(7), None, "")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .insert("other", &data, None, Some(7), None, "")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store.submissions_of_user(7, None, 10).await.unwrap().len(),
        2
    );
    assert_eq!(
        store.submissions_of_user(8, None, 10).await.unwrap().len(),
        0
    );
    store
        .record_event("other", "accepted", "ip:1", None)
        .await
        .unwrap();
    assert_eq!(store.recent_accepted("other", "ip:1", 60).await.unwrap(), 1);
    assert_eq!(store.recent_accepted("other", "ip:2", 60).await.unwrap(), 0);
    t.cleanup().await;
}

/// The keys behind the spool's exactly-once replay: a keyed insert is
/// stored once, a replay claim is taken once and only if its transaction
/// commits, and events still count after keyed inserts.
#[tokio::test(flavor = "multi_thread")]
async fn spool_keys_are_used_once() {
    let Some(t) = TestDb::create("spool_keys_are_used_once").await else {
        return;
    };
    migrate::apply(&t.db, &engine_forms::migrations(t.db.backend()))
        .await
        .unwrap();
    let store = FormStore::new(t.db.clone());
    let data = serde_json::json!({"message": "hello"});
    let key = engine_forms::spool::new_key();
    let first = FormStore::insert_keyed(&t.db, &key, "contact", &data, None, None, None, "ua")
        .await
        .unwrap();
    assert!(first.is_some());
    // The same key again (a replay of a post whose first insert committed).
    let again = FormStore::insert_keyed(&t.db, &key, "contact", &data, None, None, None, "ua")
        .await
        .unwrap();
    assert_eq!(again, None);
    assert_eq!(
        FormStore::submission_by_key(&t.db, &key).await.unwrap(),
        first
    );
    // A claim rolled back is not taken; a committed one is taken once.
    let mut tx = t.db.begin().await.unwrap();
    assert!(FormStore::claim_replay(&mut tx, &key).await.unwrap());
    drop(tx);
    let mut tx = t.db.begin().await.unwrap();
    assert!(FormStore::claim_replay(&mut tx, &key).await.unwrap());
    tx.commit().await.unwrap();
    let mut tx = t.db.begin().await.unwrap();
    assert!(!FormStore::claim_replay(&mut tx, &key).await.unwrap());
    drop(tx);
    for _ in 0..3 {
        store
            .record_event("contact", "accepted", "ip:9", None)
            .await
            .unwrap();
    }
    assert_eq!(
        store.recent_accepted("contact", "ip:9", 60).await.unwrap(),
        3
    );
    t.cleanup().await;
}
