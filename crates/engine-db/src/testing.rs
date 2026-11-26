//! Throwaway databases for tests.
//!
//! Tests that need Postgres call [`TestDb::create`]. When
//! `ENGINE_TEST_DATABASE_URL` is unset it returns `None` and the test should
//! return early (it is skipped, not failed), so `cargo test` stays green on a
//! machine without a database. When set, each call creates a fresh database
//! named `wrought_test_*` next to the one in the URL; [`TestDb::cleanup`]
//! drops it.
//!
//! ```sh
//! docker compose -f deploy/compose.postgres.yml up -d --wait
//! ENGINE_TEST_DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought \
//!     cargo test --workspace --all-features
//! ```

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;

use crate::{Db, DbConfig};

pub const TEST_URL_ENV: &str = "ENGINE_TEST_DATABASE_URL";

pub struct TestDb {
    pub url: String,
    pub db: Db,
    admin_url: String,
    name: String,
}

/// `url` with its database name replaced.
pub fn with_database(url: &str, name: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("postgres", url));
    let (before_query, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let host = before_query.split('/').next().unwrap_or(before_query);
    let mut out = format!("{scheme}://{host}/{name}");
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

impl TestDb {
    /// A fresh, empty database, or `None` (after printing why) when no test
    /// database is configured.
    pub async fn create(test: &str) -> Option<TestDb> {
        let Ok(admin_url) = std::env::var(TEST_URL_ENV) else {
            eprintln!("skipping {test}: set {TEST_URL_ENV} to run tests that need Postgres");
            return None;
        };
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "wrought_test_{}_{}_{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        );
        let admin = Db::connect(&DbConfig::new(admin_url.clone()))
            .await
            .unwrap_or_else(|e| panic!("{TEST_URL_ENV} is set but unusable: {e:#}"));
        exec(&admin, &format!("CREATE DATABASE {name}"))
            .await
            .expect("creating test database");
        admin.close().await;
        let url = with_database(&admin_url, &name);
        let db = Db::connect(&DbConfig::new(url.clone()))
            .await
            .expect("connecting to test database");
        Some(TestDb {
            url,
            db,
            admin_url,
            name,
        })
    }

    /// Drop the database.
    pub async fn cleanup(self) {
        self.db.close().await;
        if let Ok(admin) = Db::connect(&DbConfig::new(self.admin_url.clone())).await {
            let _ = exec(
                &admin,
                &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name),
            )
            .await;
            admin.close().await;
        }
    }
}

async fn exec(db: &Db, sql: &str) -> Result<()> {
    match db {
        #[cfg(feature = "postgres")]
        Db::Postgres(p) => {
            sqlx::raw_sql(sql).execute(p).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_database_name() {
        assert_eq!(
            with_database("postgres://u:p@h:1/main?sslmode=disable", "t1"),
            "postgres://u:p@h:1/t1?sslmode=disable"
        );
        assert_eq!(with_database("postgres://h", "t"), "postgres://h/t");
    }

    #[tokio::test]
    async fn migrations_apply_once_and_detect_edits() {
        let Some(t) = TestDb::create("migrations_apply_once_and_detect_edits").await else {
            return;
        };
        let m1 = crate::Migration::new(
            "site/0001_a",
            "CREATE TABLE a (x int); INSERT INTO a VALUES (1);",
        );
        let m2 = crate::Migration::new("site/0002_b", "CREATE TABLE b (y int)");
        let ran = crate::migrate::apply(&t.db, std::slice::from_ref(&m1))
            .await
            .unwrap();
        assert_eq!(ran, ["site/0001_a"]);
        let pending = crate::migrate::pending(&t.db, &[m1.clone(), m2.clone()])
            .await
            .unwrap();
        assert_eq!(pending, ["site/0002_b"]);
        let ran = crate::migrate::apply(&t.db, &[m1.clone(), m2.clone()])
            .await
            .unwrap();
        assert_eq!(ran, ["site/0002_b"]);
        assert!(
            crate::migrate::apply(&t.db, &[m1.clone(), m2.clone()])
                .await
                .unwrap()
                .is_empty()
        );
        let edited = crate::Migration::new("site/0001_a", "CREATE TABLE a (x bigint)");
        let err = crate::migrate::apply(&t.db, &[edited]).await.unwrap_err();
        assert!(err.to_string().contains("changed after"), "{err}");
        // A failing migration rolls back and is not recorded.
        let bad = crate::Migration::new("site/0003_bad", "CREATE TABLE c (z int); SELECT nope;");
        assert!(
            crate::migrate::apply(&t.db, std::slice::from_ref(&bad))
                .await
                .is_err()
        );
        assert_eq!(
            crate::migrate::pending(&t.db, &[bad]).await.unwrap(),
            ["site/0003_bad"]
        );
        t.cleanup().await;
    }
}
