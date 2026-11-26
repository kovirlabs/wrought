+++
title = "The render dial"
description = "Build, publish and request: when each route makes its HTML."
weight = 4
+++

Every route picks one of three render modes. The mix decides whether a site
acts static, hybrid or app-like.

| Mode | When HTML is made | Stored | Typical use | Available |
| --- | --- | --- | --- | --- |
| `build` | At `engine build` | Files on disk | Docs, marketing pages, blogs | 0.1 |
| `publish` | When content changes | Files on disk, swapped atomically | CMS sites, catalogues, news | 0.2, `cms` preset |
| `request` | On every request | Fragment cache in memory (0.4) | Accounts, carts, dashboards, search | 0.3 with `auth` (no cache yet) |

## Setting the mode

From most to least specific:

1. `mode` on a `[[routes]]` entry with the page's path;
2. `mode` in the page's front matter;
3. `[render] default` in `site.toml` (itself `build` by default).

## Modes that are not in this build

`publish` and `request` are real values: the parser accepts them in every
build, so a site can be written for a fuller engine. A binary without the
feature stops with a message such as:

```text
error: route `/account/` uses render mode `request`, which is not in this
build: this engine was compiled with the `static` preset, which only supports
`build`. Set `mode = "build"`, or use a build with the `request` feature
(planned for a later release).
```

## Features and presets

Capabilities are Cargo features, so code that is switched off is not compiled
or shipped. Presets bundle them:

| Preset | Features | Render modes |
| --- | --- | --- |
| `static` (default) | core | `build` |
| `cms` | core, `db-postgres`, `content`, `publish`, `auth`, `roles`, `admin` | `build`, `publish`, `request` |
| `app` | later releases | adds forms, jobs, mail |

`cargo build --release --features cms` builds the `cms` engine. The spec's
`cms` preset also has `workflow` and `media`; they join it as they land.
Features follow the spec's dependency table: `admin` needs `auth` and
`content`, `roles` needs `auth`, `auth` needs a database. A default build compiles no database code at all. `engine --version`
shows what a binary contains.

## What request does

A `request` route is rendered by `engine serve` for each request and never
written to disk (`engine build` skips it). In 0.3 every such request is
rendered in full; the in-memory fragment cache with tag-based eviction
arrives in 0.4.

```toml
[[routes]]
path = "/account/"
template = "account.html"
mode = "request"
requires = "auth"     # signed-out visitors go to the sign-in page
```

Templates of request pages also get `user` (`name`, `email`, `roles`,
`admin`, `csrf`; undefined when signed out) and `request` (`path`, `query`,
`args`). A wildcard path (`/app/*`) is allowed for request routes.

The spec's rule applies to every other route too: a request from a
signed-in visitor, or with a query string, is rendered per request (with
`Cache-Control: private, no-store` when signed in), so personal pages are
never served from a shared file. A route opts out with `always_static =
true`. Behind Caddy, pass such requests to the engine; see
[deployment](/docs/deployment/#accounts-behind-caddy).

## What publish does

A `publish` route is written at `engine build` like a `build` route, and
rewritten whenever the data it showed changes; see
[publishing](/docs/publishing/). If its file is missing, `engine serve`
renders it on request and writes it.

## Cache tags

Every render records the data it read, as tags:

| Tag | Meaning | Recorded when |
| --- | --- | --- |
| `i:<type>:<id>` | one item | a page renders itself, or a template calls `get_page(id)` |
| `c:<type>` | a collection | a section listing renders, or a template calls `section_pages(...)` |
| `s:<name>` | a singleton | every page records `s:site`; templates can call `tag("menu")` |

Database content adds its own: `list_items("article")` records
`c:article`, `get_item("author", 7)` records `i:author:7`, and
`get_singleton("settings")` records `s:settings`.

`engine build -v` prints them per route, and the build writes them to
`.engine-manifest.json` in the output folder. For `publish` routes they also
go into the tag index in the database, which decides which files an edit
rewrites. `request` mode (a later release) will use them to evict cached
fragments.
