+++
title = "Real-time"
description = "Live regions: parts of a page that refresh themselves when their data changes, over server-sent events fed by the change feed."
weight = 23
+++

The `realtime` feature (in the `app` preset; `--features cms,realtime`
adds it to a cms build) keeps parts of a page up to date while it is open.
A **live region** follows cache tags, the same tags publishing and the
fragment cache use (`c:article`, `i:article:3`, `s:settings`). When
something changes the data under one of them, the page hears about it and
fetches the region again. It needs no JavaScript of your own and no other
store: events travel through the site's PostgreSQL.

## A live region

```jinja
{% set latest %}
<ul>
{% for a in list_items("article", limit=5) %}
  <li><a href="{{ a.url }}">{{ a.title }}</a></li>
{% endfor %}
</ul>
{% endset %}
{{ component("live_region", id="latest", tags=["c:article"], body=latest) }}
```

renders the list as usual inside
`<div id="latest" data-live="c:article">` and loads one small module
(`live_region`, from the site's own origin). The page opens **one** event
stream for all its regions. When an article is saved, published and the
list page written again, the stream says `c:article` changed; the region
fetches the page again (revalidated, so it costs a `200` only when the
page changed) and puts the children of `#latest` in place. A burst of
changes is one fetch. Without JavaScript the region is simply what the page
was rendered with.

| Argument | What it does |
| --- | --- |
| `id` (required) | unique on the page: the new content is the element with this id in the fetched page |
| `tags` (required) | a list of tags, or one string of tags separated by spaces |
| `body` | the region's content as rendered now |
| `src` | fetch this URL instead of the page (a `request` route or a handler that returns the fragment) |
| `endpoint` | stream events from this handler of the site instead of `/_engine/live` (tags that are not public, below) |
| `announce` | `aria-live="polite"`: screen readers announce each update (short status text, not lists) |
| `label` | an accessible name (the region becomes a labelled `region`) |

The region's element also receives a `hammerwright:live` DOM event
(`detail`: `{tag, kind, payload}`) for every event, so a page's own script
can react. `data-live-swap="none"` on the element leaves the update to that
script. With htmx on the page, a region with `hx-get` gets an htmx `live`
trigger instead of the built-in fetch:

```html
<div id="cart" data-live="i:cart:42" hx-get="/cart/summary/" hx-trigger="live">...</div>
```

## Where events come from

- **The change feed.** After every publish batch, once its pages are written
  and the fragment cache has dropped what the batch touched, the batch's
  tags are sent to every subscriber of them. Content saved in the admin,
  the API or `engine content`, and rows of a site's own tables with the
  change-log trigger (see [App crates](/docs/app-crates/#named-queries-in-templates)),
  all arrive this way. A rebuild or a template change tells every region to
  fetch again.
- **An app crate's evictions and messages.** `cx.evict(&["c:signups"])`
  also tells subscribers of those tags; `cx.notify(&["c:board"],
  Some(json!({"moves": 3})))` sends a message with a JSON payload (at most
  4 KiB) that regions and scripts receive as it is. Jobs have
  `cx.notify(...)` too, and anything else with the database can call
  `engine_realtime::notify(&db, tags, payload)`.

Every engine process hears every event: each one sends it with PostgreSQL's
`NOTIFY engine_realtime`, and each process that has subscribers keeps one
`LISTEN` connection and hands events out in memory. Subscribers never
hold a database connection, and nothing else (no Redis) is involved.
Another process's change also drops that process's cached fragments, so a
region refetching there gets the new content.

## Who may follow what

The engine's endpoint, `GET /_engine/live?tags=...`, streams **public tags
only**:

- the tags of the site's content types and vocabularies (`c:article`,
  `i:article:3`, `s:<singleton>`): the content of published pages;
- tags an app crate declares with `App::live_public(&["c:board",
  "i:game:*"])` (`i:<kind>:*` is every item of a kind).

Anything else (a site's own tables, media, anything personal) is refused
with `403`. An event carries only its tag (and a message's payload), never
content: a region fetches its content through the normal page or fragment
route, with that route's own access checks. Still, an event says *that*
something changed and when, and a message's payload goes to everyone
following the tag, so never declare a tag of private data public and never
put private data in a payload to a public tag.

To stream private tags, write a handler that checks the visitor and
chooses the tags itself, and point a region's `endpoint` at it:

```rust
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use engine_cli::app::Context;

// .route("/inbox/live", get(inbox_live))
async fn inbox_live(State(cx): State<Context>, headers: HeaderMap, q: RawQuery) -> Response {
    let Some(s) = cx.session(&headers).await else {
        return StatusCode::FORBIDDEN.into_response();
    };
    // Never the tags the page asked for: this visitor's own.
    let tag = format!("i:inbox:{}", s.user.id);
    cx.live(&[&tag], &headers, q.0.as_deref())
}
```

```jinja
{{ component("live_region", id="inbox", tags=["i:inbox:" ~ user.id], endpoint="/inbox/live", src="/inbox/fragment/", body=inbox) }}
```

## Limits and slow clients

Each subscriber has a buffer bounded in bytes: `[limits] live_buffer_kb`,
by default 1/32768 of memory, 16 to 64 KiB (an event is about 100 bytes).
A client that falls further behind (a stalled connection) is disconnected
rather than allowed to grow the server's memory; its browser reconnects.
At most `[limits] live_subscribers` streams are open at once: by default as
many as fit 1/16 of memory with every buffer full, 256 to 65536. More get
`503` with `Retry-After`, and the page's script tries again later. `engine
doctor` prints both values and the open-file limit (each stream is an open
socket; the engine raises its soft limit to the hard one at start). The
loopback health detail (`/_engine/health`) has a `live` section:
subscribers, limit, dropped, sent, and whether this process is listening.

An idle stream gets a comment line every 25 seconds so proxies keep it
open. The shipped Caddy configuration passes `/_engine/live` straight
through (no compression, no buffering); behind nginx the engine's
`X-Accel-Buffering: no` does the same.

## Reconnecting

Every event has an id. A browser that lost its stream reconnects with
`Last-Event-ID`; the engine replays what it missed while it still holds
it (recent events, sixteen buffers' worth), and otherwise tells the page to
fetch every region again (`reset`). The same happens whenever this process
may have missed other processes' events (its `LISTEN` connection was lost,
the database was asleep). Between a page being rendered and its stream
opening there is a short moment no event covers; a region fetches again on
the next change.

## Idle sites still sleep

An open stream is not use. With the hosting admin's
[idle exit](/docs/deployment/) the engine exits after the quiet period even
while streams are open: it ends each one with a `bye`, and pages reconnect
only when the visitor does something (a key, a click, the tab shown again),
so an open tab does not start the engine again by itself. Likewise the
[sleeping database](/docs/deployment/) watchdog puts PostgreSQL to sleep as
usual; this process's `LISTEN` connection does not count, and it listens
again (with a `reset` to its subscribers) once the database is awake.

For a site that should stay awake while someone watches, the hosting admin
sets `live_keeps_awake = true` (in `/etc/hammerwright/host.toml`, for the
site or in `[defaults]`): then an open stream keeps both the engine and its
database up. It is never read from `site.toml`.

## WebSockets

There are none. Every live update is server to client; a page sends its
changes with ordinary requests (form posts, handlers), which keep CSRF
checks, sessions, the outage path and the request log as they are.
Server-sent events reconnect and resume by themselves, pass through every
proxy, and need no extra protocol or dependency. If a feature appears that
needs many small messages from the browser (collaborative editing), it can
add a WebSocket route on the same hub.
