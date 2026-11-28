//! The in-memory fan-out: subscribers by cache tag, each with a buffer
//! bounded in bytes, and a short history of recent events for clients that
//! reconnect with `Last-Event-ID`.
//!
//! Every event is formatted once, as the bytes of one server-sent event,
//! and shared by every subscriber that follows its tag (`Bytes` is a
//! reference count, not a copy). One lock guards the subscriptions, the
//! history and the event counter: events are rare next to requests, and
//! holding it while an event is handed out makes "subscribe and replay
//! what was missed" and "publish to the current subscribers" exclusive, so
//! no event is lost or sent twice to a subscriber that arrives meanwhile.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};

use crate::stream::LiveStream;

/// What an event says happened to its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The data under the tag changed (a publish, an eviction): a live
    /// region fetches its content again.
    Change,
    /// A site's own message to the tag ([`crate::Hub::message`]), with
    /// an optional JSON payload.
    Message,
    /// Too much changed to name (a rebuild, a very large batch): every
    /// region fetches its content again.
    Reset,
}

/// Drops cached fragments for tags another engine process changed.
pub type Evict = Arc<dyn Fn(&[String]) + Send + Sync>;

/// Why a subscription was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The subscriber limit (`[limits] live_subscribers`) is reached.
    Full,
    /// The server is stopping.
    Closing,
}

/// Counters for the health detail and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub subscribers: usize,
    pub limit: usize,
    pub buffer_bytes: usize,
    /// Subscribers disconnected because they fell behind.
    pub dropped: u64,
    /// Events handed to subscribers.
    pub sent: u64,
    /// Whether this process listens to the other processes' events now.
    pub listening: bool,
}

/// The fan-out of one engine process. Cheap to clone.
#[derive(Clone)]
pub struct Hub(pub(crate) Arc<Inner>);

pub(crate) struct Inner {
    /// This process, in the messages it sends (so it skips its own).
    pub(crate) origin: String,
    limit: usize,
    buffer: usize,
    history_bytes: usize,
    state: Mutex<State>,
    /// Mirrors the subscription count for reads without the lock.
    count: AtomicUsize,
    dropped: AtomicU64,
    sent: AtomicU64,
    closing: AtomicBool,
    listening: AtomicBool,
    /// The first subscriber arrived (the listener connects).
    pub(crate) wanted: Notify,
    evict: OnceLock<Evict>,
}

struct State {
    /// Changes whenever events may have been missed (a new listener
    /// session); ids of another epoch cannot be resumed.
    epoch: u64,
    seq: u64,
    /// Every event after this one is in `history`.
    floor: u64,
    history: VecDeque<(u64, Arc<str>, Bytes)>,
    history_used: usize,
    by_tag: HashMap<Arc<str>, HashMap<u64, Arc<Sub>>>,
    next_sub: u64,
}

/// One open stream.
pub(crate) struct Sub {
    id: u64,
    tags: Vec<Arc<str>>,
    /// `None` once dropped or closed: the stream ends after what is queued.
    tx: Mutex<Option<mpsc::UnboundedSender<Bytes>>>,
    /// Bytes queued and not yet taken by the stream.
    pub(crate) queued: AtomicUsize,
}

enum Pushed {
    Queued,
    /// Over its buffer: disconnected now.
    Dropped,
    /// Already disconnected.
    Gone,
}

impl Sub {
    fn push(&self, frame: Bytes, cap: usize) -> Pushed {
        let mut tx = lock(&self.tx);
        let Some(t) = tx.as_ref() else {
            return Pushed::Gone;
        };
        let len = frame.len();
        if self.queued.fetch_add(len, Ordering::AcqRel) + len > cap {
            *tx = None;
            return Pushed::Dropped;
        }
        if t.send(frame).is_err() {
            *tx = None;
            return Pushed::Gone;
        }
        Pushed::Queued
    }

    /// Queue a last frame whatever the buffer holds, then end the stream.
    fn finish(&self, frame: Bytes) {
        let mut tx = lock(&self.tx);
        if let Some(t) = tx.take() {
            self.queued.fetch_add(frame.len(), Ordering::AcqRel);
            let _ = t.send(frame);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A number no earlier process or listener session used (std's per-process
/// random hash keys, mixed with the time).
fn fresh_epoch() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.write_u32(std::process::id());
    // Short ids: 48 bits are plenty to tell sessions apart.
    h.finish() & 0xffff_ffff_ffff
}

fn event_id(epoch: u64, seq: u64) -> String {
    format!("{epoch:x}-{seq}")
}

fn parse_id(id: &str) -> Option<(u64, u64)> {
    let (e, s) = id.trim().split_once('-')?;
    Some((u64::from_str_radix(e, 16).ok()?, s.parse().ok()?))
}

fn frame(id: &str, event: &str, data: &str) -> Bytes {
    Bytes::from(format!("id: {id}\nevent: {event}\ndata: {data}\n\n"))
}

/// How long the client waits before reconnecting by itself.
const RETRY: &[u8] = b"retry: 3000\n\n";

/// The server tells an idle client to close: no reconnect until the
/// visitor does something (`idle`), or after a short pause (a restart).
fn bye(idle: bool) -> Bytes {
    Bytes::from(format!("event: bye\ndata: {{\"idle\":{idle}}}\n\n"))
}

impl Hub {
    /// A hub for at most `limit` subscribers, each with `buffer` bytes.
    pub fn new(limit: usize, buffer: usize) -> Hub {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        Hub(Arc::new(Inner {
            origin: format!("{}-{:x}", std::process::id(), h.finish()),
            limit: limit.max(1),
            buffer: buffer.max(256),
            // Enough recent events for a reconnect after a short drop.
            history_bytes: buffer.max(256) * 16,
            state: Mutex::new(State {
                epoch: fresh_epoch(),
                seq: 0,
                floor: 0,
                history: VecDeque::new(),
                history_used: 0,
                by_tag: HashMap::new(),
                next_sub: 0,
            }),
            count: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            listening: AtomicBool::new(false),
            wanted: Notify::new(),
            evict: OnceLock::new(),
        }))
    }

    /// A hub sized by the machine's limits (`engine_core::limits`).
    pub fn from_limits() -> Hub {
        let l = engine_core::limits::current();
        Hub::new(l.live_subscribers, l.live_buffer_bytes)
    }

    /// Drop cached fragments of tags that other processes changed.
    pub fn set_evict(&self, evict: Evict) {
        let _ = self.0.evict.set(evict);
    }

    /// Open streams now.
    pub fn subscribers(&self) -> usize {
        self.0.count.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> Stats {
        Stats {
            subscribers: self.subscribers(),
            limit: self.0.limit,
            buffer_bytes: self.0.buffer,
            dropped: self.0.dropped.load(Ordering::Relaxed),
            sent: self.0.sent.load(Ordering::Relaxed),
            listening: self.0.listening.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn set_listening(&self, on: bool) {
        self.0.listening.store(on, Ordering::Relaxed);
    }

    /// Follow `tags` (already checked by the caller). `last` is the
    /// client's `Last-Event-ID`: what it missed since is sent first when
    /// it is still known, else a `reset` (fetch everything again).
    pub fn subscribe(&self, tags: &[String], last: Option<&str>) -> Result<LiveStream, Refused> {
        if self.0.closing.load(Ordering::Acquire) {
            return Err(Refused::Closing);
        }
        let mut st = lock(&self.0.state);
        if self.0.count.load(Ordering::Acquire) >= self.0.limit {
            return Err(Refused::Full);
        }
        let mut seen = HashSet::new();
        let tags: Vec<Arc<str>> = tags
            .iter()
            .filter(|t| seen.insert(t.as_str()))
            .map(|t| Arc::<str>::from(t.as_str()))
            .collect();
        let (tx, rx) = mpsc::unbounded_channel();
        st.next_sub += 1;
        let sub = Arc::new(Sub {
            id: st.next_sub,
            tags,
            tx: Mutex::new(Some(tx)),
            queued: AtomicUsize::new(0),
        });
        let _ = sub.push(Bytes::from_static(RETRY), usize::MAX);
        if let Some(last) = last.filter(|l| !l.trim().is_empty()) {
            let resumable = parse_id(last)
                .filter(|&(e, s)| e == st.epoch && s >= st.floor && s <= st.seq)
                .map(|(_, s)| s);
            let mut replay = Vec::new();
            let mut bytes = 0;
            if let Some(after) = resumable {
                for (s, tag, f) in &st.history {
                    if *s > after && sub.tags.contains(tag) {
                        bytes += f.len();
                        replay.push(f.clone());
                    }
                }
            }
            if resumable.is_none() || bytes > self.0.buffer {
                let id = event_id(st.epoch, st.seq);
                let _ = sub.push(frame(&id, "reset", "{}"), usize::MAX);
            } else {
                for f in replay {
                    let _ = sub.push(f, usize::MAX);
                }
            }
        }
        for t in &sub.tags {
            st.by_tag
                .entry(t.clone())
                .or_default()
                .insert(sub.id, sub.clone());
        }
        let n = self.0.count.fetch_add(1, Ordering::AcqRel) + 1;
        drop(st);
        if n == 1 {
            self.0.wanted.notify_one();
        }
        Ok(LiveStream::new(rx, sub, self.clone()))
    }

    pub(crate) fn unregister(&self, sub: &Sub) {
        let mut st = lock(&self.0.state);
        let mut found = false;
        for t in &sub.tags {
            if let Some(m) = st.by_tag.get_mut(t) {
                found |= m.remove(&sub.id).is_some();
                if m.is_empty() {
                    st.by_tag.remove(t);
                }
            }
        }
        if found {
            self.0.count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Hand an event for each of `tags` to their subscribers in this
    /// process (nothing else: [`Hub::change`] and [`Hub::message`] also
    /// tell the other processes).
    pub fn deliver(&self, kind: Kind, tags: &[String], payload: Option<&serde_json::Value>) {
        if tags.is_empty() || self.0.closing.load(Ordering::Acquire) {
            return;
        }
        if kind == Kind::Reset {
            self.gap();
            return;
        }
        let mut st = lock(&self.0.state);
        let mut seen = HashSet::new();
        for tag in tags.iter().filter(|t| seen.insert(t.as_str())) {
            st.seq += 1;
            if self.0.count.load(Ordering::Acquire) == 0 {
                // Nobody to replay to later either: keep no history.
                st.floor = st.seq;
                st.history.clear();
                st.history_used = 0;
                continue;
            }
            let id = event_id(st.epoch, st.seq);
            let data = match payload {
                Some(p) => serde_json::json!({"tag": tag, "kind": kind, "payload": p}),
                None => serde_json::json!({"tag": tag, "kind": kind}),
            };
            let f = frame(&id, "live", &data.to_string());
            let seq = st.seq;
            let key: Arc<str> = st
                .by_tag
                .get_key_value(tag.as_str())
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| Arc::from(tag.as_str()));
            st.history_used += f.len();
            st.history.push_back((seq, key, f.clone()));
            while st.history_used > self.0.history_bytes {
                let Some((s, _, old)) = st.history.pop_front() else {
                    break;
                };
                st.history_used -= old.len();
                st.floor = s;
            }
            if let Some(subs) = st.by_tag.get(tag.as_str()) {
                for s in subs.values() {
                    match s.push(f.clone(), self.0.buffer) {
                        Pushed::Queued => {
                            self.0.sent.fetch_add(1, Ordering::Relaxed);
                        }
                        Pushed::Dropped => {
                            self.0.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Pushed::Gone => {}
                    }
                }
            }
        }
    }

    /// Events may have been missed (the listener reconnected; a rebuild
    /// changed everything): start a new epoch and tell every subscriber to
    /// fetch its regions again.
    pub fn gap(&self) {
        let mut st = lock(&self.0.state);
        Self::new_epoch_locked(&mut st);
        let id = event_id(st.epoch, st.seq);
        let f = frame(&id, "reset", "{}");
        for s in unique(&st) {
            if let Pushed::Dropped = s.push(f.clone(), self.0.buffer) {
                self.0.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// No listener for a while: ids of before cannot be resumed (events of
    /// other processes went unheard), but nobody needs telling.
    pub(crate) fn new_epoch(&self) {
        Self::new_epoch_locked(&mut lock(&self.0.state));
    }

    fn new_epoch_locked(st: &mut State) {
        st.epoch = fresh_epoch();
        st.floor = st.seq;
        st.history.clear();
        st.history_used = 0;
    }

    /// The server stops: every stream gets a `bye` and ends, so the
    /// graceful shutdown does not wait for them. `idle`: the engine exits
    /// for being idle (clients reconnect only when the visitor does
    /// something, so they do not start it again for nothing).
    pub fn close(&self, idle: bool) {
        self.0.closing.store(true, Ordering::Release);
        let st = lock(&self.0.state);
        let f = bye(idle);
        for s in unique(&st) {
            s.finish(f.clone());
        }
    }

    /// A message from another process (the `engine_realtime` channel).
    pub(crate) fn remote(&self, payload: &str) {
        let Ok(w) = serde_json::from_str::<crate::wire::Wire>(payload) else {
            eprintln!(
                "live: a message on {} that is not one: ignored",
                crate::CHANNEL
            );
            return;
        };
        if w.o == self.0.origin {
            return;
        }
        if w.k != Kind::Message
            && let Some(e) = self.0.evict.get()
        {
            e(&w.t);
        }
        if w.k == Kind::Reset {
            self.gap();
        } else {
            self.deliver(w.k, &w.t, w.p.as_ref());
        }
    }

    /// Wait until someone subscribes.
    pub(crate) async fn wait_wanted(&self) {
        loop {
            let n = self.0.wanted.notified();
            if self.subscribers() > 0 {
                return;
            }
            n.await;
        }
    }
}

fn unique(st: &State) -> Vec<Arc<Sub>> {
    let mut seen = HashSet::new();
    st.by_tag
        .values()
        .flat_map(|m| m.values())
        .filter(|s| seen.insert(s.id))
        .cloned()
        .collect()
}

/// How often an idle stream gets a comment line, so proxies and the
/// browser keep it open (most close a silent connection after 60 s).
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(25);

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_core::Stream;

    fn tags(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    /// Everything the stream has ready now, as text.
    fn drain(s: &mut LiveStream) -> String {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut out = String::new();
        while let Poll::Ready(Some(Ok(b))) = Pin::new(&mut *s).poll_next(&mut cx) {
            out.push_str(std::str::from_utf8(&b).unwrap());
        }
        out
    }

    fn ids(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn events_reach_the_subscribers_of_their_tags_only() {
        let hub = Hub::new(10, 4096);
        let mut a = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        let mut b = hub.subscribe(&tags(&["i:post:3", "c:page"]), None).unwrap();
        assert_eq!(hub.subscribers(), 2);
        hub.deliver(Kind::Change, &tags(&["c:post", "i:post:3"]), None);
        let ta = drain(&mut a);
        let tb = drain(&mut b);
        assert!(ta.starts_with("retry: 3000\n\n"), "{ta}");
        assert!(
            ta.contains(r#"data: {"kind":"change","tag":"c:post"}"#),
            "{ta}"
        );
        assert!(!ta.contains("i:post:3"), "{ta}");
        assert!(tb.contains(r#""tag":"i:post:3""#), "{tb}");
        assert!(!tb.contains("c:post\""), "{tb}");
        // Payloads travel with messages.
        hub.deliver(
            Kind::Message,
            &tags(&["c:page"]),
            Some(&serde_json::json!({"n": 1})),
        );
        assert!(drain(&mut b).contains(r#""payload":{"n":1}"#));
        drop(a);
        assert_eq!(hub.subscribers(), 1);
        drop(b);
        assert_eq!(hub.subscribers(), 0);
        assert!(lock(&hub.0.state).by_tag.is_empty());
    }

    #[tokio::test]
    async fn a_reconnect_gets_what_it_missed_or_a_reset() {
        let hub = Hub::new(10, 4096);
        let mut a = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        hub.deliver(Kind::Change, &tags(&["c:post"]), None);
        let first = ids(&drain(&mut a));
        assert_eq!(first.len(), 1);
        // Keep one subscriber so history is kept while `a` is away.
        let _other = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        drop(a);
        hub.deliver(Kind::Change, &tags(&["c:post", "c:page"]), None);
        hub.deliver(Kind::Change, &tags(&["c:post"]), None);
        let mut back = hub.subscribe(&tags(&["c:post"]), Some(&first[0])).unwrap();
        let t = drain(&mut back);
        assert_eq!(ids(&t).len(), 2, "both c:post events, not c:page: {t}");
        assert!(!t.contains("reset"), "{t}");
        // An id of another epoch (another process, a listener gap): reset.
        let mut old = hub.subscribe(&tags(&["c:post"]), Some("abc-1")).unwrap();
        assert!(drain(&mut old).contains("event: reset"));
        // After a gap the old ids are no longer resumable.
        hub.gap();
        let t = drain(&mut back);
        assert!(t.contains("event: reset"), "{t}");
        let mut late = hub.subscribe(&tags(&["c:post"]), Some(&first[0])).unwrap();
        assert!(drain(&mut late).contains("event: reset"));
    }

    #[tokio::test]
    async fn a_slow_subscriber_is_dropped_not_buffered() {
        let hub = Hub::new(10, 1024);
        let mut slow = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        let mut fast = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        for _ in 0..100 {
            hub.deliver(Kind::Change, &tags(&["c:post"]), None);
            drain(&mut fast);
        }
        // About 1 KiB queued for `slow`, then it was cut off: what it had
        // queued is still sent, then its stream ends.
        let t = drain(&mut slow);
        assert!(t.len() <= 1024 + 64, "{}", t.len());
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(
            Pin::new(&mut slow).poll_next(&mut cx),
            Poll::Ready(None)
        ));
        assert_eq!(hub.stats().dropped, 1);
        assert!(lock(&slow.sub.tx).is_none());
        drop(slow);
        assert_eq!(hub.subscribers(), 1);
    }

    #[tokio::test]
    async fn the_limit_and_closing_refuse_new_subscribers() {
        let hub = Hub::new(2, 4096);
        let _a = hub.subscribe(&tags(&["c:x"]), None).unwrap();
        let mut b = hub.subscribe(&tags(&["c:x"]), None).unwrap();
        assert_eq!(
            hub.subscribe(&tags(&["c:x"]), None).err(),
            Some(Refused::Full)
        );
        hub.close(true);
        let t = drain(&mut b);
        assert!(t.ends_with("event: bye\ndata: {\"idle\":true}\n\n"), "{t}");
        assert_eq!(
            hub.subscribe(&tags(&["c:x"]), None).err(),
            Some(Refused::Closing)
        );
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(parse_id(&event_id(0xabc, 7)), Some((0xabc, 7)));
        assert_eq!(parse_id("nonsense"), None);
    }

    #[tokio::test]
    async fn remote_messages_of_this_process_are_skipped() {
        let hub = Hub::new(10, 4096);
        let evicted = Arc::new(Mutex::new(Vec::<String>::new()));
        let e = evicted.clone();
        hub.set_evict(Arc::new(move |t: &[String]| lock(&e).extend_from_slice(t)));
        let mut s = hub.subscribe(&tags(&["c:post"]), None).unwrap();
        let own = crate::wire::encode(&hub.0.origin, Kind::Change, &tags(&["c:post"]), None)
            .unwrap()
            .remove(0);
        hub.remote(&own);
        assert!(!drain(&mut s).contains("c:post\""));
        let other = crate::wire::encode("other", Kind::Change, &tags(&["c:post"]), None)
            .unwrap()
            .remove(0);
        hub.remote(&other);
        assert!(drain(&mut s).contains(r#""tag":"c:post""#));
        assert_eq!(*lock(&evicted), ["c:post"]);
    }
}
