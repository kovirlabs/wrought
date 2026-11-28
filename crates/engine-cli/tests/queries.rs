//! The query catalogue check: every statement the engine runs on its own
//! tables (each crate's `sql::ALL`) is prepared against a database that has
//! every engine migration applied. A misspelt column, a missing table or a
//! syntax slip fails here, in seconds, instead of at run time. This stands
//! in for sqlx's compile-time `query!` checks (see DECISIONS.md, v0.5): run
//! it with `scripts/check-queries.sh`.
//!
//! Needs every feature (`--all-features`) and ENGINE_TEST_DATABASE_URL.

#![cfg(all(
    feature = "app",
    feature = "db-postgres",
    feature = "media",
    feature = "search",
    feature = "api",
    feature = "oidc"
))]

use engine_db::migrate;
use engine_db::sql::Sql;
use engine_db::testing::TestDb;

fn catalogue() -> Vec<&'static Sql> {
    let mut v: Vec<&'static Sql> = Vec::new();
    for list in [
        engine_db::migrate::sql::ALL,
        engine_content::sql::ALL,
        engine_publish::sql::ALL,
        engine_auth::sql::ALL,
        engine_auth::oidc::sql::ALL,
        engine_admin::workflow::sql::ALL,
        engine_forms::store::sql::ALL,
        engine_jobs::sql::ALL,
        engine_media::sql::ALL,
        engine_search::sql::ALL,
        engine_api::sql::ALL,
    ] {
        v.extend(list.iter().copied());
    }
    #[cfg(feature = "payments")]
    v.extend(engine_payments::sql::ALL.iter().copied());
    v
}

fn all_migrations() -> Vec<engine_db::Migration> {
    let mut all = Vec::new();
    all.extend(engine_content::migrations());
    all.extend(engine_publish::migrations());
    all.extend(engine_auth::migrations());
    all.extend(engine_admin::workflow_migrations());
    all.extend(engine_forms::migrations());
    all.extend(engine_jobs::migrations());
    all.extend(engine_media::migrations());
    all.extend(engine_search::migrations());
    all.extend(engine_api::migrations());
    #[cfg(feature = "payments")]
    all.extend(engine_payments::migrations());
    all
}

#[tokio::test(flavor = "multi_thread")]
async fn every_statement_prepares_against_the_real_schema() {
    let Some(t) = TestDb::create("query_catalogue").await else {
        return;
    };
    migrate::apply(&t.db, &all_migrations()).await.unwrap();
    let list = catalogue();
    let mut names = std::collections::BTreeSet::new();
    let mut failed = Vec::new();
    let mut checked = 0;
    for s in &list {
        assert!(names.insert(s.name), "{} is listed twice", s.name);
        checked += 1;
        if let Err(e) = engine_db::sql::check(&t.db, s.text).await {
            failed.push(format!("{}: {e}", s.name));
        }
    }
    eprintln!(
        "query catalogue: {checked} of {} statements prepared",
        list.len()
    );
    assert!(checked > 100, "only {checked} statements checked");
    assert!(
        failed.is_empty(),
        "{} statement(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
    t.cleanup().await;
}

/// No statement is left in SQLite's dialect (removed in 0.8: one SQL text
/// per statement, PostgreSQL's), and every name says where it lives.
#[test]
fn statements_are_postgresql_only() {
    for s in catalogue() {
        assert!(s.name.starts_with("engine_"), "{}", s.name);
        let lower = s.text.to_lowercase();
        for slip in [
            "json_each(",
            "sqlite_master",
            "strftime(",
            " glob ",
            "begin immediate",
            "autoincrement",
        ] {
            assert!(!lower.contains(slip), "{} uses `{slip}`", s.name);
        }
    }
}
