//! Hammerwright's `realtime` feature: live regions, parts of a page that
//! refresh themselves when the data under their cache tags changes.
//!
//! * **One source of events: the change feed and cache tags of publish.**
//!   After a publish batch has written its pages and the fragment cache
//!   dropped what it touched, `engine serve` hands the batch's tags to the
//!   [`Hub`] ([`Hub::change`]); so does `Context::evict` of an app crate.
//!   A site's own messages ([`Hub::message`], `Context::notify`) take the
//!   same path.
//! * **Every engine process hears every event, through PostgreSQL**
//!   (`NOTIFY engine_realtime`): one listening connection per process
//!   ([`listener`]), only while it has subscribers, fanned out in memory.
//!   No connection per subscriber, no Redis.
//! * **Server-sent events** (`GET /_engine/live?tags=c:post,i:post:3`):
//!   each subscriber has a buffer bounded in bytes (`[limits]
//!   live_buffer_kb`); a client that falls behind is disconnected rather
//!   than allowed to grow the server's memory, and resumes with
//!   `Last-Event-ID` (what it missed is replayed while the hub still has
//!   it, else it is told to fetch everything again). A comment line every
//!   25 s keeps proxies from closing a quiet stream. At most `[limits]
//!   live_subscribers` streams at once (`engine doctor`).
//! * **Only public tags on the engine's endpoint.** The tags of the site's
//!   content types (`c:<type>`, `i:<type>:<id>`, `s:<singleton>`, and
//!   vocabularies) and those an app crate declares public. Events carry
//!   the tag only (and a message's payload), never content: a region
//!   fetches its new content through the normal page or fragment route,
//!   with that route's own access rules. Any other tag (a site's own
//!   tables, media, anything personal) is subscribed to through a handler
//!   of the site that checks who is asking ([`Hub::stream`]).
//! * **The `live_region` component** ([`LiveExtension`]) marks a region
//!   and loads one small module (one event stream per page for all its
//!   regions): no JavaScript of the site's own.
//!
//! See the documentation's "Real-time" page.

pub mod hub;
pub mod listener;
pub mod stream;
pub mod wire;

use std::sync::Arc;

use anyhow::{Result, bail};
use axum::Router;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine_core::builder::SiteExtension;
use engine_core::tags::CacheTag;
use engine_db::Db;
use include_dir::{Dir, include_dir};

pub use hub::{Hub, Kind, Refused, Stats};
pub use listener::listen;
pub use stream::LiveStream;
pub use wire::{MAX_PAYLOAD, MAX_TAGS, check_tags};

/// The NOTIFY channel between engine processes.
pub const CHANNEL: &str = "engine_realtime";
/// The engine's event stream endpoint.
pub const PATH: &str = "/_engine/live";
/// Tags one subscription may follow.
pub const MAX_SUBSCRIBED: usize = 32;

/// The statements of this crate (for the catalogue test).
pub mod sql {
    pub use crate::wire::{ALL, NOTIFY};
}

impl Hub {
    /// The data under `tags` changed and is published (pages written,
    /// fragments dropped): tell the subscribers here and in every other
    /// engine process. A batch of more than [`MAX_TAGS`] tags is a reset
    /// (every region fetches again) rather than thousands of events.
    pub async fn change(&self, db: &Db, tags: &[String]) -> Result<()> {
        if tags.is_empty() {
            return Ok(());
        }
        if tags.len() > MAX_TAGS {
            return self.reset(db).await;
        }
        let n = wire::encode(&self.0.origin, Kind::Change, tags, None)?;
        self.deliver(Kind::Change, tags, None);
        execute(db, n).await
    }

    /// Everything may have changed (a rebuild, new templates): every
    /// region, in every process, fetches its content again.
    pub async fn reset(&self, db: &Db) -> Result<()> {
        self.gap();
        let n = wire::encode(&self.0.origin, Kind::Reset, &["*".to_string()], None)?;
        execute(db, n).await
    }

    /// A site's own event for `tags`, with an optional JSON payload (at
    /// most [`MAX_PAYLOAD`] bytes) that every subscriber of those tags
    /// receives as it is: never put anything private in a message to a
    /// public tag. Regions following the tags fetch their content again.
    pub async fn message<S: AsRef<str>>(
        &self,
        db: &Db,
        tags: &[S],
        payload: Option<serde_json::Value>,
    ) -> Result<()> {
        let tags = check_tags(tags)?;
        if tags.len() > MAX_TAGS {
            bail!("a live message names at most {MAX_TAGS} tags");
        }
        let n = wire::encode(&self.0.origin, Kind::Message, &tags, payload.as_ref())?;
        self.deliver(Kind::Message, &tags, payload.as_ref());
        execute(db, n).await
    }

    /// A stream of `tags` for a request, without the public-tag rule: for
    /// a site's own handler that has checked the visitor may follow them
    /// (a signed-in user's own `i:inbox:<id>`). The client's last event id
    /// comes from `Last-Event-ID`, else the query's `last=`.
    pub fn stream<S: AsRef<str>>(
        &self,
        tags: &[S],
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Response {
        let tags = match check_tags(tags) {
            Ok(t) if !t.is_empty() && t.len() <= MAX_SUBSCRIBED => t,
            _ => return bad_request(),
        };
        let last = last_event_id(headers, query);
        match self.subscribe(&tags, last.as_deref()) {
            Ok(s) => s.into_response(),
            Err(r) => stream::refused(r),
        }
    }
}

/// Send an event from outside `engine serve` (a job, a command): every
/// serving process hands it to its subscribers of `tags`.
pub async fn notify<S: AsRef<str>>(
    db: &Db,
    tags: &[S],
    payload: Option<serde_json::Value>,
) -> Result<()> {
    let tags = check_tags(tags)?;
    if tags.len() > MAX_TAGS {
        bail!("a live message names at most {MAX_TAGS} tags");
    }
    execute(
        db,
        wire::encode("", Kind::Message, &tags, payload.as_ref())?,
    )
    .await
}

async fn execute(db: &Db, notifications: Vec<String>) -> Result<()> {
    for n in notifications {
        wire::NOTIFY.bind(CHANNEL).bind(n).execute(db).await?;
    }
    Ok(())
}

/// Which tags anyone may follow on the engine's endpoint.
#[derive(Clone)]
pub struct Public(Arc<dyn Fn(&CacheTag) -> bool + Send + Sync>);

impl Public {
    pub fn new(f: impl Fn(&CacheTag) -> bool + Send + Sync + 'static) -> Public {
        Public(Arc::new(f))
    }

    pub fn allows(&self, t: &CacheTag) -> bool {
        (self.0)(t)
    }
}

/// A list of tags an app crate declared public: `c:signups`, or
/// `i:note:*` for every item of a kind.
pub fn pattern_matches(pattern: &str, tag: &CacheTag) -> bool {
    match pattern.strip_suffix(":*") {
        Some(prefix) => {
            matches!(tag, CacheTag::Item { kind, .. } if prefix.strip_prefix("i:") == Some(kind.as_str()))
        }
        None => pattern == tag.to_string(),
    }
}

/// The tags and the client's last event id of a query string
/// (`tags=c:post,i:post:3&last=...`).
pub fn parse_query(q: Option<&str>) -> Result<(Vec<String>, Option<String>)> {
    let mut tags = Vec::new();
    let mut last = None;
    for pair in q.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(&v.replace('+', " "));
        match k {
            "tags" | "tag" => tags.extend(
                v.split([',', ' '])
                    .filter(|t| !t.is_empty())
                    .map(str::to_string),
            ),
            "last" => last = Some(v),
            _ => {}
        }
    }
    if tags.is_empty() || tags.len() > MAX_SUBSCRIBED {
        bail!("between 1 and {MAX_SUBSCRIBED} tags");
    }
    Ok((check_tags(&tags)?, last))
}

/// The client's last event id: the browser's own reconnect sends
/// `Last-Event-ID`; the page's script, after it closed a stream, `last=`.
pub fn last_event_id(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            query?
                .split('&')
                .find_map(|p| p.strip_prefix("last="))
                .map(percent_decode)
        })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(h) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bad_request() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Name between 1 and 32 cache tags: ?tags=c:<type>,i:<type>:<id>\n",
    )
        .into_response()
}

/// `GET /_engine/live`: public tags only (see [`Public`]).
pub fn router(hub: Hub, public: Public) -> Router {
    Router::new().route(
        PATH,
        axum::routing::get(move |q: RawQuery, headers: HeaderMap| {
            let hub = hub.clone();
            let public = public.clone();
            async move {
                let Ok((tags, last)) = parse_query(q.0.as_deref()) else {
                    return bad_request();
                };
                if tags
                    .iter()
                    .any(|t| t.parse::<CacheTag>().map_or(true, |t| !public.allows(&t)))
                {
                    return (
                        StatusCode::FORBIDDEN,
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        "Not a public tag: follow it through one of the site's own handlers.\n",
                    )
                        .into_response();
                }
                let last = headers
                    .get("last-event-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .or(last);
                match hub.subscribe(&tags, last.as_deref()) {
                    Ok(s) => s.into_response(),
                    Err(r) => stream::refused(r),
                }
            }
        }),
    )
}

/// The `live_region` component.
pub static COMPONENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/site-templates");

/// Adds the `live_region` component to the site's templates.
#[derive(Clone, Default)]
pub struct LiveExtension;

impl SiteExtension for LiveExtension {
    fn extend_env(&self, _env: &mut minijinja::Environment<'static>) {}

    fn template_dirs(&self) -> Vec<&'static Dir<'static>> {
        vec![&COMPONENTS]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_name_tags_and_the_last_event() {
        let (t, last) = parse_query(Some("tags=c%3Apost%2Ci%3Apost%3A3&last=ab-4")).unwrap();
        assert_eq!(t, ["c:post", "i:post:3"]);
        assert_eq!(last.as_deref(), Some("ab-4"));
        let (t, _) = parse_query(Some("tags=c:post+c:page")).unwrap();
        assert_eq!(t, ["c:post", "c:page"]);
        assert!(parse_query(None).is_err());
        assert!(parse_query(Some("tags=bogus")).is_err());
        let many: Vec<String> = (0..40).map(|i| format!("i:x:{i}")).collect();
        assert!(parse_query(Some(&format!("tags={}", many.join(",")))).is_err());
    }

    #[test]
    fn declared_patterns() {
        let t = |s: &str| s.parse::<CacheTag>().unwrap();
        assert!(pattern_matches("c:signups", &t("c:signups")));
        assert!(!pattern_matches("c:signups", &t("i:signups:1")));
        assert!(pattern_matches("i:note:*", &t("i:note:7")));
        assert!(!pattern_matches("i:note:*", &t("i:notes:7")));
        assert!(!pattern_matches("i:note:*", &t("c:note")));
    }

    #[test]
    fn the_component_is_embedded() {
        for f in ["live_region.html", "live_region.toml", "live_region.js"] {
            assert!(
                COMPONENTS.get_file(format!("components/{f}")).is_some(),
                "{f}"
            );
        }
    }
}
