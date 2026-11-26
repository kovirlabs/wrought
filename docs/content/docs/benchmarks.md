+++
title = "Benchmarks"
description = "Wrought against WordPress, Drupal and Astro on the same content, on one machine: throughput, latency, build and publish times, memory and size, with the method and its limits."
weight = 19
+++

Measured on 2026-11-26 with `bench/run.sh` (one command; see
[`bench/`](https://github.com/kovirlabs/wrought/tree/main/bench)),
Wrought 0.6.0. Read the [limits](#limits) before quoting any number.

## Summary

- **Pages from files are fast for everyone who serves files.** Wrought's
  own server answered 11,966 requests a second for an article page on one
  core; Caddy serving the same files did 3,433, and Caddy serving Astro's
  files 7,062. Rails rendered the page at 343 a second (353 with its
  fragment caching), Drupal from its page cache 62, WordPress 13.
- **Dynamic pages:** search results, a different word each time: Wrought
  269 a second on SQLite and 167 on PostgreSQL; Rails 82 (caching does not
  apply to a different query each time); WordPress 11 and Drupal 10.
- **Builds:** 1000 articles from a database now build in about 2.3 s on
  PostgreSQL and 1.8 s on SQLite on one core (0.5: 14.9 s and 9.3 s), and
  from Markdown files in 0.7 s, against Astro's 6.4 to 7.4 s; see
  [cold publish](#cold-publish-of-1000-articles) for every run, including
  the slow ones caused by the disk. Rails, WordPress and Drupal render on
  request and have no build.
- **Where Wrought loses:** Caddy served Astro's article page faster than
  Wrought's (its file is 30 % smaller; we did not find the rest of the
  gap); an edit reached the public page in about a second on PostgreSQL in
  this run (140 ms in 0.5's run; see [edit to published](#edit-to-published)),
  slower than WordPress (197 ms) and Rails (28 to 42 ms, which simply
  render the next request); and one build in three still stalls for
  seconds on a busy disk.
- **Edit to published:** 21 ms on SQLite, against 197 ms for WordPress,
  768 ms for Drupal and 3.3 s for Astro, which rebuilds the site.
- **Memory:** 25 MB for Wrought on SQLite (96 MB with its PostgreSQL),
  about 190 MB for Rails with PostgreSQL (101 MB idle), 324 MB for
  WordPress with MariaDB, 460 MB for Drupal with PostgreSQL.
- **Cold start:** 0.7 s for Wrought and Drupal, 1.2 s for WordPress, 3.4 s
  for Rails.
- **Size:** a 5.3 MB binary for static sites, 31 MB with every feature and
  both databases.

## Results

Full run of 2026-11-26 (`bench/run.sh`, five systems, Wrought 0.6.0).

## Static page

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Wrought (`engine serve`) | 11,966 | 2.6 | 8.7 | 4,602 |
| Wrought output via Caddy | 3,433 | 9.0 | 31.2 | 4,602 |
| Astro output via Caddy | 7,062 | 4.5 | 12.4 | 3,198 |
| WordPress | 13 | 2,090 | 7,740 | 72,099 |
| Drupal | 62 | 470 | 3,780 | 17,695 |
| Rails (a) plain rendering | 343 | 93.1 | 101 | 3,749 |
| Rails (b) with Rails caching | 353 | 87.0 | 153 | 3,749 |

## Dynamic page (search, a different word per request)

| System | Requests/s | p50 ms | p99 ms |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 167 | 188 | 214 |
| Wrought, SQLite | 269 | 119 | 133 |
| WordPress | 11 | 2,430 | 6,100 |
| Drupal | 10 | 3,200 | 5,360 |
| Rails (a) plain rendering | 82 | 383 | 439 |
| Rails (b) with Rails caching | 82 | 386 | 423 |

## Cold publish of 1000 articles

Measured 2026-11-26 with `bench/run.sh pg before|after` (results in
`bench/results/pg-before.json` and `pg-after.json`): Wrought 0.5.0 against
0.6.0, the same machine, the same day, the same content, each build three
times on a fresh database, on one core and on four (the engine pins its
render threads, see [limits](#limits)). Times include `docker compose exec`
(about 0.4 s) and, for the database rows, writing the publish and search
indexes. PostgreSQL *stock* is the image's defaults; *tuned* is the
[tuning profile](/docs/databases/#a-tuning-profile) scaled to its 1 GB
container; *socket* is tuned over a Unix socket.

| Build of 1000 articles | 0.5, 1 core | 0.6, 1 core | 0.6, 4 cores | Runs, 0.6, 1 core (s) |
| --- | ---: | ---: | ---: | --- |
| Wrought, Markdown files (like Astro) | 0.7 s | 0.7 s | 0.5 s | 0.6, 0.7, 0.7 |
| Wrought, SQLite | 9.3 s | 8.9 s (1.8 s without stalls) | 1.6 s | 8.9, 10.8, 1.8 |
| Wrought, PostgreSQL stock | 14.9 s | 2.8 s | 2.1 s | 2.2, 10.3, 2.8 |
| Wrought, PostgreSQL tuned | 19.6 s | 2.3 s | 2.1 s | 2.3, 2.3, 2.4 |
| Wrought, PostgreSQL tuned, socket | 22.7 s | 2.5 s | 2.2 s | 2.2, 9.2, 2.5 |
| Astro (Markdown files) | — | 7.4 s | 6.5 s | 7.4, 10.0, 6.4 |

All builds wrote 1115 pages (Astro 1111: no colophon, search or 404). The
full five-system run later the same day measured Markdown 0.7 s, SQLite
1.9 s (1.6 s on four cores), Astro 6.4 s, and PostgreSQL 9.3 s as the
median of 9.3, 2.3 and 11.4 s: two of its three runs hit the disk stalls
described below (`bench/results/results.json`).
Medians of three; the stalled runs are shown, not dropped.

- **What changed:** a 0.5 build made about thirteen database round trips
  per article page (author, related articles and each of their terms, the
  cover, settings) and committed the search index one document at a time.
  0.6 reads every content type once for the whole build, answers those
  lookups from memory, and writes the tag and search indexes in one
  transaction each; pages render on several threads. On PostgreSQL the
  build is five to six times faster on one core.
- **Like for like with Astro** (both reading Markdown files): Wrought 0.7 s,
  Astro 7.4 s on one core. From a database, Wrought now builds faster than
  Astro builds from files.
- **The slow runs:** in both versions about one build in three took
  7 to 8 s longer. During those runs the engine used under 5 % of its core
  and waited on the disk: the host's file system was being resized and the
  kernel's background initialisation (`ext4lazyinit`) kept the disk busy
  (I/O pressure 25 to 50 %), so any `fsync` could stall for seconds. 0.5
  made over a thousand commits per build (one per search document), so it
  stalled more often and longer; 0.6 makes a handful, which is why its
  clean runs are clean. The v0.5 report's "one run in three about twice as
  slow" had the same shape.
- **Where tuning did not help:** for these builds the stock and tuned
  PostgreSQL settings are within the noise (the data fits in memory
  either way), and the Unix socket saved nothing measurable over the
  container network once round trips were gone. The tuning profile is for
  larger databases and concurrent editors, not this test.
- **Four cores** help the SQLite and Markdown builds most (rendering is
  the remaining work); PostgreSQL builds are already mostly waiting for
  the single-core database.

Search results (dynamic, PostgreSQL, a different word per request, one
core): 139 requests a second on 0.5 and 141 on 0.6 with stock settings,
161 with the tuning profile, 144 tuned over the socket (p99 249, 223 and
246 ms). The search page was already two queries; 0.6 runs them at the
same time, which matters little on one core.

## Edit to published

| System | Median | All runs (s) |
| --- | ---: | --- |
| Wrought, SQLite | 21 ms | 0.34, 0.02, 0.02, 0.02, 0.02 |
| Wrought, PostgreSQL | 1,055 ms | 0.57, 1.23, 1.05, 1.05, 1.09 |
| WordPress | 197 ms | 0.51, 0.19, 0.19, 0.20, 0.22 |
| Drupal | 768 ms | 0.96, 0.79, 0.73, 0.77, 0.74 |
| Astro | 3,340 ms | 3.34, 3.28, 3.35 |
| Rails (a) plain rendering | 28 ms | 0.20, 0.04, 0.02, 0.03, 0.02 |
| Rails (b) with Rails caching | 42 ms | 0.18, 0.05, 0.04, 0.04, 0.04 |

For Rails the number is the time until an edit is visible: it renders on
request, and in (b) the fragment keys change with `updated_at`, so the
next view already shows the edit. Wrought on PostgreSQL took about a second
here against 140 ms in the 0.5 run: in this run the disk was slow to sync
(PostgreSQL's `pg_test_fsync` measured 61 ms per `fdatasync`, normally a
few ms on this SSD, while the file system was being resized), and a
publish batch commits its index writes and advances its cursor; SQLite in
WAL mode does not wait for the disk on each commit (21 ms).

## Memory after the load runs (MiB)

| System | Application | Database | Total |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 19.0 | 77.0 | 96.0 |
| Wrought, SQLite | 24.8 | — | 24.8 |
| WordPress | 215.2 | 108.5 | 323.7 |
| Drupal | 406.8 | 52.9 | 459.7 |
| Astro (Caddy serving its files) | 18.6 | — | 18.6 |
| Rails (a) plain rendering | 120.7 | 68.4 | 189.1 |
| Rails (b) with Rails caching | 124.9 | 75.2 | 200.1 |

Rails idle (after start and one view): Rails (a) plain rendering 101.2 MiB, Rails (b) with Rails caching 103.2 MiB

The Rails rows are the highest sample during its load runs (its idle
figure is on the line above); the others are taken right after the load.

## Cold start (container started until a page answers)

| System | Seconds |
| --- | ---: |
| Wrought, SQLite | 0.7 |
| WordPress | 1.2 |
| Drupal | 0.7 |
| Rails (a) plain rendering | 3.4 |
| Rails (b) with Rails caching | 3.6 |

## Size

- Wrought binary: 5.3 MB (static preset), 30.6 MB (app preset, both databases)
- Container images: Wrought 202.8 MB (Ubuntu base + binary), WordPress 1123.9 MB + MariaDB 450.6 MB, Drupal 1009.4 MB (with Drush) + PostgreSQL 665.2 MB, Astro build: Node 239.1 MB + node_modules 159.6 MB; served by Caddy 91.7 MB
- Output for 200 articles: Wrought public/ 1.3 MB, Astro dist/ 0.8 MB

Drupal's page cache answered `X-Drupal-Cache: HIT` for the static page.

Drupal's page cache answered `X-Drupal-Cache: HIT` for the static page.

## Setup

| | |
| --- | --- |
| Machine | Intel Xeon Silver 4214 (12 cores, 24 threads, 2.2 GHz), 60 GB RAM, Ubuntu 26.04.1, kernel 7.0.0, Docker 29.10.0 |
| CPU placement | the application on core 6, its database on core 7, setup tools on core 8, the load generator on core 9; one core each (see below) |
| Memory caps | 2 GB per application container, 1 GB per database |
| Load | `wrk` (Alpine 3.22.8 package), 1 thread, 32 connections, 3 s warm-up then 10 s measured, 30 s request timeout, over loopback |
| Content | 200 generated articles (about 400 words, 2–3 topics each) from `bench/data/generate.py`, the same for all; 1000 for build times |
| Wrought | 0.6.0, `app` preset, release build (Rust 1.95.0), the `examples/cms-site` templates; SQLite and PostgreSQL 17.12 |
| WordPress | 7.1.3, `php8.4-apache` image, MariaDB 11.4.13, default theme, pretty permalinks, no caching plugin |
| Drupal | 11.3.23, `php8.4-apache-bookworm` image plus Drush, PostgreSQL 17.12, standard profile (internal page cache on, as installed) |
| Astro | 7.4.10 on Node 22.23.5, static output, Markdown imported with `import.meta.glob` |
| Ruby on Rails | 8.1.4.1 on Ruby 4.0.10 (`ruby:4.0.10-slim-trixie`), an app made by `rails new --database=postgresql` (the generator's defaults; JavaScript, mailers, Action Text/Cable, Active Storage and the deployment extras skipped), `RAILS_ENV=production` (eager loading, precompiled assets, YJIT as Rails enables it, log level info), Puma with the generated `config/puma.rb` (3 threads, no forked workers, the default for one core), PostgreSQL 17.12. Two runs: **(a)** plain rendering; **(b)** Rails caching: the `cache` helper with Russian-doll keys in Solid Cache (Rails 8's default store, in PostgreSQL) and `fresh_when` conditional GETs. The only changes to the generated production config: `force_ssl` off and the host allow-list cleared (plain HTTP on loopback, like everyone). Search with PostgreSQL full text and a GIN index, as its docs describe. Source in `bench/rails/`. |
| Static files | Caddy 2.11.8 serving Wrought's `public/` and Astro's `dist/` with the same configuration |

Image digests are in `bench/results/results.json`.

**Why one core each:** the machine boots with `isolcpus` on 22 of its 24
threads, so the scheduler never spreads a process's threads over several
cores. A first run with "4 CPUs" per container in fact gave every system
one core, chosen by chance (an application could share it with its
database). The harness now pins each role to its own core, which is equal
and explicit, but it is a one-core benchmark: on a normal machine PHP-FPM or
Apache workers, Caddy and Wrought's server all use more cores, and the
absolute numbers would be higher for everyone.

## What each test does

- **Static page:** `/articles/article-17/` (Drupal: `/article-17`). Wrought
  and Astro serve a file written at build time; WordPress renders the post
  with its default theme on every view; Drupal answers from its page cache
  (`X-Drupal-Cache: HIT`); Rails renders the page in (a) and assembles it
  from cached fragments in (b) (the load generator sends no
  `If-None-Match`, so conditional GETs do not apply).
- **Dynamic page:** the search page with a different word on each request
  (a `wrk` Lua script cycling through 20 words), so no page cache can
  answer: Wrought's `/search/?q=` (a `request` route over its full-text
  index), WordPress `/?s=`, Drupal `/search/node?keys=`, Rails `/search/?q=`. Astro has no
  server, so no row.
- **Cold publish:** a fresh database (or folder) with 1000 articles, then
  the time of `engine build` (every page, the publish index and the search
  index) or `astro build`, three times. WordPress and Drupal do not
  pre-render pages, and Rails has no static build, so they have no row
  ("not applicable").
- **Edit to published:** change an article's title through each system's
  API (Wrought's JSON API, the WordPress REST API with an application
  password, Drupal's JSON:API, a token-protected JSON update in the Rails
  app), then poll the public page every 5 ms until
  the new title shows. For Astro: edit the Markdown file, run `astro build`,
  poll. Five edits (Astro three); the median is shown.
- **Memory:** `docker stats` right after the load runs; for Rails also
  idle (after start and one view) and the highest sample taken every three
  seconds during its load runs.
- **Cold start:** stop a server's container, start it, and time it until
  a page answers (Wrought, WordPress, Drupal, Rails).

## Fairness

Every system runs as its official image ships it, with its default
configuration; nothing was tuned for the test. Two setup steps exist only
to make the defaults work in containers and are applied to every system
that needs them: WordPress's due cron events are run once (WP-Cron calls the
site's own URL, which is not reachable from inside its container, so it
would otherwise retry on every page view), and WordPress is told it is a
local site so application passwords work over plain HTTP. Drupal's page
cache is on because it is on by default; WordPress has no page cache by
default, and adding a plugin would be tuning. Pages differ in weight
(WordPress's default theme sends 72 KB of HTML, Drupal's 18 KB, Wrought's
4.5 KB, Astro's 3.2 KB): that is part of what each system costs.

## Limits

- **One machine, loopback.** No network latency, no TLS, no compression;
  the load generator shares the machine (on its own core). Real traffic
  arrives slower and spread out.
- **One core per system,** for the reason above. More cores would change
  the ratios: PHP scales with workers, Caddy and Wrought with threads.
- **A busy disk on the day of the 0.6 runs.** The host's file system had
  just been resized, and the kernel's background initialisation kept the
  disk busy (I/O pressure 25 to 50 %, 61 ms per `fdatasync`). Anything that
  waits for the disk (database commits, builds) was slower and more
  variable than the CPU-bound rows; all runs are reported.
- **Short runs, one sample each** (10 seconds per load test). Two runs
  with this setup differed by under 5 % on the static rows; the build
  times varied more (see above). Take the orders of magnitude, not the
  digits.
- **Small data:** 200 articles for the load tests. Search and listing
  costs grow with content; a site with 100,000 items needs its own test.
- **What a CDN or page cache would change:** any of these systems behind a
  CDN serves cached pages at the CDN's speed. Wrought's point is that its
  published pages are files from the start, without a cache to warm or
  purge.
- **The edit test measures each system's own path** (Wrought: publish
  through the change feed; WordPress and Drupal: render on the next view;
  Astro: full rebuild); it does not include a CDN purge.

## Run it yourself

```sh
bench/run.sh                    # about 25 minutes; needs Docker and network
python3 bench/report.py         # the tables above, from bench/results/results.json
bench/run.sh build              # only the cold builds (Markdown, SQLite, PostgreSQL, Astro)
BENCH_ENGINE=path/to/engine bench/run.sh pg <label>   # builds and search on stock,
                                # tuned and socket PostgreSQL for one binary
                                # (results/pg-<label>.json; BENCH_SITE_REV=<git rev>
                                # for an older engine's example site)
BENCH_APP_CPU=2 BENCH_DB_CPU=3 BENCH_TOOL_CPU=4 BENCH_LOAD_CPU=5 bench/run.sh
```

The run removes its containers, volumes and working folder when it ends
(`BENCH_KEEP=1` keeps them); `BENCH_RMI=all` also removes the pulled images.
