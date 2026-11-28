+++
title = "Publishing"
description = "The publish render mode: pages rewritten when content changes, and only those."
weight = 8
+++

A `publish` route is rendered to a file like a `build` route, but it is
rendered again whenever the content it shows changes. Readers get static
files; editors see their change live within a moment. This needs the `cms`
preset and a database (see [content types](/docs/content-types/)).

## The flow

```text
engine content set ...  ─┐
psql UPDATE ...          ─┼─▶ trigger ─▶ engine_changes row + NOTIFY
                          │
engine serve ◀────────────┘  listener wakes up
  ├─ change → cache tags: i:article:3, c:article
  ├─ tag index → paths that recorded those tags
  ├─ re-render each path, write it atomically, record its new tags
  └─ paths that no longer exist (unpublished, deleted, new slug) → file removed
```

1. **Every render records cache tags** (see [the render dial](/docs/render-dial/#cache-tags)):
   an item page records `i:article:3`; `list_items("article")` records
   `c:article`; `get_item("author", 7)` records `i:author:7`;
   `get_singleton("settings")` records `s:settings`.
2. **`engine build` writes the tag index**: for each `publish` page, the tags
   its render recorded, in the `engine_pages` and `engine_page_tags` tables.
3. **A change** to an item gives `i:<type>:<id>` and `c:<type>` (`s:<type>`
   for a singleton); a change to a term gives `i:<vocabulary>:<id>` and
   `c:<vocabulary>`.
4. **Only the pages carrying those tags are re-rendered**, plus the item's own
   page if it is new. Everything else is left alone: editing one article
   rewrites that article's page and the pages that list articles, not the
   other articles, not pages that show nothing from the database, and never
   `build` routes.

## Changes are never lost

Database triggers on every content table write each change to
`engine_changes` and then send `NOTIFY engine_changes`. The listener only
uses the notification as a wake-up: it reads the change log from where it
left off. So edits made while `engine serve` was stopped, or while its
connection dropped, are published when it comes back, and edits made in
`psql` are published like edits made through the engine.

## Files are swapped atomically

Each page is written to a hidden temporary file next to the old one and then
renamed over it, so a web server serving the folder sees the old page or the
new one, never half a page. Pages are not fsynced one by one; after a power
loss, `engine rebuild` regenerates them all.

## Pre-compressed copies

Every text file the engine writes (pages, stylesheets, scripts, SVG, feeds,
sitemaps, JSON, plain text) of at least 512 bytes gets a Brotli copy
(`page.html.br`, quality 5) and a gzip copy (`page.html.gz`, level 6) beside
it, unless the copy would save under 5 %. The front door sends the copy the
browser takes and compresses nothing per request: Caddy with `file_server {
precompressed br gzip }` (the shipped Caddyfiles), the built-in server by
itself.

- **`engine build`** makes them on its render threads, in the staging
  folder, so they arrive with the atomic swap; a file whose bytes equal the
  previous build's keeps its copies (hard-linked, not compressed again).
- **A publish** writes a page's copies, then the page: copies first, then
  renames, the page last. A page that shrinks below the threshold or stops
  compressing loses its copies before the new page goes live; a deleted
  page loses its copies first. A page whose bytes did not change is not
  rewritten at all (its copies, its modification time and the ETag a front
  door derives from it stay as they were).
- **Never stale:** a copy is always written before its page and is never
  newer than it after a complete write. The built-in server serves a copy
  only if it is a regular file (never a symlink) no newer than its page,
  so a write cut short by a crash falls back to the page itself until the
  next write heals it. Copies are never served under their own names.

Headers from the built-in server: `Content-Encoding` for a copy,
`Vary: Accept-Encoding` on every text file, a strong ETag derived from the
page's bytes (the first 128 bits of its SHA-256, with `-br` or `-gz` for a
copy, so each encoding has its own), `304 Not Modified` for a matching
`If-None-Match`, and `Cache-Control: no-cache` for HTML, feeds, sitemaps and
other un-hashed documents. Caddy derives its ETag from the file sent (its
modification time and size, per encoding).

## Render on miss

When `engine serve` is asked for a page whose file is missing and the path
belongs to a `publish` route, it renders the page, writes the file and
answers with it. With Caddy or nginx in front, a missing file is passed to
the engine and is a file from then on. Paths that are not publish routes, and
items that are not published, get the 404 page.

## Commands

```sh
engine build                  # render everything, write the tag index
engine serve                  # serve, listen for changes, render on miss
engine rebuild                # re-render every page in place
engine rebuild --tag c:article --tag s:settings   # only pages with these tags
```

`engine serve` watches `templates/` and `content/`: after an edit it reloads
them and re-renders every page (a failing template keeps the old files and
reports the error). Changes to `site.toml`, `types/` or `migrations/` need a
restart, and it says so.

## Publish targets

```toml
[publish]
target = "disk"   # the default: the site's output folder
```

With the `s3` feature, `target = "s3"` publishes to an S3-compatible bucket
behind a CDN instead: only pages whose bytes changed are uploaded, and
exactly their paths are invalidated after each batch. There is no
render-on-miss then. See [publishing to a bucket](/docs/buckets/).

## Limits in this release

- A change to an item re-renders every page that lists its type (`c:<type>`),
  whether or not that page shows the changed item.
- One `engine serve` process should publish a site at a time.
- `engine dev` re-renders every published page after a template change, as
  `engine serve` does; it runs the publish listener too.
