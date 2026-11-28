+++
title = "Database: PostgreSQL"
description = "The one database the engine uses, how a new site gets one, how changes reach the publisher, and how the engine's queries are checked."
weight = 15
+++

The `static` preset needs no database at all: it builds pages from files.
Every database feature (content, publishing, accounts, roles, the admin,
review, forms, media, search, the JSON API, jobs, mail and payments) runs on
**PostgreSQL**, the only database the engine supports. One SQL dialect, one
code path: every query is written, tested and tuned once.

```toml
[database]
url = "postgres://engine@db.internal/site"
max_connections = 5      # optional: sized from the machine when unset
```

`ENGINE_DATABASE_URL` overrides `url` (keep passwords out of the site
folder in production).

## A database for a new site

`engine new my-site --preset cms` (or `app`) writes the site's own
`compose.yml`: PostgreSQL 17 in Docker, on a free port of `127.0.0.1` below
the kernel's ephemeral range, with a password generated for that folder.
`[database] url` in the new `site.toml` points at it, so one command starts
the database and nothing has to be exported:

```sh
engine new my-site --preset cms
cd my-site
docker compose up -d --wait      # this site's PostgreSQL
engine migrate && engine content seed && engine build
engine serve
```

For production, or to use a PostgreSQL server you already run, set
`ENGINE_DATABASE_URL` (it overrides the line in `site.toml`) and delete
`compose.yml`. On a small host one PostgreSQL server serves every site, a
database per site.

## SQLite was removed

Up to 0.7 a site could also use a SQLite file. That meant a second SQL text
for many statements, a second set of migrations and a second change-feed
mechanism, all to be kept in step. Since 0.8 the engine refuses a
`sqlite:` database URL with a message: create a PostgreSQL database (the
`compose.yml` above, or a server), point `ENGINE_DATABASE_URL` at it, and
run `engine migrate`. There is no automatic copy out of a SQLite file:
content is re-entered, or written into the site's seed files
(`seed/*.toml`, loaded by `engine content seed`). A site's
`migrations/sqlite/` folder is no longer read and can be deleted.

## How changes reach the publisher

Triggers write each change to `engine_changes` and send `NOTIFY`; the
publisher listens on a connection of its own and reads the change log from
its cursor, so nothing is lost while `engine serve` is down. A slow safety
poll covers a `NOTIFY` lost to a dropped connection.

## How the engine's queries are checked

Up to 0.4 every query was a `sqlx::query!` macro, checked against a live
PostgreSQL schema at compile time, with offline data in `.sqlx/` that took an
hour to regenerate. Since 0.5 each crate declares its statements in a `sql`
module (`engine_db::statements!`) and the **catalogue test** prepares every
one against a fully migrated database, so an unknown column, a missing table
or a syntax slip fails in seconds:

```sh
scripts/check-queries.sh
```

Column types are decoded with `#[derive(sqlx::FromRow)]` structs and checked
by the test suite (`scripts/test-postgres.sh`). Builds need no database and
no `DATABASE_URL`.

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
the same machine (Ingotyard's default: one VM), connect over the socket
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
Hammerwright and PostgreSQL together, with an SSD, these settings are a sound
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
Ingotyard's `postgres` role is meant to apply by RAM; the
[benchmark](/docs/benchmarks/) reports stock and tuned PostgreSQL
separately.
