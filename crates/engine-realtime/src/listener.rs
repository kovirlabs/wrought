//! The one `LISTEN engine_realtime` connection of an engine process.
//!
//! It exists only while someone in this process subscribes: it connects
//! when the first subscriber arrives and closes a minute after the last one
//! left, so a site without live regions (or with nobody watching) holds no
//! extra connection. Every notification is handed to the hub, which fans
//! it out in memory; subscribers never hold a connection of their own.
//!
//! PostgreSQL delivers every notification to a listening session, in
//! order, but none sent while it was not listening: whenever the session
//! is lost (an outage, the sleeping database's watchdog stopping the
//! server) the hub starts a new epoch and tells its subscribers to fetch
//! their regions again, instead of pretending nothing was missed.

use std::future::Future;
use std::time::Duration;

use engine_db::Db;

use crate::hub::Hub;

/// The listener closes after this long without subscribers.
const LINGER: Duration = Duration::from_secs(60);

/// Listen until `shutdown` resolves.
pub async fn listen(db: Db, hub: Hub, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    // Whether the last session ended in a way that may have lost events
    // for subscribers that are still there.
    let mut lost = false;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = hub.wait_wanted() => {}
        }
        // A sleeping database has nobody sending; it is woken by whatever
        // needs it, not by this.
        if db.health().is_asleep() {
            tokio::select! {
                _ = &mut shutdown => return,
                _ = db.health().wait_up() => {}
            }
            continue;
        }
        let mut l = match connect(&db).await {
            Ok(l) => l,
            Err(e) => {
                if !db.health().is_down() {
                    eprintln!("live: listening for events: {e}; retrying");
                }
                lost = true;
                tokio::select! {
                    _ = &mut shutdown => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                continue;
            }
        };
        hub.set_listening(true);
        if lost {
            hub.gap();
            lost = false;
        }
        let mut tick = tokio::time::interval(LINGER / 4);
        tick.tick().await;
        let mut empty_since: Option<tokio::time::Instant> = None;
        loop {
            if db.health().is_asleep() {
                lost = true;
                break;
            }
            tokio::select! {
                _ = &mut shutdown => {
                    hub.set_listening(false);
                    return;
                }
                m = l.try_recv() => match m {
                    Ok(Some(n)) => hub.remote(n.payload()),
                    // The connection was lost: listen again on a new one,
                    // then tell the subscribers (what was sent meanwhile
                    // is gone).
                    Ok(None) => {
                        lost = true;
                        break;
                    }
                    Err(e) => {
                        if !db.health().is_down() && !db.health().is_asleep() {
                            eprintln!("live: event listener: {e}; reconnecting");
                        }
                        lost = true;
                        break;
                    }
                },
                _ = tick.tick() => {
                    if hub.subscribers() > 0 {
                        empty_since = None;
                    } else if empty_since
                        .get_or_insert_with(tokio::time::Instant::now)
                        .elapsed()
                        >= LINGER
                    {
                        // Nobody to tell: no connection. Ids from before
                        // cannot be resumed (nothing was heard meanwhile).
                        hub.new_epoch();
                        break;
                    }
                }
            }
        }
        hub.set_listening(false);
        drop(l);
        if lost {
            tokio::select! {
                _ = &mut shutdown => return,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
    }
}

async fn connect(db: &Db) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
    let mut l = db.pg_listener().await?;
    l.listen(crate::CHANNEL).await?;
    Ok(l)
}
