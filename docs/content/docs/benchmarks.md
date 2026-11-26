+++
title = "Benchmarks"
description = "Wrought against WordPress, Drupal and Astro on the same content, on one machine: throughput, latency, build and publish times, memory and size, with the method and its limits."
weight = 19
+++

Measured on 2026-11-26 with `bench/run.sh` (one command; see
[`bench/`](https://codeberg.org/kovirlabs/wrought/src/branch/main/bench)),
Wrought 0.5.0. Read the [limits](#limits) before quoting any number.

## Summary

- **Pages from files are fast for everyone who serves files.** Wrought's
  own server answered 12,964 requests a second for an article page on one
  core; Caddy serving the same files did 3,924, and Caddy serving Astro's
  files 7,144. WordPress, rendering every view, did 11; Drupal, from its
  page cache, 61.
- **Dynamic pages:** search results, a different word each time, rendered
  per request: Wrought 289 a second on SQLite and 161 on PostgreSQL;
  WordPress 9 and Drupal 10 (seconds of latency at 32 connections on one
  core).
- **Where Wrought loses:** building 1000 articles took Wrought 7.8 s on
  SQLite and 13.7 s on PostgreSQL; Astro took 6.4 s. Wrought's build renders on
  one thread with several database queries per page (the author, related
  articles, the cover, the publish index), which is the likely cost; Astro
  reads Markdown files. Caddy also
  served Astro's article page faster than Wrought's (its file is 30 %
  smaller; we did not find the rest of the gap). Wrought's builds also
  varied more between runs (one run in three was about twice as slow).
- **Edit to published:** after an API edit, the public article page showed
  the change in 25 ms on SQLite and 140 ms on PostgreSQL (median; the
  first edit after a restart took 0.6 and 2.3 s), against 305 ms for
  WordPress and 834 ms for Drupal, which render on the next request, and
  3.1 s for Astro, which rebuilds the site.
- **Memory:** 19 MB for Wrought on SQLite (89 MB with its PostgreSQL),
  255 MB for WordPress with MariaDB, 499 MB for Drupal with PostgreSQL.
- **Size:** a 4.8 MB binary for static sites, 26 MB with every feature and
  both databases.

## Results

## Static page

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Wrought (`engine serve`) | 12,964 | 2.6 | 4.2 | 4,551 |
| Wrought output via Caddy | 3,924 | 8.1 | 20.8 | 4,551 |
| Astro output via Caddy | 7,144 | 4.5 | 12.4 | 3,198 |
| WordPress | 11 | 2,450 | 4,030 | 72,099 |
| Drupal | 61 | 456 | 4,570 | 17,695 |

## Dynamic page (search, a different word per request)

| System | Requests/s | p50 ms | p99 ms |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 161 | 198 | 213 |
| Wrought, SQLite | 289 | 110 | 131 |
| WordPress | 9 | 2,700 | 4,670 |
| Drupal | 10 | 3,270 | 4,210 |

## Cold publish of 1000 articles

| System | Median s | Runs (s) | Pages |
| --- | ---: | --- | ---: |
| Wrought, PostgreSQL | 13.7 | 13.2, 13.7, 22.9 | 1115 |
| Wrought, SQLite | 7.8 | 7.7, 7.8, 15.9 | 1115 |
| Astro | 6.4 | 6.4, 6.6, 6.1 | 1111 |

## Edit to published

| System | Median | All runs (s) |
| --- | ---: | --- |
| Wrought, SQLite | 25 ms | 0.63, 0.03, 0.02, 0.02, 0.02 |
| Wrought, PostgreSQL | 140 ms | 2.25, 0.12, 0.14, 0.22, 0.13 |
| WordPress | 305 ms | 0.49, 0.31, 0.30, 0.27, 0.30 |
| Drupal | 834 ms | 1.02, 9.73, 0.83, 0.72, 0.78 |
| Astro | 3,103 ms | 3.14, 3.10, 3.10 |

## Memory after the load runs (MiB)

| System | Application | Database | Total |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 17.3 | 71.8 | 89.1 |
| Wrought, SQLite | 18.7 | — | 18.7 |
| WordPress | 142.8 | 112.4 | 255.2 |
| Drupal | 445.7 | 53.2 | 498.9 |
| Astro (Caddy serving its files) | 18.5 | — | 18.5 |

## Size

- Wrought binary: 4.8 MB (static preset), 26.0 MB (app preset, both databases)
- Container images: Wrought 196.2 MB (Ubuntu base + binary), WordPress 1123.9 MB + MariaDB 450.6 MB, Drupal 1009.4 MB (with Drush) + PostgreSQL 665.2 MB, Astro build: Node 239.1 MB + node_modules 136.1 MB; served by Caddy 91.7 MB
- Output for 200 articles: Wrought public/ 1.3 MB, Astro dist/ 0.8 MB

Drupal's page cache answered `X-Drupal-Cache: HIT` for the static page.


## Setup

| | |
| --- | --- |
| Machine | Intel Xeon Silver 4214 (12 cores, 24 threads, 2.2 GHz), 60 GB RAM, Ubuntu 26.04.1, kernel 7.0.0, Docker 29.10.0 |
| CPU placement | the application on core 6, its database on core 7, setup tools on core 8, the load generator on core 9; one core each (see below) |
| Memory caps | 2 GB per application container, 1 GB per database |
| Load | `wrk` (Alpine 3.22.8 package), 1 thread, 32 connections, 3 s warm-up then 10 s measured, 30 s request timeout, over loopback |
| Content | 200 generated articles (about 400 words, 2–3 topics each) from `bench/data/generate.py`, the same for all; 1000 for build times |
| Wrought | 0.5.0, `app` preset, release build (Rust 1.95.0), the `examples/cms-site` templates; SQLite and PostgreSQL 17.12 |
| WordPress | 7.1.3, `php8.4-apache` image, MariaDB 11.4.13, default theme, pretty permalinks, no caching plugin |
| Drupal | 11.3.23, `php8.4-apache-bookworm` image plus Drush, PostgreSQL 17.12, standard profile (internal page cache on, as installed) |
| Astro | 7.4.10 on Node 22.23.5, static output, Markdown imported with `import.meta.glob` |
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
  (`X-Drupal-Cache: HIT`).
- **Dynamic page:** the search page with a different word on each request
  (a `wrk` Lua script cycling through 20 words), so no page cache can
  answer: Wrought's `/search/?q=` (a `request` route over its full-text
  index), WordPress `/?s=`, Drupal `/search/node?keys=`. Astro has no
  server, so no row.
- **Cold publish:** a fresh database (or folder) with 1000 articles, then
  the time of `engine build` (every page, the publish index and the search
  index) or `astro build`, three times. WordPress and Drupal do not
  pre-render pages, so they have no row.
- **Edit to published:** change an article's title through each system's
  API (Wrought's JSON API, the WordPress REST API with an application
  password, Drupal's JSON:API), then poll the public page every 5 ms until
  the new title shows. For Astro: edit the Markdown file, run `astro build`,
  poll. Five edits (Astro three); the median is shown.
- **Memory:** `docker stats` right after the load runs.

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
BENCH_APP_CPU=2 BENCH_DB_CPU=3 BENCH_TOOL_CPU=4 BENCH_LOAD_CPU=5 bench/run.sh
```

The run removes its containers, volumes and working folder when it ends
(`BENCH_KEEP=1` keeps them); `BENCH_RMI=all` also removes the pulled images.
