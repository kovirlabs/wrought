//! The publish listener: waits for change notifications and runs the
//! publisher.
//!
//! Triggers on content tables write each change to `engine_changes`. The
//! listener treats every signal only as a wake-up: it always reads the
//! change log from the publisher's cursor, so signals missed while it was
//! down (or coalesced during the short debounce) lose nothing. It also
//! catches up at start and on a timer as a safety net.
//!
//! The triggers also send `NOTIFY engine_changes`; the listener `LISTEN`s,
//! catches up after every reconnect, and every minute.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use engine_db::Db;

use crate::publisher::Publisher;

/// The NOTIFY channel the content triggers use.
pub const CHANNEL: &str = "engine_changes";

/// How long to wait for more notifications before publishing a batch.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// How often to look at the change log without a notification.
const SAFETY_POLL: Duration = Duration::from_secs(60);

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

/// Whether the change log has entries past the cursor.
async fn has_pending(publisher: &Publisher) -> bool {
    let idx = publisher.index();
    match (idx.latest_change().await, idx.cursor().await) {
        (Ok(latest), Ok(cursor)) => latest > cursor,
        // Let the batch report the error.
        _ => true,
    }
}
