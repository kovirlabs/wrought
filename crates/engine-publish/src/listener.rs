//! The publish listener: waits for change notifications and runs the
//! publisher.
//!
//! Triggers on content tables write each change to `engine_changes`. The
//! listener treats every signal only as a wake-up: it always reads the
//! change log from the publisher's cursor, so signals missed while it was
//! down (or coalesced during the short debounce) lose nothing. It also
//! catches up at start and on a timer as a safety net.
//!
//! * **Postgres:** the triggers also send `NOTIFY engine_changes`; the
//!   listener `LISTEN`s, catches up after every reconnect, and every minute.
//! * **SQLite** has no `NOTIFY`. Three wake-ups replace it: the store's
//!   in-process signal (`Db::notify_changed`, instant for edits made through
//!   this server: admin, API), a watch on the database file and its WAL
//!   (edits by other processes: `engine content set`, the `sqlite3` shell),
//!   and a cheap poll of the change log every two seconds (file systems
//!   where watching does not work, e.g. some network mounts).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use engine_db::{Backend, Db};

use crate::publisher::Publisher;

/// The NOTIFY channel the content triggers use (Postgres).
pub const CHANNEL: &str = "engine_changes";

/// How long to wait for more notifications before publishing a batch.
const DEBOUNCE: Duration = Duration::from_millis(50);
#[cfg(feature = "postgres")]
const SAFETY_POLL: Duration = Duration::from_secs(60);
/// SQLite: how often to look at the change log without a wake-up.
const SQLITE_POLL: Duration = Duration::from_secs(2);

async fn run_batch(publisher: &Arc<Publisher>) {
    let p = publisher.clone();
    match tokio::task::spawn_blocking(move || p.process_pending()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("publish failed: {e:#}"),
        Err(e) => eprintln!("publish task failed: {e}"),
    }
}

/// Listen until `shutdown` resolves.
pub async fn listen(
    db: Db,
    publisher: Arc<Publisher>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    match db.backend() {
        Backend::Postgres => listen_pg(db, publisher, shutdown).await,
        Backend::Sqlite => listen_sqlite(db, publisher, shutdown).await,
    }
}

#[cfg(feature = "postgres")]
async fn listen_pg(
    db: Db,
    publisher: Arc<Publisher>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let mut listener = db.pg_listener().await?;
    listener.listen(CHANNEL).await?;
    {
        let _awake = db.health().hold_awake();
        run_batch(&publisher).await;
    }
    let mut tick = tokio::time::interval(SAFETY_POLL);
    tick.tick().await;
    tokio::pin!(shutdown);
    loop {
        // A sleeping database has nothing new to publish; it is woken by
        // whatever changes it (an editor's request).
        if db.health().is_asleep() {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = db.health().wait_up() => {}
            }
            continue;
        }
        tokio::select! {
            _ = &mut shutdown => break,
            msg = listener.try_recv() => match msg {
                Ok(Some(_)) => {
                    // Coalesce a burst of notifications into one batch.
                    while let Ok(Ok(Some(_))) =
                        tokio::time::timeout(DEBOUNCE, listener.try_recv()).await
                    {}
                    // No sleep while a publish runs.
                    let _awake = db.health().hold_awake();
                    run_batch(&publisher).await;
                }
                Ok(None) => {
                    eprintln!("publish listener: reconnected to the database, catching up");
                    let _awake = db.health().hold_awake();
                    run_batch(&publisher).await;
                }
                Err(e) => {
                    if !db.health().is_down() {
                        eprintln!("publish listener: {e}; retrying");
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            },
            // The safety poll is background work: a look at the change log
            // that does not count as use; a batch only if there is work.
            _ = tick.tick() => {
                if !db.health().is_down()
                    && engine_db::sleep::quietly(has_pending(&publisher)).await
                {
                    let _awake = db.health().hold_awake();
                    run_batch(&publisher).await;
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(feature = "postgres"))]
async fn listen_pg(_: Db, _: Arc<Publisher>, _: impl Future<Output = ()>) -> Result<()> {
    unreachable!("no Postgres database without the postgres feature")
}

/// Whether the change log has entries past the cursor.
async fn has_pending(publisher: &Publisher) -> bool {
    let idx = publisher.index();
    match (idx.latest_change().await, idx.cursor().await) {
        (Ok(latest), Ok(cursor)) => latest > cursor,
        // Let the batch report the error.
        _ => true,
    }
}

async fn listen_sqlite(
    db: Db,
    publisher: Arc<Publisher>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let mut changes = db.subscribe_changes();
    let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<()>(16);
    // Keep the watcher alive for the loop's lifetime.
    let _watcher = db.sqlite_path().and_then(|path| {
        use notify::Watcher as _;
        let dir = path.parent()?.to_path_buf();
        let name = path.file_name()?.to_string_lossy().to_string();
        let wanted = [name.clone(), format!("{name}-wal")];
        let mut w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            let Ok(ev) = ev else { return };
            if ev.kind.is_access() {
                return;
            }
            if ev.paths.iter().any(|p| {
                p.file_name()
                    .is_some_and(|f| wanted.iter().any(|w| f == w.as_str()))
            }) {
                let _ = file_tx.try_send(());
            }
        })
        .map_err(|e| eprintln!("publish listener: cannot watch {}: {e}", dir.display()))
        .ok()?;
        w.watch(&dir, notify::RecursiveMode::NonRecursive)
            .map_err(|e| eprintln!("publish listener: cannot watch {}: {e}", dir.display()))
            .ok()?;
        Some(w)
    });
    run_batch(&publisher).await;
    let mut tick = tokio::time::interval(SQLITE_POLL);
    tick.tick().await;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            r = changes.changed() => {
                if r.is_err() {
                    break;
                }
                tokio::time::sleep(DEBOUNCE).await;
                changes.borrow_and_update();
                run_batch(&publisher).await;
            }
            Some(()) = file_rx.recv() => {
                tokio::time::sleep(DEBOUNCE).await;
                while file_rx.try_recv().is_ok() {}
                if has_pending(&publisher).await {
                    run_batch(&publisher).await;
                }
            }
            _ = tick.tick() => {
                if has_pending(&publisher).await {
                    run_batch(&publisher).await;
                }
            }
        }
    }
    Ok(())
}
