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
| `request` | On every request | Fragment cache in memory | Accounts, carts, dashboards, search | 0.4, with `auth` or `forms` (`cms`, `app` presets) |

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
build: this engine was compiled with the `static` preset, which supports:
build. Set `mode = "build"`, or use a build with the `request` feature.
```

## Features and presets

Capabilities are Cargo features, so code that is switched off is not compiled
or shipped. Presets bundle them:

| Preset | Features | Render modes |
| --- | --- | --- |
| `static` (default) | core | `build` |
| `cms` | core, `db-postgres`, `content`, `publish`, `auth`, `roles`, `admin`, `workflow`, `media`, `search` | `build`, `publish`, `request` |
| `app` | `cms` plus `forms`, `jobs`, `mail`, `api` | `build`, `publish`, `request` |

`cargo build --release --features app` builds the `app` engine (`--features
cms` the `cms` one). Features follow the spec's dependency table: `admin`
needs `auth` and `content`, `workflow` needs `admin`, `roles` needs `auth`,
`auth`, `content`, `forms` and `media` need the database (they switch on
`db-postgres`), `search` needs `content`, `api` needs `auth` and `content`,
`jobs` needs PostgreSQL, `mail` needs `jobs`. A default build compiles no database code
at all. `engine --version` shows what a binary contains.

## What request does

A `request` route is rendered by `engine serve` for each request and never
written to disk (`engine build` skips it). Parts of it can come from the
[fragment cache](#the-fragment-cache).

```toml
[[routes]]
path = "/account/"
template = "account.html"
mode = "request"
requires = "auth"     # signed-out visitors go to the sign-in page
```

Templates of request pages also get `user` (`id`, `name`, `full_name`,
`email`, `roles`, `admin`, `csrf`; undefined when signed out), `csrf` (the
visitor's form token, also for signed-out visitors) and `request` (`path`,
`query`, `args`). A wildcard path (`/app/*`) is allowed for request routes.

The spec's rule applies to every other route too: a request from a
signed-in visitor, or with a query string, is rendered per request (with
`Cache-Control: private, no-store` when signed in), so personal pages are
never served from a shared file. A route opts out with `always_static =
true`. Behind Caddy, pass such requests to the engine; see
[deployment](/docs/deployment/#accounts-behind-caddy).

## The fragment cache

A request page is rendered on every request, but the parts that are the
same for many requests need not be. Mark them with `fragment()`:

```jinja
{{ fragment("components/latest_updates.html", key="latest-updates", limit=3) }}
{{ fragment("components/my_requests.html", per_user=true) }}
```

`fragment(template, key=None, per_user=false, **args)` renders `template`
with `args` (plus `site`, `page`, `ui`, `engine` and `route`) and keeps the
HTML in memory with the cache tags its render recorded. The next request
with the same template, `key` (without one, the page's address), args and
scope gets the stored copy.

Personal data never crosses accounts:

- A **shared** fragment (the default) does not get `user`, `request` or any
  form token at all, so it cannot show anything personal even by mistake.
  One copy serves everyone.
- A `per_user=true` fragment gets `user` and is kept once per account (by
  id). Forms carry a per-session token, so they do not belong in fragments.

When content changes, the same notification and tags that drive `publish`
drop every fragment that showed it: editing an update drops each fragment
that called `list_items("update")` (`c:update`). A form submission drops
the sender's `my_submissions()` fragments. Fragments also expire after
`[cache] fragment_ttl_seconds` (600), the cache stays within `[cache]
max_mb` (unset: 1/64 of memory, 4 to 256 MiB), and editing a template starts afresh. Outside request
renders (`engine build`, publishing, draft previews) `fragment()` just
renders: files on disk are never built from cached parts.

Each request page says how many fragments it reused:

```text
$ curl -sI http://127.0.0.1:4400/status/ | grep -i fragments
x-hammerwright-fragments: hits=2 misses=0
```

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
rewrites. `request` mode uses them to evict cached fragments.
