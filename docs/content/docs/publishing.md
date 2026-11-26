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

Templates and Markdown files are read when `engine serve` starts. After
changing them, run `engine build` (or `engine rebuild`) and restart
`engine serve`.

## Publish targets

```toml
[publish]
target = "disk"   # the default: the site's output folder
```

The S3-compatible bucket target with CDN invalidation from the deployment
spec is planned; the publisher already reports each batch's changed paths to
its target, which is where that invalidation will happen.

## Limits in this release

- Listings of database items are not paginated yet (`list_items` takes
  `limit` and `offset`; a route per listing page comes later).
- A change to an item re-renders every page that lists its type (`c:<type>`),
  whether or not that page shows the changed item.
- One `engine serve` process should publish a site at a time.
- `engine dev` rebuilds the whole site on file changes; it does not run the
  publish listener.
