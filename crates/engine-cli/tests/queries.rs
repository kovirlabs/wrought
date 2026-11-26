//! The query catalogue check: every statement the engine runs on its own
//! tables (each crate's `sql::ALL`) is prepared against a database that has
//! every engine migration applied, on the test backend (Postgres or
//! SQLite). A misspelt column, a missing table or a dialect slip fails here,
//! in seconds, instead of at run time. This stands in for sqlx's
//! compile-time `query!` checks (see DECISIONS.md, v0.5): run it on both
//! backends with `scripts/check-queries.sh`.
//!
//! Needs every feature (`--all-features`) and ENGINE_TEST_DATABASE_URL.

#![cfg(all(
    feature = "app",
    feature = "db-sqlite",
    feature = "db-postgres",
    feature = "media",
    feature = "search",
    feature = "api"
))]

use engine_db::sql::Sql;
use engine_db::testing::TestDb;
use engine_db::{Backend, migrate};

fn catalogue() -> Vec<&'static Sql> {
    let mut v: Vec<&'static Sql> = Vec::new();
    for list in [
        engine_db::migrate::sql::ALL,
        engine_content::sql::ALL,
        engine_publish::sql::ALL,
        engine_auth::sql::ALL,
        engine_admin::workflow::sql::ALL,
        engine_forms::store::sql::ALL,
        engine_jobs::sql::ALL,
        engine_media::sql::ALL,
        engine_search::sql::ALL,
        engine_api::sql::ALL,
    ] {
        v.extend(list.iter().copied());
    }
    v
}

fn all_migrations(backend: Backend) -> Vec<engine_db::Migration> {
    let mut all = Vec::new();
    all.extend(engine_content::migrations(backend));
    all.extend(engine_publish::migrations(backend));
    all.extend(engine_auth::migrations(backend));
    all.extend(engine_admin::workflow_migrations(backend));
    all.extend(engine_forms::migrations(backend));
    all.extend(engine_jobs::migrations(backend));
    all.extend(engine_media::migrations(backend));
    all.extend(engine_search::migrations(backend));
    all.extend(engine_api::migrations(backend));
    all
}

#[tokio::test(flavor = "multi_thread")]
async fn every_statement_prepares_against_the_real_schema() {
    let Some(t) = TestDb::create("query_catalogue").await else {
        return;
    };
    let backend = t.backend();
    migrate::apply(&t.db, &all_migrations(backend))
        .await
        .unwrap();
    let list = catalogue();
    let mut names = std::collections::BTreeSet::new();
    let mut failed = Vec::new();
    let mut checked = 0;
    for s in &list {
        assert!(names.insert(s.name), "{} is listed twice", s.name);
        let Some(text) = s.text(backend) else {
            continue;
        };
        checked += 1;
        if let Err(e) = engine_db::sql::check(&t.db, text).await {
            failed.push(format!("{}: {e}", s.name));
        }
    }
    eprintln!(
        "query catalogue: {checked} of {} statements prepared on {backend}",
        list.len()
    );
    assert!(checked > 100, "only {checked} statements checked");
    assert!(
        failed.is_empty(),
        "{} statement(s) failed on {backend}:\n{}",
        failed.len(),
        failed.join("\n")
    );
    t.cleanup().await;
}

/// Every statement that is not Postgres-only also exists for SQLite, and
/// the Postgres-only ones belong to the Postgres-only features.
#[test]
fn only_postgres_features_are_postgres_only() {
    for s in catalogue() {
        if s.sqlite.is_none() {
            assert!(
                s.name.starts_with("engine_jobs::")
                    || s.name.starts_with("engine_db::migrate::sql::LOCK")
                    || s.name.starts_with("engine_db::migrate::sql::PG_")
                    || s.name.starts_with("engine_search::sql::PG_"),
                "{} has no SQLite text",
                s.name
            );
        }
    }
}
