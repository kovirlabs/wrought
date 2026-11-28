//! `Db::pinned`: a render's queries share one pooled connection (Postgres;
//! skipped without ENGINE_TEST_DATABASE_URL or on SQLite).

use engine_db::testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn pinned_work_uses_one_connection_and_gives_it_back() {
    let Some(t) = TestDb::create_postgres("db_pinned").await else {
        return;
    };
    let db = t.db.clone();
    let rt = tokio::runtime::Handle::current();
    let pid = |db: &engine_db::Db| {
        let db = db.clone();
        async move {
            engine_db::query("SELECT pg_backend_pid()")
                .scalar::<i32>(&db)
                .await
                .unwrap()
        }
    };
    let (same, held_conn, idle_before, idle_after) = tokio::task::spawn_blocking({
        let db = db.clone();
        move || {
            let pids: Vec<i32> = db.pinned(|| (0..6).map(|_| rt.block_on(pid(&db))).collect());
            // `acquire` inside pinned work hands out the same connection.
            let held = db.pinned(|| {
                let a = rt.block_on(pid(&db));
                let mut c = rt.block_on(db.acquire()).unwrap();
                let b = rt.block_on(async {
                    engine_db::query("SELECT pg_backend_pid()")
                        .scalar::<i32>(&mut c)
                        .await
                        .unwrap()
                });
                drop(c);
                let d = rt.block_on(pid(&db));
                (a, b, d)
            });
            let idle_before = db.postgres().unwrap().num_idle();
            // After the work the connection is back in the pool.
            db.pinned(|| rt.block_on(pid(&db)));
            std::thread::sleep(std::time::Duration::from_millis(200));
            let idle_after = db.postgres().unwrap().num_idle();
            (pids, held, idle_before, idle_after)
        }
    })
    .await
    .unwrap();
    assert!(same.windows(2).all(|w| w[0] == w[1]), "{same:?}");
    assert_eq!(held_conn.0, held_conn.1);
    assert_eq!(held_conn.0, held_conn.2);
    assert!(
        idle_after >= 1,
        "returned to the pool: {idle_before} → {idle_after}"
    );
    // A panic inside pinned work leaves no pin behind.
    let db2 = db.clone();
    let r = tokio::task::spawn_blocking(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            db2.pinned(|| panic!("boom"));
        }));
        // Not pinned any more: a query outside works as usual.
        tokio::runtime::Handle::current().block_on(async {
            engine_db::query("SELECT 1")
                .scalar::<i32>(&db2)
                .await
                .unwrap()
        })
    })
    .await
    .unwrap();
    assert_eq!(r, 1);
    t.cleanup().await;
}
