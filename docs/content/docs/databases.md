+++
title = "Databases: SQLite and PostgreSQL"
description = "Choose a backend per site with its URL; what differs, how changes reach the publisher, and how the engine's queries are checked."
weight = 15
+++

Every feature that the spec lists as needing only a database works on both
**SQLite** and **PostgreSQL**: content, publishing, accounts, roles, the
admin, review, forms, media, search and the JSON API. The background `jobs`
queue and `mail` (sent through it) stay on PostgreSQL, as the spec says.

The `cms` and `app` presets compile both backends; the database URL picks
one at run time:

```toml
[database]
url = "sqlite://data/site.db"       # a file, relative to the site folder
# url = "postgres://engine@db.internal/site"
max_connections = 5
```

`ENGINE_DATABASE_URL` overrides `url` (keep passwords out of the site
folder). A SQLite file is created on first use; `sqlite:///var/lib/site.db`
is an absolute path.

## Which one

| | SQLite | PostgreSQL |
| --- | --- | --- |
| Setup | none: a file next to the site | a server (or a managed one) |
| Editors at once | one writer at a time (others wait milliseconds) | many |
| Publishing on change | in-process wake-up, file watch, 2 s poll | `LISTEN/NOTIFY` |
| Full-text search | FTS5, Porter stemming (English) | `tsvector`, any text search configuration |
| Jobs and mail | not available | yes |
| Backups | copy the file (with `sqlite3 .backup`) | `pg_dump` |

SQLite suits single-editor sites, small teams, a Raspberry Pi, and trying
things out; PostgreSQL suits several editors, background jobs and email.

An `app` engine pointed at SQLite turns jobs and mail off and logs account
messages instead. A site whose configuration *asks* for them (`[jobs]` or
`[mail]` in `site.toml`, or a form with `[receipt]` or `[notify]`) refuses to
start with a message saying they need PostgreSQL, instead of silently losing
email.

## Migrations for both

`engine types sync` writes one migration per backend
(`migrations/postgres/` and `migrations/sqlite/`) from the same type files, so
a site can move between them. A site made before 0.5 gets its SQLite
migrations on its next `engine types sync`. Hand-written migrations (a field
type change) need one file per backend.

The engine's own tables have the same shape on both: SQLite stores times as
ISO-8601 text (`2026-11-26T07:00:00.123Z`, which sorts like time), JSON as
text with a `json_valid` check, and slugs are checked with `GLOB` patterns
instead of a regular expression.

## How changes reach the publisher on SQLite

PostgreSQL triggers write each change to `engine_changes` and send `NOTIFY`.
SQLite has the same triggers but no `NOTIFY`, so three things wake the
publisher instead:

1. an in-process signal from every save made through this server (the
   admin, the API): pages are rewritten at once;
2. a watch on the database file and its write-ahead log: edits by other
   processes (`engine content set`, the `sqlite3` shell) are seen within
   moments;
3. a poll of the change log every two seconds, for file systems where
   watching does not work.

Each wake-up only prompts a read of the change log from the publisher's
cursor, as on PostgreSQL, so nothing is lost while `engine serve` is down.

## How the engine's queries are checked

Up to 0.4 every query was a `sqlx::query!` macro, checked against a live
PostgreSQL schema at compile time, with offline data in `.sqlx/` that took an
hour to regenerate. That does not extend to two backends: the macros check
against one database type per build.

Since 0.5 each crate declares its statements in a `sql` module
(`engine_db::statements!`): one SQL text both backends accept (`$1`-style
parameters work in both), or a pair where the dialects differ, or a
PostgreSQL-only text for jobs. The **catalogue test** prepares every
statement against a fully migrated database of each backend, so an unknown
column, a missing table or a dialect slip fails in seconds:

```sh
scripts/check-queries.sh          # SQLite and PostgreSQL
scripts/check-queries.sh sqlite   # SQLite only, no container
```

Column types are decoded with `#[derive(sqlx::FromRow)]` structs and checked
by the test suite, which runs whole on both backends
(`scripts/test-sqlite.sh`, `scripts/test-postgres.sh`). Builds need no
database and no `DATABASE_URL`.
