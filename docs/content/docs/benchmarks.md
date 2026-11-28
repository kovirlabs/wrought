+++
title = "Benchmarks"
description = "Hammerwright against WordPress, Drupal, Astro, Phoenix, Loco, Rails, Hugo and Zola on the same content, on one machine: throughput, latency, build and publish times, memory and size, with the method and its limits."
weight = 19
+++


Measured with `bench/run.sh` (one command; see
[`bench/`](https://github.com/kovirlabs/hammerwright/tree/main/bench)); the
latest run is first, earlier runs follow as history. Read the
[limits](#limits) before quoting any number.

## Results of 2026-11-28 (PostgreSQL-only build)

The current numbers. SQLite was removed in 0.8, so the engine is measured
on PostgreSQL only; there is no SQLite row in this run. Older SQLite rows
appear only in the historical sections further down.

**Method.** Commit `435d3a3` (branch `v0.8-benchmarks`; engine version
string `0.7.0`), release build made by the harness, on the same machine as
every earlier run: Intel Xeon Silver 4214 (24 threads, 2.2 GHz), 60 GB RAM,
Ubuntu 26.04.1, kernel 7.0.0, Docker 29.10.0, rustc 1.100.0, `isolcpus`
on all but two threads. One dedicated core each for the application, its
database, the tools and the load generator; 2 GB per application
container, 1 GB per database. There were three runs, and each wrote its own file:

- `bench/run.sh --group core` (Hammerwright, WordPress, Drupal, Astro, Phoenix
  and Loco serving; Hammerwright, Astro, Hugo and Zola building; 102 minutes)
  into `bench/results/results.json`;
- `bench/run.sh --group extended` (Rails; 11 minutes) into
  `bench/results/results-extended.json`;
- `bench/run.sh --targets drupal,loco` (23 minutes), merged into
  results.json with `bench/merge.py` (`reruns` in results.json; the file
  is `results/rerun-2026-11-28-drupal-loco.json`). The core run had two
  obvious outliers: Drupal's search at 3.3 requests a second (its own
  resource profile, minutes later, measured 10.3) and Loco's cold start at
  8.5 s (0.78 s before). The re-run measured 10.1 and 0.58 s. Every Drupal
  and Loco figure below comes from the re-run.

The tables are `python3 bench/report.py` of these files. All figures come
from this run. The 2026-11-27 files are kept in `bench/results/2026-11-27/`
for comparison.

**Idle memory is now read once it has settled.** Earlier runs read
"idle" 5 s after a warm-up and "idle after" 10 s after the last load
test. The engine hands freed memory back 10 to 20 s after its last
request, so those readings were taken too early. The harness now waits
60 s with no requests, then reads every 15 s until two readings in a row
agree within 2 % (total and anonymous memory). Every stack settled at the
first pair (75 s quiet); the readings are kept under `settle` in
results.json. Because of this, the idle figures are not directly comparable with
earlier runs (see [changes](#changes-since-the-2026-11-27-run)).

**Which Hammerwright figure is which.** *Published file* rows are pages
written ahead of time and served as files (by `engine serve` or Caddy).
Compare them with Astro, Hugo and Zola and with page caches. *Rendered per
request* rows (render dial `request`, PostgreSQL) build the page on every
request like Phoenix, Loco, Rails, WordPress and Drupal do. Compare those
with each other. Hammerwright's search page is always rendered per request.

**Where Hammerwright loses, plainly:**

- **Loco, rendered per request, is faster on every page.** Article: Loco
  1,766 requests a second, Hammerwright 1,624. Listing: Loco 2,781, Hammerwright
  1,534. Search: Loco 2,246, Hammerwright 1,108. CPU per 1,000 requests:
  articles 1.37 s (Loco) against 1.41 s (Hammerwright), listings 0.89 against
  1.41, searches 1.04 against 2.09. Loco's search does less work: it
  returns the 20 newest matches, while Hammerwright's ranks every match, cuts a
  highlighted extract for each hit and counts the total. Hammerwright has the
  lower tail latency: article p99 22 ms against 30, search p99 34 ms
  against 255.
- **Phoenix answers more searches:** 1,749 a second against 1,108, at
  1.34 s of CPU per 1,000 against 2.09. Hammerwright renders more articles
  (1,624 against 1,022) and listings (1,534 against 1,388).
- **Edit to published:** Phoenix 20 ms, Loco 26 ms and Rails 33 to 44 ms
  just render the next request. Hammerwright takes 167 ms (median of five) to
  re-render and publish the affected pages. It is still faster than
  WordPress (214 ms), Drupal (711 ms) and Astro (3.3 s, a full rebuild).
- **Hugo and Zola build 1000 Markdown articles faster when Hammerwright writes
  its compressed copies:** Hammerwright 1.13 s, Hugo 0.74 s, Zola 0.69 s (each
  includes about 0.35 s of `docker exec`). Without the copies
  (`ENGINE_BUILD_PRECOMPRESS=0`), Hammerwright takes 0.68 s, level with them.
  Hugo and Zola write no compressed copies, search index or tag index.
- **Idle memory, Loco against the engine:** the Loco process idles in 4.4 MiB
  (2.9 anonymous) and holds 6.8 MiB after its load tests. The
  request-mode engine idles in 5.2 MiB (4.0) but holds 16.2 MiB (14.9)
  after its load tests, even after 75 s of quiet. The publish-mode
  engine, which by then has served every load test of the run, holds
  21.7 MiB (18.8) before its profile and 25.7 MiB (23.0) after it.
- **Cold start:** Loco 0.58 s, Hammerwright 0.87 s, Drupal 0.99 s.
- **Caddy serving Hammerwright's files is slower than Caddy serving Astro's**
  (3,931 against 7,105 requests a second without `Accept-Encoding`), and
  also slower than WordPress behind Apache's page cache (4,514). Hammerwright's own `engine serve`
  answers 9,095.
- **Slower than the 2026-11-27 run:** published files through `engine
  serve` (article −6.5 %, listing −9.7 %, CPU per page +8 %), the
  request-rendered listing (−5.7 %) and the request-mode engine's memory
  after load (+51 %). None of these has been investigated (see
  [changes](#changes-since-the-2026-11-27-run)).

**Where it does not:** published pages from `engine serve` (9,095 a
second, 0.15 s of CPU per 1,000) outrun Astro via Caddy (7,105). Builds from a
database (2.22 s for 1000 articles from PostgreSQL) are three times faster
than Astro's build from files (6.97 s). Twenty sites run on one PostgreSQL
server at 31.9 MiB each. The engine served the load test at an 8 MiB
container limit (the harness tries nothing smaller). Against Rails,
WordPress and Drupal, Hammerwright is faster on every page measured.

**Noise seen in this run.** The disk probe beside the first PostgreSQL
build caught one `fdatasync` that took 6.6 s. The build still finished in
2.22 s, because it no longer waits for a sync. No edit waited for the disk
this time, but in the 2026-11-27 run two Hammerwright edits and two WordPress
edits did. Drupal's search is bimodal on this machine: each run had one
search measurement at about 10 a second and one at about 3. The
"CPU s / 1,000 searches" for Drupal (382) comes from a slow one (3.2 a
second); the search table's 10.1 comes from a fast one.

**Memory figures** are sums over a target's containers from cgroup v2.
`memory.current` includes the page cache of the files a container read (a
database's data files count there), so the anonymous memory (what the
processes allocated) is shown beside it, and the application and database
are also shown separately. **Page caches:** WordPress is shown stock and
with Apache's `mod_cache` (disk store, anonymous pages, 10 minutes,
`CacheIgnoreNoLastMod On`, `CacheIgnoreHeaders Set-Cookie`, quick
handler). Drupal's stock install already caches pages. Phoenix serves its
stylesheet gzipped (`mix phx.digest`); Loco and Rails serve it
uncompressed, as generated.

### Article page

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Hammerwright, published file (`engine serve`) | 9,095 | 3.6 | 5.9 | 4,635 |
| Hammerwright, published file via Caddy | 3,931 | 8.1 | 20.6 | 4,635 |
| Astro output via Caddy | 7,105 | 4.5 | 12.5 | 3,198 |
| WordPress (stock) | 12 | 2,270 | 7,290 | 72,124 |
| Drupal (stock: its page cache is on) | 62 | 472 | 4,240 | 17,695 |
| WordPress with a page cache (Apache mod_cache) | 4,514 | 4.1 | 460 | n/a |
| Hammerwright, rendered per request (render dial `request`) | 1,624 | 19.6 | 22.1 | n/a |
| Phoenix (rendered per request) | 1,022 | 31.2 | 45.9 | 3,764 |
| Loco (rendered per request) | 1,766 | 17.7 | 30.3 | 3,757 |
| Rails (a) plain rendering | 351 | 90.8 | 99.2 | 3,749 |
| Rails (b) with Rails caching | 379 | 84.0 | 93.7 | 3,749 |

### Listing page (/articles/, 10 per page)

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Hammerwright, published file (`engine serve`) | 8,970 | 3.5 | 7.5 | 4,940 |
| Hammerwright, rendered per request (render dial `request`) | 1,534 | 20.7 | 24.5 | n/a |
| Phoenix (rendered per request) | 1,388 | 22.9 | 31.4 | 3,715 |
| Loco (rendered per request) | 2,781 | 11.3 | 16.0 | 3,714 |
| Rails (a) plain rendering | 431 | 74.0 | 82.0 | 3,710 |
| Rails (b) with Rails caching | 401 | 75.2 | 139 | 3,710 |

### Dynamic page (search, a different word per request)

| System | Requests/s | p50 ms | p99 ms |
| --- | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | 1,091 | 28.7 | 36.8 |
| WordPress (stock) | 9 | 2,760 | 4,750 |
| Drupal (stock: its page cache is on) | 10 | 3,060 | 3,750 |
| Hammerwright, rendered per request (render dial `request`) | 1,108 | 28.6 | 33.8 |
| Phoenix (rendered per request) | 1,749 | 17.8 | 172 |
| Loco (rendered per request) | 2,246 | 13.4 | 255 |
| Rails (a) plain rendering | 82 | 384 | 506 |
| Rails (b) with Rails caching | 81 | 388 | 452 |

### Cold build of 1000 articles (one core)

| System | Median s | Runs (s) | Peak memory MiB | Output MB | 4 cores, median s |
| --- | ---: | --- | ---: | ---: | ---: |
| Hammerwright, Markdown files | 1.13 | 1.20, 1.13, 1.10 | 21.4 | 9.2 | 0.59 |
| Hammerwright, Markdown files, no compressed copies | 0.68 | 0.68, 0.61, 0.71 | 18.2 | 5.4 | 0.47 |
| Hammerwright, PostgreSQL | 2.22 | 2.22, 2.26, 2.21 | 42.0 | 10.7 | 1.67 |
| Astro | 6.97 | 7.32, 6.97, 6.53 | 354.7 | 3.6 | 6.37 |
| Hugo | 0.74 | 0.86, 0.73, 0.74 | 64.2 | 7.7 | n/a |
| Zola | 0.69 | 0.69, 0.62, 0.73 | 150.6 | 3.7 | n/a |

Hammerwright, Markdown files, phases of a median run: load templates 42 ms, static files 9 ms, render and write pages 723 ms.

Hammerwright, PostgreSQL, phases of a median run: connect 13 ms, open site 14 ms, search index 723 ms, build 990 ms, tag index 128 ms, (outside the phases) 16 ms.

One article changed, then built again: Hugo 0.77 s (full rebuild (no incremental build)); Zola 0.71 s (full rebuild (no incremental build))

### Edit to published

| System | Median | All runs (s) |
| --- | ---: | --- |
| Hammerwright, PostgreSQL | 167 ms | 0.13, 0.17, 0.28, 0.08, 0.29 |
| WordPress (stock) | 214 ms | 0.46, 0.18, 0.21, 0.21, 0.23 |
| Drupal (stock: its page cache is on) | 711 ms | 0.98, 0.70, 0.71, 0.71, 0.81 |
| Astro | 3,316 ms | 3.49, 3.31, 3.32 |
| Phoenix (rendered per request) | 20 ms | 0.02, 0.02, 0.02, 0.02, 0.02 |
| Loco (rendered per request) | 26 ms | 0.07, 0.02, 0.03, 0.03, 0.02 |
| Rails (a) plain rendering | 33 ms | 0.18, 0.05, 0.03, 0.03, 0.03 |
| Rails (b) with Rails caching | 44 ms | 0.26, 0.10, 0.04, 0.04, 0.04 |

### Whole-stack memory and CPU (cgroup accounting, MiB)

| System | Idle | Light load | Peak, load test | Idle after | CPU s / 1,000 pages | CPU s / 1,000 searches |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | 197.9 | 198.5 | 200.4 | 202.0 | 0.147 | 1.976 |
| WordPress (stock) | 532.6 | 535.8 | 731.0 | 547.4 | 139.270 | 144.254 |
| Drupal (stock: its page cache is on) | 292.0 | 293.4 | 421.3 | 292.4 | 35.181 | 381.793 |
| Astro | 11.9 | 15.9 | 19.6 | 18.3 | 0.184 | n/a |
| WordPress with a page cache (Apache mod_cache) | 464.6 | 469.1 | 485.6 | 541.2 | 0.289 | 151.835 |
| Hammerwright, rendered per request (render dial `request`) | 166.9 | 168.1 | 178.4 | 186.7 | 1.410 | 2.088 |
| Phoenix (rendered per request) | 238.2 | 239.4 | 248.5 | 265.8 | 1.997 | 1.340 |
| Loco (rendered per request) | 83.7 | 89.6 | 107.9 | 86.5 | 1.366 | 1.040 |
| Rails (a) plain rendering | 238.4 | 247.7 | 255.6 | 261.6 | 4.967 | 20.390 |
| Rails (b) with Rails caching | 245.7 | 259.0 | 272.0 | 291.1 | 4.470 | 20.538 |

Containers counted: Hammerwright, PostgreSQL: engine, wrought-db; WordPress (stock): wp, wp-db; Drupal (stock: its page cache is on): drupal, drupal-db; Astro: caddy; WordPress with a page cache (Apache mod_cache): wp, wp-db; Hammerwright, rendered per request (render dial `request`): engine, wrought-db; Phoenix (rendered per request): phoenix, phoenix-db; Loco (rendered per request): loco, loco-db; Rails (a) plain rendering: rails, rails-db; Rails (b) with Rails caching: rails, rails-db. `memory.current` includes the page cache the containers' files occupy; the anonymous memory alone: Hammerwright, PostgreSQL 44.8, WordPress (stock) 184.8, Drupal (stock: its page cache is on) 76.7, Astro 10.9, WordPress with a page cache (Apache mod_cache) 127.3, Hammerwright, rendered per request (render dial `request`) 18.5, Phoenix (rendered per request) 126.1, Loco (rendered per request) 9.5, Rails (a) plain rendering 105.2, Rails (b) with Rails caching 108.7 MiB idle.

### Application and database memory, separately (MiB; cgroup total / anonymous)

| System | Container | Idle | Peak, load test (article) | Peak, search |
| --- | --- | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | engine | 21.7 / 18.8 | 24.2 / 19.8 | 30.7 / 26.6 |
| Hammerwright, PostgreSQL | wrought-db | 176.3 / 26.0 | 176.5 / 26.0 | 177.2 / 26.9 |
| WordPress (stock) | wp | 382.5 / 76.4 | 579.8 / 258.5 | 624.3 / 302.9 |
| WordPress (stock) | wp-db | 150.1 / 108.4 | 151.9 / 109.0 | 152.2 / 109.7 |
| Drupal (stock: its page cache is on) | drupal | 134.3 / 72.3 | 223.3 / 145.0 | 427.6 / 350.8 |
| Drupal (stock: its page cache is on) | drupal-db | 157.7 / 4.4 | 222.1 / 60.7 | 270.8 / 104.3 |
| Astro | caddy | 11.9 / 10.9 | 19.6 / 18.0 | n/a |
| WordPress with a page cache (Apache mod_cache) | wp | 315.9 / 17.7 | 336.7 / 27.4 | 633.7 / 314.5 |
| WordPress with a page cache (Apache mod_cache) | wp-db | 148.7 / 109.6 | 148.9 / 109.6 | 151.9 / 111.1 |
| Hammerwright, rendered per request (render dial `request`) | engine | 5.2 / 4.0 | 10.8 / 8.7 | 19.2 / 17.1 |
| Hammerwright, rendered per request (render dial `request`) | wrought-db | 161.7 / 14.5 | 167.8 / 19.7 | 171.2 / 23.3 |
| Phoenix (rendered per request) | phoenix | 143.4 / 105.4 | 151.3 / 113.1 | 163.5 / 125.1 |
| Phoenix (rendered per request) | phoenix-db | 94.8 / 20.7 | 97.5 / 23.0 | 103.2 / 28.6 |
| Loco (rendered per request) | loco | 4.4 / 2.9 | 7.1 / 5.0 | 7.4 / 5.2 |
| Loco (rendered per request) | loco-db | 79.3 / 6.6 | 101.1 / 25.8 | 103.0 / 27.6 |
| Rails (a) plain rendering | rails | 105.0 / 98.8 | 117.7 / 111.1 | 122.9 / 116.0 |
| Rails (a) plain rendering | rails-db | 133.5 / 6.4 | 138.2 / 10.1 | 139.7 / 11.8 |
| Rails (b) with Rails caching | rails | 106.5 / 100.2 | 122.2 / 115.3 | 131.6 / 125.0 |
| Rails (b) with Rails caching | rails-db | 139.2 / 8.5 | 149.9 / 15.4 | 155.0 / 17.2 |

The total includes the page cache of the files a container read (for a database, its data files); the anonymous figure is what its processes allocated.

| System | CPU s / 1,000 articles | CPU s / 1,000 listings | CPU s / 1,000 searches |
| --- | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | 0.147 | 0.144 | 1.976 |
| WordPress (stock) | 139.270 | n/a | 144.254 |
| Drupal (stock: its page cache is on) | 35.181 | n/a | 381.793 |
| Astro | 0.184 | n/a | n/a |
| WordPress with a page cache (Apache mod_cache) | 0.289 | n/a | 151.835 |
| Hammerwright, rendered per request (render dial `request`) | 1.410 | 1.407 | 2.088 |
| Phoenix (rendered per request) | 1.997 | 1.409 | 1.340 |
| Loco (rendered per request) | 1.366 | 0.894 | 1.040 |
| Rails (a) plain rendering | 4.967 | 4.047 | 20.390 |
| Rails (b) with Rails caching | 4.470 | 4.336 | 20.538 |

### Cold start (container started until the article page answers)

| System | Seconds |
| --- | ---: |
| Phoenix (rendered per request) | 2.64 |
| Loco (rendered per request) | 0.58 |
| Hammerwright, PostgreSQL | 0.87 |
| WordPress (stock) | 2.77 |
| Drupal (stock: its page cache is on) | 0.99 |
| Rails (a) plain rendering | 3.02 |
| Rails (b) with Rails caching | 3.19 |

### Smallest memory limit that still serves the load test

| System | Application container | Database container |
| --- | ---: | ---: |
| Hammerwright, PostgreSQL | 8 MiB | 96 MiB |
| WordPress (stock) | 384 MiB | 128 MiB |
| Drupal (stock: its page cache is on) | 256 MiB | 192 MiB |

### Page weight, first visit, default theme (headless Chromium)

| System | Page | Requests | Transferred KB | Uncompressed KB | HTML | CSS | JS | Images | Fonts |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Phoenix (rendered per request) | home | 2 | 13.3 | 67.3 | 0.7 | 12.6 | 0 | 0 | 0 |
| Phoenix (rendered per request) | listing | 2 | 13.8 | 69.5 | 1.2 | 12.6 | 0 | 0 | 0 |
| Phoenix (rendered per request) | article | 2 | 14.2 | 69.5 | 1.6 | 12.6 | 0 | 0 | 0 |
| Loco (rendered per request) | home | 2 | 67.3 | 67.3 | 1.5 | 65.7 | 0 | 0 | 0 |
| Loco (rendered per request) | listing | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |
| Loco (rendered per request) | article | 2 | 69.5 | 69.5 | 3.8 | 65.7 | 0 | 0 | 0 |
| Hammerwright, PostgreSQL | home | 4 | 24.9 | 104.1 | 1.2 | 12.1 | 11.6 | 0 | 0 |
| Hammerwright, PostgreSQL | listing | 4 | 25.2 | 105.6 | 1.4 | 12.1 | 11.6 | 0 | 0 |
| Hammerwright, PostgreSQL | article | 4 | 25.5 | 105.3 | 1.8 | 12.1 | 11.6 | 0 | 0 |
| Astro | home | 2 | 66.9 | 66.9 | 1.1 | 65.7 | 0 | 0 | 0 |
| Astro | listing | 2 | 67.0 | 67.0 | 1.3 | 65.7 | 0 | 0 | 0 |
| Astro | article | 2 | 68.9 | 68.9 | 3.2 | 65.7 | 0 | 0 | 0 |
| WordPress (stock) | home | 5 | 90.7 | 200.2 | 20.9 | 0 | 16.3 | 0 | 53.6 |
| WordPress (stock) | listing | 5 | 91.3 | 202.0 | 21.5 | 0 | 16.3 | 0 | 53.6 |
| WordPress (stock) | article | 7 | 87.9 | 192.6 | 14.1 | 2.6 | 17.6 | 0 | 53.6 |
| Drupal (stock: its page cache is on) | home | 8 | 111.1 | 217.9 | 6.0 | 13.4 | 7.5 | 0 | 84.2 |
| Drupal (stock: its page cache is on) | listing | 8 | 111.1 | 217.9 | 6.0 | 13.4 | 7.5 | 0 | 84.2 |
| Drupal (stock: its page cache is on) | article | 8 | 111.2 | 205.7 | 5.4 | 13.7 | 7.8 | 0 | 84.2 |
| Rails (a) plain rendering | home | 2 | 67.3 | 67.3 | 1.5 | 65.7 | 0 | 0 | 0 |
| Rails (a) plain rendering | listing | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |
| Rails (a) plain rendering | article | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |
| Rails (b) with Rails caching | home | 2 | 67.3 | 67.3 | 1.5 | 65.7 | 0 | 0 | 0 |
| Rails (b) with Rails caching | listing | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |
| Rails (b) with Rails caching | article | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |

### 20 Hammerwright sites on one PostgreSQL server

| PostgreSQL | PostgreSQL MiB | Engine each MiB | Total MiB | Per site MiB | Connections |
| --- | ---: | ---: | ---: | ---: | ---: |
| defaults (`-c max_connections=200`) | 779.7 | 4.7 | 873.8 | 43.7 | 97 |
| small (`-c shared_buffers=32MB -c work_mem=2MB -c maintenance_work_mem=16MB -c max_connections=200 -c wal_buffers=1MB -c effective_cache_size=256MB`) | 543.7 | 4.7 | 637.6 | 31.9 | 96 |

### Sites per 64 GB (an estimate, as arithmetic)

- wrought_shared_pg: 64 GiB x 0.8 / 31.9 MiB per site (20 sites: one PostgreSQL 543.7 MiB plus 4.7 MiB per engine, divided by 20) = **1,643** (estimate; idle sites of 200 articles; PostgreSQL grows with data and load)
- wordpress_own_stack: 64 GiB x 0.8 / 532.6 MiB idle stack = **98** (estimate; one application and one database per site, idle)
- drupal_own_stack: 64 GiB x 0.8 / 292.0 MiB idle stack = **179** (estimate; one application and one database per site, idle)
- hammerwright_own_stack: 64 GiB x 0.8 / 197.9 MiB idle stack = **264** (estimate; one application and one database per site, idle)

Wall time of the core run per target (s): setup 1,246, hammerwright 965, build:hammerwright 142, build:astro 121, build:hugo+zola 28, wordpress 734, drupal 940, astro 215, phoenix 283, loco 270, page weight 20, shared postgres 1,144; total 102 min.

Wall time of the extended run per target (s): setup 103, hammerwright 10, wordpress 0, drupal 0, page weight 1, rails 553; total 11 min.

Re-run of 2026-11-28 (`--targets`, rerun-2026-11-28-drupal-loco.json; its figures replace the core run's for these): serving drupal, loco, builds none; total 23 min.

Versions: hammerwright engine 0.7.0, astro 7.4.10, wordpress 7.1.3 (php8.4-apache), mariadb 11.4.13, drupal 11.3.23 (php8.4-apache-bookworm), postgres 17.12, node 22.23.5, caddy 2.11.8, wrk alpine 3.22.8 package, rails 8.1.4.1 (Ruby 4.0.10, Puma, production), hugo v0.170.1+withdeploy, zola zola 0.22.1, phoenix phoenix 1.8.17, bandit 1.15.0, ecto_sql 3.14.0, elixir 1.20.5, OTP 29, loco loco-rs 1.3.1, sea-orm 2.0.4, rustc 1.100.0.

### Changes since the 2026-11-27 run

Every figure that moved by more than 5 % against the 2026-11-27 published
numbers (`bench/results/2026-11-27/`), with the likely reason or
"unexplained". Nothing was tuned between the runs. Engine changes since
then: browser caching with tag ETags and hashed asset URLs, TLS and HTTP/3
defaults for the front door, surviving a sleeping database, start on
demand, the form spool, and the removal of SQLite.

| Figure | 2026-11-27 | 2026-11-28 | Change | Likely reason |
| --- | ---: | ---: | ---: | --- |
| Published file (`engine serve`), article, req/s | 9,730 | 9,095 | −6.5 % | Unexplained. CPU per 1,000 pages rose 8 % (0.136 → 0.147 s), so the drop is not noise. Candidate: the serving-path changes since (tag ETags and hashed asset URLs; the page grew 33 bytes). Not isolated. |
| Published file (`engine serve`), listing, req/s | 9,929 | 8,970 | −9.7 % | As above. p99 5.7 → 7.5 ms. |
| Rendered per request, article, req/s | 1,360 | 1,624 | +19.4 % | Within the earlier spread: the first 2026-11-27 re-run measured 1,670, and 1,360 was the low one of the two (the "19 % difference" left open in DECISIONS.md). p99 32.9 → 22.1 ms; CPU per 1,000 1.70 → 1.41 s. |
| Rendered per request, listing, req/s | 1,627 | 1,534 | −5.7 % | Unexplained. |
| Search (publish mode), p99 ms | 32.9 | 36.8 | +11.8 % | Unexplained. Throughput 1,111 → 1,091 (−1.8 %). CPU per 1,000 searches +10.6 % (1.79 → 1.98 s). |
| Phoenix, article, req/s | 731 | 1,022 | +39.9 % | Phoenix's figure moves between runs (1,005 in the 2026-11-27 complete run). Phoenix unchanged. p99 170 → 46 ms. |
| Phoenix, search p99 ms | 133 | 172 | +28.7 % | Unexplained (Phoenix unchanged). |
| Rails (a), article / listing, req/s | 316 / 392 | 351 / 431 | +11 % / +10 % | Unexplained; Rails unchanged. Search p99 422 → 506 ms. |
| WordPress with page cache, article, req/s | 4,116 | 4,514 | +9.7 % | Unexplained; unchanged image. |
| WordPress stock, article / search, req/s | 11.0 / 9.6 | 11.6 / 9.1 | +5.5 % / −5.2 % | Noise at this level (about 120 requests in 10 s). |
| Hammerwright, Markdown build, no copies, s | 0.62 | 0.68 | +8.9 % | Runs 0.61 to 0.71 s; on four cores 0.50 → 0.47 s (−5.3 %). Probably noise. |
| Astro / Zola / Hugo build, s | 6.46 / 0.64 / 0.78 | 6.97 / 0.69 / 0.74 | +8 % / +8 % / −5 % | Noise (tools unchanged; Astro's runs 6.5 to 7.3 s). |
| Edit to published, Hammerwright, median ms | 525 | 167 | −68 % | The 2026-11-27 run had two edits waiting 1.8 and 5.0 s on this VM's disk sync; none did today (runs 0.08 to 0.29 s). |
| Edit to published, WordPress, ms | 467 | 214 | −54 % | Same: two 8.5 s MariaDB sync waits last time. |
| Edit to published, Loco, ms | 248 | 26 | −89 % | Unexplained (Loco unchanged; it measured 15 ms in an earlier run). |
| Edit to published, Astro / Drupal / Rails (b), ms | 3,574 / 752 / 41 | 3,316 / 711 / 44 | −7 % / −5 % / +7 % | Noise. |
| Cold start, Hammerwright, s | 1.73 | 0.87 | −50 % | Likely the start-on-demand work (listen first); not isolated. |
| Cold start, Drupal, s | 0.71 | 0.99 | +40 % | Unexplained (0.84 s in the core run). |
| Cold start, Loco / WordPress, s | 0.78 / 2.59 | 0.58 / 2.77 | −26 % / +7 % | Noise (Loco's core-run 8.5 s was the outlier re-run above). |
| Engine (publish mode) idle, MiB total / anon | 32.9 / 30.4 | 21.7 / 18.8 | −34 % / −38 % | Method: read after 75 s of quiet, not 5 s. Not comparable. |
| Engine (request mode) idle after load, MiB total / anon | 10.7 / 9.2 | 16.2 / 14.9 | +51 % / +62 % | Unexplained, despite the longer wait. A regression. |
| Hammerwright stack (request mode) idle anon, MiB | 16.5 | 18.5 | +12 % | Mostly its database (12.3 → 14.5 MiB anon); unexplained. |
| WordPress / Drupal idle after load, MiB total | 668 / 453 | 547 / 292 | −18 % / −35 % | Method: they also release memory after the load, given time. |
| Drupal idle anon, MiB | 67.6 | 76.7 | +13.5 % | Spread between runs (51.8 in today's core run). |
| WordPress database idle, MiB total | 134.7 | 150.1 | +11 % | Page cache of its data files; unexplained. |
| Smallest workable engine limit, MiB | 12 | 8 | −33 % | Less memory held after the load tests. 8 MiB is the smallest step the harness tries, so the real floor may be lower. |
| Binary, `app` preset, MB | 33.6 | 30.7 | −8.6 % | SQLite removed. The `static` preset is 6.4 → 6.5 MB. |
| CPU per 1,000 pages, Phoenix / WordPress / Rails (a), s | 2.76 / 155.8 / 5.52 | 2.00 / 139.3 / 4.97 | −28 % / −11 % / −10 % | Follows their throughput. |

Wall time of the runs: core 102 minutes (the 2026-11-27 core run took 66;
the settled idle readings add about 2.5 minutes per profiled stack, and its
setup (release build and images) took 21 minutes), extended 11, re-run 23.

## Earlier runs (historical)

The 2026-11-27 run (Hammerwright 0.7 before the v0.8 small-host work, with
SQLite) is in `bench/results/2026-11-27/` (`results.json`,
`results-extended.json` and its two re-runs); its numbers are the left
column of the table above. The sections below are the 0.6 runs of
2026-11-26. They were made when the engine could also use SQLite, and
their "Hammerwright, SQLite" rows are kept as measured, as history: the engine
no longer has SQLite.

### Summary of the 2026-11-26 run (0.6)

- **Pages from files are fast for everyone who serves files.** Hammerwright's
  own server answered 11,966 requests a second for an article page on one
  core; Caddy serving the same files did 3,433, and Caddy serving Astro's
  files 7,062. Rails rendered the page at 343 a second (353 with its
  fragment caching), Drupal from its page cache 62, WordPress 13.
- **Dynamic pages:** search results, a different word each time: Hammerwright
  269 a second on SQLite and 167 on PostgreSQL; Rails 82 (caching does not
  apply to a different query each time); WordPress 11 and Drupal 10.
- **Builds:** 1000 articles from a database now build in about 2.3 s on
  PostgreSQL and 1.8 s on SQLite on one core (0.5: 14.9 s and 9.3 s), and
  from Markdown files in 0.7 s, against Astro's 6.4 to 7.4 s; see
  [cold publish](#cold-publish-of-1000-articles) for every run, including
  the slow ones caused by the disk. Rails, WordPress and Drupal render on
  request and have no build.
- **Where Hammerwright loses:** Caddy served Astro's article page faster than
  Hammerwright's (its file is 30 % smaller; we did not find the rest of the
  gap); an edit reached the public page in about a second on PostgreSQL in
  this run (140 ms in 0.5's run; see [edit to published](#edit-to-published)),
  slower than WordPress (197 ms) and Rails (28 to 42 ms, which simply
  render the next request); and one build in three still stalls for
  seconds on a busy disk.
- **Edit to published:** 21 ms on SQLite, against 197 ms for WordPress,
  768 ms for Drupal and 3.3 s for Astro, which rebuilds the site.
- **Memory:** 25 MB for Hammerwright on SQLite (96 MB with its PostgreSQL),
  about 190 MB for Rails with PostgreSQL (101 MB idle), 324 MB for
  WordPress with MariaDB, 460 MB for Drupal with PostgreSQL.
- **Cold start:** 0.7 s for Hammerwright and Drupal, 1.2 s for WordPress, 3.4 s
  for Rails.
- **Size:** a 5.3 MB binary for static sites, 31 MB with every feature and
  both databases.

### Results of 2026-11-26 (0.6)

Full run of 2026-11-26 (`bench/run.sh`, five systems, Hammerwright 0.6.0).

#### Static page

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Hammerwright (`engine serve`) | 11,966 | 2.6 | 8.7 | 4,602 |
| Hammerwright output via Caddy | 3,433 | 9.0 | 31.2 | 4,602 |
| Astro output via Caddy | 7,062 | 4.5 | 12.4 | 3,198 |
| WordPress | 13 | 2,090 | 7,740 | 72,099 |
| Drupal | 62 | 470 | 3,780 | 17,695 |
| Rails (a) plain rendering | 343 | 93.1 | 101 | 3,749 |
| Rails (b) with Rails caching | 353 | 87.0 | 153 | 3,749 |

#### Dynamic page (search, a different word per request)

| System | Requests/s | p50 ms | p99 ms |
| --- | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | 167 | 188 | 214 |
| Hammerwright, SQLite | 269 | 119 | 133 |
| WordPress | 11 | 2,430 | 6,100 |
| Drupal | 10 | 3,200 | 5,360 |
| Rails (a) plain rendering | 82 | 383 | 439 |
| Rails (b) with Rails caching | 82 | 386 | 423 |

#### Cold publish of 1000 articles

Measured 2026-11-26 with `bench/run.sh pg before|after` (results in
`bench/results/pg-before.json` and `pg-after.json`): Hammerwright 0.5.0 against
0.6.0, the same machine, the same day, the same content, each build three
times on a fresh database, on one core and on four (the engine pins its
render threads, see [limits](#limits)). Times include `docker compose exec`
(about 0.4 s) and, for the database rows, writing the publish and search
indexes. PostgreSQL *stock* is the image's defaults; *tuned* is the
[tuning profile](/docs/databases/#a-tuning-profile) scaled to its 1 GB
container; *socket* is tuned over a Unix socket.

| Build of 1000 articles | 0.5, 1 core | 0.6, 1 core | 0.6, 4 cores | Runs, 0.6, 1 core (s) |
| --- | ---: | ---: | ---: | --- |
| Hammerwright, Markdown files (like Astro) | 0.7 s | 0.7 s | 0.5 s | 0.6, 0.7, 0.7 |
| Hammerwright, SQLite | 9.3 s | 8.9 s (1.8 s without stalls) | 1.6 s | 8.9, 10.8, 1.8 |
| Hammerwright, PostgreSQL stock | 14.9 s | 2.8 s | 2.1 s | 2.2, 10.3, 2.8 |
| Hammerwright, PostgreSQL tuned | 19.6 s | 2.3 s | 2.1 s | 2.3, 2.3, 2.4 |
| Hammerwright, PostgreSQL tuned, socket | 22.7 s | 2.5 s | 2.2 s | 2.2, 9.2, 2.5 |
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
- **Like for like with Astro** (both reading Markdown files): Hammerwright 0.7 s,
  Astro 7.4 s on one core. From a database, Hammerwright now builds faster than
  Astro builds from files.
- **The slow runs (found and fixed after 0.7):** about one build in three
  took 7 to 14 s longer. `engine build --timings` put the whole delay in
  the search-index phase (sometimes the tag index): rewriting the search
  index produced more write-ahead log than PostgreSQL buffers, so the build
  waited for PostgreSQL's WAL `fdatasync`, and on this machine's virtual
  disk a sync takes 2 to 8.6 s dozens of times an hour (an independent
  8 KiB `fdatasync` probe stalled at the same moments). The search index is
  now an unlogged table and the build's tag index commits without waiting
  for the sync, so a build waits for no sync at all. 30 builds before and
  after on the same machine: median 2.98 s → 1.70 s, worst 16.9 s → 3.3 s,
  builds over 5 s 10 → 0 (DECISIONS.md; `bench/run.sh stall` repeats it).
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

#### Edit to published (0.6)

| System | Median | All runs (s) |
| --- | ---: | --- |
| Hammerwright, SQLite | 21 ms | 0.34, 0.02, 0.02, 0.02, 0.02 |
| Hammerwright, PostgreSQL | 1,055 ms | 0.57, 1.23, 1.05, 1.05, 1.09 |
| WordPress | 197 ms | 0.51, 0.19, 0.19, 0.20, 0.22 |
| Drupal | 768 ms | 0.96, 0.79, 0.73, 0.77, 0.74 |
| Astro | 3,340 ms | 3.34, 3.28, 3.35 |
| Rails (a) plain rendering | 28 ms | 0.20, 0.04, 0.02, 0.03, 0.02 |
| Rails (b) with Rails caching | 42 ms | 0.18, 0.05, 0.04, 0.04, 0.04 |

For Rails the number is the time until an edit is visible: it renders on
request, and in (b) the fragment keys change with `updated_at`, so the
next view already shows the edit. Hammerwright on PostgreSQL took about a second
in this run (140 ms in 0.5's): not the disk, as first thought, but the
batch. Every article page lists related articles (`c:article`), so an edit
re-renders all 233 article pages; the batch read the database for every
page and wrote the edited page wherever it fell in path order. Fixed after
0.7: the edited page goes first, large batches render from one content
snapshot, and the batch commits once. Re-measured on the same machine
(15 edits): median 1.73 s → 106 ms, worst 2.7 s → 186 ms.

#### Memory after the load runs (MiB)

| System | Application | Database | Total |
| --- | ---: | ---: | ---: |
| Hammerwright, PostgreSQL | 19.0 | 77.0 | 96.0 |
| Hammerwright, SQLite | 24.8 | — | 24.8 |
| WordPress | 215.2 | 108.5 | 323.7 |
| Drupal | 406.8 | 52.9 | 459.7 |
| Astro (Caddy serving its files) | 18.6 | — | 18.6 |
| Rails (a) plain rendering | 120.7 | 68.4 | 189.1 |
| Rails (b) with Rails caching | 124.9 | 75.2 | 200.1 |

Rails idle (after start and one view): Rails (a) plain rendering 101.2 MiB, Rails (b) with Rails caching 103.2 MiB

The Rails rows are the highest sample during its load runs (its idle
figure is on the line above); the others are taken right after the load.

#### Cold start (container started until a page answers)

| System | Seconds |
| --- | ---: |
| Hammerwright, SQLite | 0.7 |
| WordPress | 1.2 |
| Drupal | 0.7 |
| Rails (a) plain rendering | 3.4 |
| Rails (b) with Rails caching | 3.6 |

#### Size

- Hammerwright binary: 5.3 MB (static preset), 30.6 MB (app preset, both databases)
- Container images: Hammerwright 202.8 MB (Ubuntu base + binary), WordPress 1123.9 MB + MariaDB 450.6 MB, Drupal 1009.4 MB (with Drush) + PostgreSQL 665.2 MB, Astro build: Node 239.1 MB + node_modules 159.6 MB; served by Caddy 91.7 MB
- Output for 200 articles: Hammerwright public/ 1.3 MB, Astro dist/ 0.8 MB

Drupal's page cache answered `X-Drupal-Cache: HIT` for the static page.

## Setup

| | |
| --- | --- |
| Machine | Intel Xeon Silver 4214 (12 cores, 24 threads, 2.2 GHz), 60 GB RAM, Ubuntu 26.04.1, kernel 7.0.0, Docker 29.10.0 |
| CPU placement | the application on core 6, its database on core 7, setup tools on core 8, the load generator on core 9; one core each (see below) |
| Memory caps | 2 GB per application container, 1 GB per database |
| Load | `wrk` (Alpine 3.22.8 package), 1 thread, 32 connections, 3 s warm-up then 10 s measured, 30 s request timeout, over loopback |
| Content | 200 generated articles (about 400 words, 2–3 topics each) from `bench/data/generate.py`, the same for all; 1000 for build times |
| Hammerwright | commit `435d3a3` (engine version string 0.7.0), `app` preset, release build (Rust 1.100.0), the `examples/cms-site` templates, PostgreSQL 17.12 (the only database since 0.8; the 0.6 runs below also used SQLite) |
| WordPress | 7.1.3, `php8.4-apache` image, MariaDB 11.4.13, default theme, pretty permalinks, no caching plugin |
| Drupal | 11.3.23, `php8.4-apache-bookworm` image plus Drush, PostgreSQL 17.12, standard profile (internal page cache on, as installed) |
| Astro | 7.4.10 on Node 22.23.5, static output, Markdown imported with `import.meta.glob` |
| Ruby on Rails | 8.1.4.1 on Ruby 4.0.10 (`ruby:4.0.10-slim-trixie`), an app made by `rails new --database=postgresql` (the generator's defaults; JavaScript, mailers, Action Text/Cable, Active Storage and the deployment extras skipped), `RAILS_ENV=production` (eager loading, precompiled assets, YJIT as Rails enables it, log level info), Puma with the generated `config/puma.rb` (3 threads, no forked workers, the default for one core), PostgreSQL 17.12. Two runs: **(a)** plain rendering; **(b)** Rails caching: the `cache` helper with Russian-doll keys in Solid Cache (Rails 8's default store, in PostgreSQL) and `fresh_when` conditional GETs. The only changes to the generated production config: `force_ssl` off and the host allow-list cleared (plain HTTP on loopback, like everyone). Search with PostgreSQL full text and a GIN index, as its docs describe. Source in `bench/rails/`. |
| Phoenix | 1.8.17 on Elixir 1.20.5 / OTP 29, an app made by `mix phx.new --database postgres` (no asset pipeline, LiveView pages, dashboard, mailer or gettext), Ecto schemas and HEEx templates, `MIX_ENV=prod` OTP release as Phoenix's release guide makes it (`mix phx.gen.release`, `mix phx.digest`, `mix release`, `bin/server`), Bandit, the generated config (pool of 10), PostgreSQL 17.12. Source in `bench/phoenix/`. |
| Loco | 1.3.1 (loco-rs 1.3.1, SeaORM 2.0.4, Rust 1.100.0), an app made by `loco new --db postgres --bg async --assets serverside`, Tera views, SQL through SeaORM raw statements, release build started with `-e production` and the generated production config (pool of 10, JSON logs at info), PostgreSQL 17.12. Its router has no trailing slashes, so its paths are requested without them. Source in `bench/loco/`. |
| Static files | Caddy 2.11.8 serving Hammerwright's `public/` and Astro's `dist/` with the same configuration |

Image digests are in `bench/results/results.json`.

**Why one core each:** the machine boots with `isolcpus` on 22 of its 24
threads, so the scheduler never spreads a process's threads over several
cores. A first run with "4 CPUs" per container in fact gave every system
one core, chosen by chance (an application could share it with its
database). The harness now pins each role to its own core, which is equal
and explicit, but it is a one-core benchmark: on a normal machine PHP-FPM or
Apache workers, Caddy and Hammerwright's server all use more cores, and the
absolute numbers would be higher for everyone.

## What each test does

- **Static page:** `/articles/article-17/` (Drupal: `/article-17`). Hammerwright
  and Astro serve a file written at build time; WordPress renders the post
  with its default theme on every view; Drupal answers from its page cache
  (`X-Drupal-Cache: HIT`); Rails renders the page in (a) and assembles it
  from cached fragments in (b) (the load generator sends no
  `If-None-Match`, so conditional GETs do not apply).
- **Dynamic page:** the search page with a different word on each request
  (a `wrk` Lua script cycling through 20 words), so no page cache can
  answer: Hammerwright's `/search/?q=` (a `request` route over its full-text
  index), WordPress `/?s=`, Drupal `/search/node?keys=`, Rails `/search/?q=`. Astro has no
  server, so no row.
- **Cold publish:** a fresh database (or folder) with 1000 articles, then
  the time of `engine build` (every page, the publish index and the search
  index) or `astro build`, three times. WordPress and Drupal do not
  pre-render pages, and Rails has no static build, so they have no row
  ("not applicable").
- **Edit to published:** change an article's title through each system's
  API (Hammerwright's JSON API, the WordPress REST API with an application
  password, Drupal's JSON:API, a token-protected JSON update in the Rails
  app), then poll the public page every 5 ms until
  the new title shows. For Astro: edit the Markdown file, run `astro build`,
  poll. Five edits (Astro three); the median is shown.
- **Memory:** each stack's containers from cgroup v2 (`memory.current`
  and anonymous memory), sampled every 100 ms during the loads for the
  peaks. *Idle* and *idle after* are read once the stack has settled: 60 s
  with no requests, then every 15 s until two readings agree within 2 %
  (from 2026-11-28; earlier runs read 5 and 10 s after the last request).
  The 0.6 runs used `docker stats` right after the load runs.
- **Cold start:** stop a server's container, start it, and time it until
  the article page answers (Hammerwright, WordPress, Drupal, Phoenix, Loco,
  Rails).

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
(WordPress's default theme sends 72 KB of HTML, Drupal's 18 KB, Hammerwright's
4.5 KB, Astro's 3.2 KB): that is part of what each system costs.

## Limits

- **One machine, loopback.** No network latency, no TLS, no compression;
  the load generator shares the machine (on its own core). Real traffic
  arrives slower and spread out.
- **One core per system,** for the reason above. More cores would change
  the ratios: PHP scales with workers, Caddy and Hammerwright with threads.
- **A virtual disk whose sync can stall for seconds.** On 2026-11-28 the
  disk probe beside the builds saw one 6.6 s `fdatasync`; earlier runs saw
  edits and builds wait 2 to 8.6 s for one. Hammerwright's build no longer waits
  for a sync, but every system's commits (edits) can.
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
  CDN serves cached pages at the CDN's speed. Hammerwright's point is that its
  published pages are files from the start, without a cache to warm or
  purge.
- **The edit test measures each system's own path** (Hammerwright: publish
  through the change feed; WordPress and Drupal: render on the next view;
  Astro: full rebuild); it does not include a CDN purge.

## Guards in CI

Two scripts keep the engine from getting heavier or slower by accident;
both run in `host.yml` and locally (Docker, PostgreSQL in
`wrought-test-postgres`):

| Script | Measures | Fails when |
| --- | --- | --- |
| `scripts/check-memory.sh` | resident memory of the `cms` preset on PostgreSQL, idle after 120 requests | above 28 MiB |
| `scripts/check-speed.sh` | requests a second, engine on one CPU, wrk on another, 32 connections, 5 s each: a published article file, the article rendered per request, search | below 4,400 / 750 / 1,550 |

The speed floors are 62 % of the mean of five runs of the script on the
benchmark machine (7,160 / 1,220 / 2,530 a second), enough margin that
noise does not trip them. Three runs on 2026-11-28 measured 7,415–7,522,
1,131–1,268 and 2,466–2,487 a second, and the memory check measured
22,056 KiB (4,264 anonymous). They are not benchmark results: the example
site has four articles. A slower CI runner sets its own floors with
`HAMMERWRIGHT_SPEED_FLOOR_FILE`, `HAMMERWRIGHT_SPEED_FLOOR_RENDER` and
`HAMMERWRIGHT_SPEED_FLOOR_SEARCH`.

## Run it yourself

```sh
bench/run.sh                    # core group, about 100 minutes; needs Docker and network
bench/run.sh --group extended   # Rails, about 11 minutes
bench/run.sh stall 25 1         # 25 PostgreSQL builds with wait events and disk syncs
python3 bench/report.py         # the tables above, from results.json and results-extended.json
bench/run.sh build              # only the cold builds (Markdown, PostgreSQL, Astro)
BENCH_ENGINE=path/to/engine bench/run.sh pg <label>   # builds and search on stock,
                                # tuned and socket PostgreSQL for one binary
                                # (results/pg-<label>.json; BENCH_SITE_REV=<git rev>
                                # for an older engine's example site)
BENCH_APP_CPU=2 BENCH_DB_CPU=3 BENCH_TOOL_CPU=4 BENCH_LOAD_CPU=5 bench/run.sh
```

The run removes its containers, volumes and working folder when it ends
(`BENCH_KEEP=1` keeps them); `BENCH_RMI=all` also removes the pulled images.
