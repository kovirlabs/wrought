//! The publish listener: waits for change notifications and runs the
//! publisher.
//!
//! Triggers on content tables write each change to `engine_changes` and send
//! `NOTIFY engine_changes`. The listener treats a notification only as a
//! wake-up: it always reads the change log from the publisher's cursor, so
//! notifications missed while disconnected (or coalesced during the short
//! debounce) lose nothing. It also catches up at start, after every
//! reconnect, and every minute as a safety net.

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
    match db {
        Db::Postgres(pool) => {
            let mut listener = sqlx::postgres::PgListener::connect_with(&pool).await?;
            listener.listen(CHANNEL).await?;
            run_batch(&publisher).await;
            let mut tick = tokio::time::interval(SAFETY_POLL);
            tick.tick().await;
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    msg = listener.try_recv() => match msg {
                        Ok(Some(_)) => {
                            // Coalesce a burst of notifications into one batch.
                            while let Ok(Ok(Some(_))) =
                                tokio::time::timeout(DEBOUNCE, listener.try_recv()).await
                            {}
                            run_batch(&publisher).await;
                        }
                        Ok(None) => {
                            eprintln!("publish listener: reconnected to the database, catching up");
                            run_batch(&publisher).await;
                        }
                        Err(e) => {
                            eprintln!("publish listener: {e}; retrying");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    },
                    _ = tick.tick() => run_batch(&publisher).await,
                }
            }
            Ok(())
        }
    }
}
