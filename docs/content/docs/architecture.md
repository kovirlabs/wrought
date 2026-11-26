+++
title = "Architecture"
description = "How Wrought is put together: the crates, the render dial, the request flow, the data layer, the publish pipeline, how features are wired, and where to extend it."
weight = 50
+++

A map of the code for contributors. The why behind each choice is in
[`DECISIONS.md`](https://github.com/kovirlabs/wrought/blob/main/DECISIONS.md);
this page says what is where and how the parts meet.

## The crates

A Cargo workspace with one crate per feature, so a contributor can work on
one area without touching the core, and a feature that is switched off is
not compiled at all.

| Crate | Feature | What it holds |
| --- | --- | --- |
| `engine-core` | always | config (`site.toml`), routes and render modes, cache tags, templates (minijinja) and the built-in components, Markdown content, build mode, the `Renderer`, the static server, the fragment cache and web helpers (`request`) |
| `engine-db` | `db-postgres`, `db-sqlite` | the `Db` pool (one of two backends, picked by the URL), the statement catalogue (`statements!`, `Query`), migrations, timestamps, throwaway test databases |
| `engine-content` | `content` | content types from `types/*.toml`, blocks, taxonomy, revisions, slugs, optimistic locking, sanitising, `types sync`, seeds, template functions |
| `engine-publish` | `publish` | change log reader, tag index, publisher, `PublishTarget` (disk), render on miss |
| `engine-auth` | `auth`, `roles` | accounts, Argon2id, sessions, CSRF, password reset, rate limits, TOTP, roles and per-field access, Accounts components |
| `engine-admin` | `admin`, `workflow`, `visual-editor` | the server-rendered admin (minijinja + bundled htmx), review and preview, and the visual editor (its Svelte + Tiptap bundle built from `editor/` and embedded) |
| `engine-forms` | `forms` | form definitions, validation, submissions, spam guard, Forms components |
| `engine-jobs` | `jobs` | the Postgres job queue (`SKIP LOCKED`), leases, schedules |
| `engine-mail` | `mail` | SMTP through lettre, mail templates, sending from jobs |
| `engine-media` | `media` | uploads checked by content, metadata stripping, variants, `/media/` |
| `engine-search` | `search` | Postgres `tsvector` or SQLite FTS5, search components |
| `engine-api` | `api` | the JSON API for content types, scoped tokens |
| `engine-s3` | `s3` | the S3-compatible publish target (SigV4), CDN invalidation (CloudFront, Google Cloud CDN), public media copies |
| `engine-payments` | `payments` | provider trait, Stripe and fake providers, webhooks, orders, cart, Commerce components (Postgres only) |
| `engine-cli` | — | the `engine` binary: commands, feature wiring, the server |

Single sign-on (`oidc`) is a module of `engine-auth` behind its `oidc`
feature. Templates are minijinja only; an `askama` compiled-template path
is deferred until there is a library API for site crates (see
DECISIONS.md).

`engine-core` is the root: it depends on no feature crate. Each feature crate
depends on core (and on `engine-db` when it stores data) and plugs into core
through the hooks below.

## The render dial

Every route has a mode (`build`, `publish` or `request`; see
[the render dial](/docs/render-dial/)):

- **build**: rendered by `engine build` into the output folder, which is
  swapped atomically.
- **publish**: rendered by `engine build` and again whenever content it
  shows changes; a missing file is rendered on the first request for it.
- **request**: rendered by `engine serve` for each request; parts a
  template marks with `fragment()` are cached in memory.

Every render records **cache tags** (`i:<type>:<id>`, `c:<type>`,
`s:<singleton>`, `i:media:<id>` ...) in a per-thread collector
(`engine_core::tags`). `publish` stores them in the tag index to know which
files to rewrite; `request` stores them with each cached fragment to know
which to evict. One change feed drives both.

## Request flow

```text
browser ─▶ front web server (Caddy, nginx) ─▶ file on disk?  ── yes ─▶ served
                                                  │ no, or POST, session
                                                  ▼ cookie, query string
                                           engine serve (axum)
                                                  │
              engine routes (/admin/, /account/, /api/v1/, /_engine/, /media/)
                                                  │ else
                                 site route resolved by the Renderer
                                                  │
                      data loaders: template functions (record tags)
                                                  │
                                    minijinja template ─▶ response
                                                  │ publish route
                                                  ▼
                                 file written for the next visitor
```

`engine serve` is also a static file server, so small installs and
development need no front server. Signed-in visitors and requests with a
query string get `request` rendering unless the route sets
`always_static`. Features that are not compiled in add no routes.

## Data layer

- `engine_db::Db` holds a pool for PostgreSQL or SQLite (both may be
  compiled in; the URL picks) and a change signal.
- Every engine query is a named statement in its crate's `sql` module,
  declared with `engine_db::statements!`: one text for both backends, a
  `{ postgres, sqlite }` pair where dialects differ, or `{ postgres }` for
  Postgres-only features. They run through `engine_db::Query` with typed
  arguments. The **catalogue test** (`scripts/check-queries.sh`) prepares
  every statement against fully migrated databases of both backends.
- Queries on per-site content tables are built at run time from validated,
  quoted identifiers with bound values.
- Migrations are embedded per feature and per backend
  (`migrations/postgres/`, `migrations/sqlite/`), then the site's own, in
  one ordered history with checksums.
- Every editable row has a `version`; a save is one transaction with its
  revision row, and a stale version is a conflict, never an overwrite.

## Publish pipeline

1. A write (admin, API, CLI or plain SQL) changes a content table; a trigger
   appends to `engine_changes` (and on Postgres sends `NOTIFY`).
2. The listener wakes up (NOTIFY, the store's signal, a file watch on
   SQLite, or a poll) and reads the log from its cursor.
3. Changed items map to tags; the tag index maps tags to pages.
4. The publisher re-renders those pages and writes them through the
   `PublishTarget` (atomic rename on disk), then calls `finish_batch` with
   the changed paths (where a CDN purge goes).
5. The batch's tags are broadcast: `engine serve` evicts fragments carrying
   them, and `search` re-indexes the items.

The cursor lives in the database, so edits made while nothing was running
are published at the next start.

## Feature wiring

Features are Cargo features of `engine-cli`
(`crates/engine-cli/Cargo.toml`). Each one switches on its crate and an
empty *marker* feature of `engine-core`, so core knows which render modes
and route kinds exist without depending on the feature crates. Chains follow
the spec (`publish` → `content` → a database backend; `admin` → `auth` +
`content`; `mail` → `jobs` → `db-postgres` ...). Presets: `static`
(default), `cms`, `app`.

`engine-cli` wires the compiled features together behind `#[cfg(feature)]`
(`wiring.rs`, `server.rs`, `main.rs`): it builds the site extensions, mounts
each feature's routes and starts its background tasks.
`scripts/check-features.sh` compiles every allowed combination and checks
that the default build contains no database code.

## Extension hooks

- **`SiteExtension`** (`engine_core::builder`): template functions, extra
  template folders (a feature's components), expanding pattern routes such
  as `/articles/{slug}/`, resolving one path. Content, accounts, forms and
  media are extensions; `Extensions` combines them.
- **`PublishTarget`** (`engine_publish`): where published files go (`put`,
  `delete`, `exists`, `finish_batch`).
- **`AccountNotifier`** (`engine_auth`): how account messages are delivered
  (logged, or mailed by `mail`).
- **Forms `Hooks`** (`engine_forms`): receipts and notifications, provided by
  the CLI with `mail`.
- **`Preview`** (`engine_admin::workflow`): renders a draft through the
  site's own templates.
- **`MissHandler`** (`engine_core::serve`): what the static server does with
  a missing file (render on miss).
- **Components**: any built-in `components/<name>.html` can be overridden by
  a site's `templates/components/<name>.html`.

There are no runtime plugins; extending Wrought means a crate and a
feature.

## Security

The baseline: auto-escaping templates (engine-made HTML reaches templates as
safe values, never through `| safe`), CSRF tokens on every form plus an
`Origin` check, `__Host-` Secure, HttpOnly, SameSite cookies, server-side
sessions storing only token hashes, Argon2id, rate limits in the database,
optional TOTP with secrets encrypted at rest, credentials only from
environment variables, uploads checked by content, and no routes for
features that are not compiled in. Dependencies are checked by `cargo audit`
and `cargo deny` in CI; release binaries are signed
([releases](/docs/releases/)). Report problems privately as described in
[`SECURITY.md`](https://github.com/kovirlabs/wrought/blob/main/SECURITY.md).

## Tests

- `cargo test`: the default static engine, no database.
- `scripts/test-sqlite.sh` and `scripts/test-postgres.sh`: every crate with
  every feature on each backend. End-to-end tests start the real `engine`
  binary and drive it over HTTP (`crates/engine-cli/tests/`).
- `scripts/check-queries.sh`, `scripts/check-features.sh`, and in CI also
  `cargo fmt`, clippy, `cargo audit`, `cargo deny` and the accessibility
  checks (`scripts/check-a11y.sh`).
