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
| Jobs, mail and payments | not available | yes |
| Backups | copy the file (with `sqlite3 .backup`) | `pg_dump` |

SQLite suits single-editor sites, small teams, a Raspberry Pi, and trying
things out; PostgreSQL suits several editors, background jobs and email.

An `app` engine pointed at SQLite turns jobs and mail off and logs account
messages instead. A site whose configuration *asks* for them (`[jobs]` or
`[mail]` in `site.toml`, or a form with `[receipt]` or `[notify]`) refuses to
start with a message saying they need PostgreSQL, instead of silently losing
email. The same goes for a shop: `[payments]` needs `jobs`, so PostgreSQL.

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

## PostgreSQL: restarts and outages

The engine keeps serving when PostgreSQL restarts, sleeps or goes away:

- a query whose connection broke is run again on a fresh connection when
  that cannot apply it twice (reads; writes the server refused before they
  ran); connections left over from before are checked once, then used as
  usual, so there is no extra round trip per query;
- while the database is unreachable, published pages are served as always;
  a page rendered per request is served from its published copy when there
  is one, and otherwise answers `503` with `Retry-After` at once (no
  waiting for the pool); form posts go to the [spool](/docs/forms/);
- the engine probes in the background and carries on by itself when the
  database is back.

`GET /_engine/health` answers `200` (database up) or `503` with
`{"status":"ok"}` or `{"status":"degraded"}`; nothing else, to anyone. A
caller on the same machine that did not come through a proxy, or one that
sends `Authorization: Bearer $ENGINE_HEALTH_TOKEN` (set it in the engine's
environment, 16 characters or more), also gets
`"database":{"up","down_for_s","outages","probes","last_error"}` and
`"form_spool":{"posts","bytes","limit_bytes","quarantined"}`. It reports the
last known state without a query; `/_engine/health?check` connects first,
at most once every 10 seconds whoever asks. `engine doctor` also connects and says whether the
database answers. The engine still needs the database to start.

## PostgreSQL: connecting and tuning

**Same host: use the Unix socket.** When the engine and PostgreSQL run on
the same machine (Bloomery's default: one VM), connect over the socket
instead of TCP. Each query then skips the TCP stack, and a build or a
publish batch makes thousands of them:

```sh
export ENGINE_DATABASE_URL="postgres://engine@localhost/site?host=/run/postgresql"
```

The `host` parameter names the socket's folder (`/var/run/postgresql` on
Debian). In containers, share that folder between the database and the
engine as a volume. Password or peer authentication both work (`pg_hba.conf`
`local` lines).

**Pool.** `[database] max_connections` (unset: sized from the machine, CPUs
+ 4 between 4 and 32, see [limits](/docs/site-config/#limits); or
`ENGINE_DATABASE_MAX_CONNECTIONS`) is the shared pool for requests, renders,
publish batches and jobs. The publisher's and the job workers' `LISTEN`
connections are separate and do not count against it. One connection is
opened at start, and each connection keeps up to 512 prepared statements,
so a statement is parsed and planned once per connection, not per query.
For `engine serve` with jobs, 5–10 is plenty; `engine build` renders on
several threads and needs no more, because whole-site renders read the
content once into memory (see the render dial page).

**Indexes.** `engine types sync` creates, for every content type, a partial
index of the published rows in list order (`..._published_newest`), which
every listing, pattern route and `list_items()` uses. Sites made before 0.6
get it on their next `engine types sync` (the migration says "index
published"). The engine's own term table has a covering index for related
items and term listings.

**Durability.** Content saves, revisions, accounts and orders always wait
for the write-ahead log (`synchronous_commit` stays on). Only job
bookkeeping (claims, heartbeats, results) commits without waiting, per
transaction: a crash of the database server can at worst re-run a job,
which jobs already allow for (delivery is at least once).

### A tuning profile

PostgreSQL's defaults are sized for a tiny machine. For a host that runs
Wrought and PostgreSQL together, with an SSD, these settings are a sound
start (shown for 4 GB of RAM; scale the memory ones with the machine). Put
them in `postgresql.conf` or a file in `conf.d/`, or pass them with `-c` to
the container:

| Setting | 4 GB host | Rule of thumb | Why |
| --- | --- | --- | --- |
| `shared_buffers` | `1GB` | 25% of RAM | PostgreSQL's own cache |
| `effective_cache_size` | `3GB` | 75% of RAM | tells the planner the OS caches the rest |
| `work_mem` | `16MB` | RAM / (4 × max connections), at most 64MB | sorts and hashes per query step |
| `maintenance_work_mem` | `256MB` | 5–10% of RAM | index builds, vacuum |
| `random_page_cost` | `1.1` | 1.1 on SSD/NVMe | random reads cost about as much as sequential ones |
| `effective_io_concurrency` | `200` | 200 on SSD | prefetching for bitmap scans |
| `wal_compression` | `on` | | smaller WAL for the revision table's JSON |
| `checkpoint_completion_target` | `0.9` | | spreads checkpoint writes |
| `max_wal_size` | `2GB` | | fewer forced checkpoints during big imports |
| `autovacuum_vacuum_scale_factor` | `0.05` | | vacuums busy tables (sessions, jobs, change log) sooner |
| `autovacuum_analyze_scale_factor` | `0.02` | | keeps planner statistics fresh after imports |
| `autovacuum_naptime` | `30s` | | |
| `max_connections` | `50` | pool size × processes + headroom | each connection costs memory |

Leave `synchronous_commit`, `fsync` and `full_page_writes` on: the engine
already relaxes durability where it is safe (above). These values are what
Bloomery's `postgres` role is meant to apply by RAM; the
[benchmark](/docs/benchmarks/) reports stock and tuned PostgreSQL
separately.
