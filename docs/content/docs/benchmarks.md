+++
title = "Benchmarks"
description = "Wrought against WordPress, Drupal, Astro, Phoenix, Loco, Rails, Hugo and Zola on the same content, on one machine: throughput, latency, build and publish times, memory and size, with the method and its limits."
weight = 19
+++

Measured with `bench/run.sh` (one command; see
[`bench/`](https://github.com/kovirlabs/wrought/tree/main/bench)); the
latest complete run is first, earlier runs follow. Read the
[limits](#limits) before quoting any number.

## Results of 2026-11-27 (complete run; Wrought, Phoenix and Loco re-run)

Two runs of `bench/run.sh`, each written by the run itself: `--group core`
(Wrought on PostgreSQL, WordPress, Drupal, Astro, Phoenix and Loco for
serving; Wrought, Astro, Hugo and Zola for builds; 66 minutes) into
`bench/results/results.json`, and `--group extended` (Rails, Wrought on
SQLite; 7 minutes) into `bench/results/results-extended.json`. After the
work on request rendering and search the same day, the affected targets
were measured again with `--targets` and merged in with `bench/merge.py`
(each figure from its own file; `reruns` in results.json lists them):
Wrought, Phoenix and Loco (`results/rerun-1-wrought-phoenix-loco.json`),
then Wrought alone after the memory fix (`results/rerun-2-wrought.json`).
The tables below are `python3 bench/report.py` of the merged files. Same
machine and limits as the earlier runs: one core per role, 2 GB per
application, 1 GB per database container.

**Which Wrought figure is which.** *Published file* rows are pages
written ahead of time and served as files (by `engine serve` or Caddy):
compare them with Astro, Hugo and Zola and with page caches. *Rendered per
request* rows (render dial `request`, PostgreSQL) build the page on every
request like Phoenix, Loco, Rails, WordPress and Drupal do: compare those
with each other. Wrought's search page is always rendered per request.

**Where Wrought loses, plainly:**

- **Rendered per request, Loco is still a little faster and spends less
  CPU.** Article page: Loco 1,801 requests a second, Wrought 1,670 in the
  first re-run and 1,360 in the second (the same binary but for the memory
  change; the spread between the two runs is the bench's), Phoenix 731 in
  this run (1,005 in the complete run: its figure moves more between runs
  than the others'). CPU per 1,000 articles: Loco 1.3 s, Wrought 1.3 to
  1.7 s, Phoenix 2.0 to 2.8 s. Listing: Loco 2,778, Wrought 1,627, Phoenix
  1,354. Before the work of 2026-11-27 Wrought rendered 566 articles and
  526 listings a second (DECISIONS.md, "Faster request rendering and
  search": a page's queries now share one connection, related items are
  two statements, items' rendered Markdown is kept).
- **Search: Phoenix and Loco still answer more searches** (1,735 and 2,202
  a second against Wrought's 1,111; CPU 1.3 and 1.1 s against 1.8 s per
  1,000), and their search does less: the 20 newest matches, where
  Wrought's ranks every match, cuts a highlighted extract for each hit on
  the page and counts the total. Before: 167 a second. One statement per
  page now, extracts cut in the engine instead of by `ts_headline`; the
  database's one core is the limit (ranking about 198 matches per query on
  this data).
- **Hugo and Zola build 1000 Markdown articles faster when Wrought writes
  its compressed copies:** Wrought 1.15 s, Hugo 0.78 s, Zola 0.64 s (each
  including about 0.35 s of `docker exec`). Without the copies
  (`ENGINE_BUILD_PRECOMPRESS=0`) Wrought takes 0.62 s; the copies cost
  about 0.5 s on one core. Hugo and Zola
  write no compressed copies, search index or tag index. On four cores:
  Wrought 0.59 s with copies, 0.50 s without.
- **Loco idles in less memory than Wrought's engine:** 4.5 MiB (2.8
  MiB anonymous) against 5.7 MiB for the engine freshly started and 33 MiB
  (30 anonymous) for the publish-mode engine after the whole sequence of
  load tests, measured 10 s after the last one. That retained memory was
  freed memory kept by the allocator in up to 192 arenas (glibc counts the
  host's 24 CPUs, not the container's one); the engine now caps its arenas
  at eight per usable CPU and hands free memory back once it has been idle
  (DECISIONS.md). The smallest workable limit for the engine container is
  12 MiB again (16 MiB in the complete run and the first re-run: at 12 MiB
  the limit was below what the engine already held, and the kernel killed
  it).
- **Edit to published on PostgreSQL** took 134 ms (median) in the first
  re-run and 525 ms in the second, where two of five edits waited 1.8 and
  5.0 s: this VM's disk sync (see the stall entry), which the commit of an
  edit must wait for. Phoenix 20 ms, Loco 15 to 248 ms between runs.
- **Edit to published on SQLite took 108 ms** (median of five), against
  21 ms in the 0.6 run; Phoenix (20 ms), Loco (15 ms) and Rails (32 ms)
  just render the next request. Not investigated.
- Caddy serving Astro's files still outruns Caddy serving Wrought's (7,038
  against 3,979 requests a second without `Accept-Encoding`).

**Where it does not:** published pages from `engine serve` (9,730 a
second, 0.13 s of CPU per 1,000) and the 20-sites-on-one-PostgreSQL
density are unchanged in kind. The PostgreSQL build no longer stalls:
2.21, 2.22 and 2.28 s in the second re-run. The stall that remained in this stack (5–8 s in
the search-index phase in 5 of 20 builds) was the search index's commit
waiting for a write-ahead-log sync; it no longer does (`bench/run.sh
stall`: worst 9.6 → 2.9 s over 25 builds, DECISIONS.md).

**Memory figures** are sums over a target's containers from cgroup v2.
`memory.current` includes the page cache of the files a container read
(a database's data files count there), so the anonymous memory (what the
processes allocated) is shown beside it, and application and database
separately. **Page caches:** WordPress is shown stock and with Apache's
`mod_cache` (disk store, anonymous pages, 10 minutes,
`CacheIgnoreNoLastMod On`, `CacheIgnoreHeaders Set-Cookie`, quick
handler); Drupal's stock install already caches pages. Phoenix serves its
stylesheet gzipped (`mix phx.digest`), Loco and Rails serve it
uncompressed, as generated. Two of WordPress's five edits took 8.5 s:
MariaDB's commit waited for this VM's slow disk sync (see the stall
entry); they are shown, not dropped.

#### Article page

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Wrought, published file (`engine serve`) | 9,730 | 3.3 | 5.8 | 4,602 |
| Wrought, published file via Caddy | 3,979 | 8.0 | 21.1 | 4,602 |
| Astro output via Caddy | 7,038 | 4.6 | 12.5 | 3,198 |
| WordPress (stock) | 11 | 2,390 | 7,490 | 72,099 |
| Drupal (stock: its page cache is on) | 60 | 488 | 4,070 | 17,695 |
| WordPress with a page cache (Apache mod_cache) | 4,116 | 4.7 | 558 | n/a |
| Wrought, rendered per request (render dial `request`) | 1,360 | 23.3 | 32.9 | n/a |
| Phoenix (rendered per request) | 731 | 43.2 | 170 | 3,764 |
| Loco (rendered per request) | 1,801 | 17.4 | 30.0 | 3,757 |
| Rails (a) plain rendering | 316 | 93.8 | 167 | 3,749 |
| Rails (b) with Rails caching | 373 | 85.7 | 94.0 | 3,749 |

#### Listing page (/articles/, 10 per page)

| System | Requests/s | p50 ms | p99 ms | Page bytes |
| --- | ---: | ---: | ---: | ---: |
| Wrought, published file (`engine serve`) | 9,929 | 3.2 | 5.7 | 4,907 |
| Wrought, rendered per request (render dial `request`) | 1,627 | 19.5 | 23.0 | n/a |
| Phoenix (rendered per request) | 1,354 | 23.4 | 32.9 | 3,715 |
| Loco (rendered per request) | 2,778 | 11.3 | 16.3 | 3,714 |
| Rails (a) plain rendering | 392 | 75.9 | 140 | 3,710 |
| Rails (b) with Rails caching | 394 | 76.5 | 141 | 3,710 |

#### Dynamic page (search, a different word per request)

| System | Requests/s | p50 ms | p99 ms |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 1,111 | 28.5 | 32.9 |
| WordPress (stock) | 10 | 2,640 | 4,500 |
| Drupal (stock: its page cache is on) | 10 | 3,650 | 4,260 |
| Wrought, rendered per request (render dial `request`) | 1,100 | 28.8 | 33.5 |
| Phoenix (rendered per request) | 1,735 | 18.1 | 133 |
| Loco (rendered per request) | 2,202 | 13.7 | 268 |
| Wrought, SQLite | 269 | 119 | 133 |
| Rails (a) plain rendering | 84 | 380 | 422 |
| Rails (b) with Rails caching | 82 | 391 | 428 |

#### Cold build of 1000 articles (one core)

| System | Median s | Runs (s) | Peak memory MiB | Output MB | 4 cores, median s |
| --- | ---: | --- | ---: | ---: | ---: |
| Wrought, Markdown files | 1.15 | 1.14, 1.15, 1.18 | 21.5 | 9.1 | 0.59 |
| Wrought, Markdown files, no compressed copies | 0.62 | 0.62, 0.62, 0.62 | 18.2 | 5.4 | 0.50 |
| Wrought, PostgreSQL | 2.22 | 2.21, 2.22, 2.28 | 42.0 | 10.6 | 1.67 |
| Astro | 6.46 | 9.06, 6.46, 6.45 | 373.2 | 3.6 | 6.48 |
| Hugo | 0.78 | 0.85, 0.76, 0.78 | 65.6 | 7.7 | n/a |
| Zola | 0.64 | 0.64, 0.64, 0.62 | 150.6 | 3.7 | n/a |

Wrought, Markdown files, phases of a median run: load templates 43 ms, static files 9 ms, render and write pages 711 ms.

Wrought, PostgreSQL, phases of a median run: connect 17 ms, open site 15 ms, search index 722 ms, build 1,010 ms, tag index 130 ms, (outside the phases) 14 ms.

One article changed, then built again: Hugo 0.73 s (full rebuild (no incremental build)); Zola 0.69 s (full rebuild (no incremental build))

#### Edit to published

| System | Median | All runs (s) |
| --- | ---: | --- |
| Wrought, PostgreSQL | 525 ms | 1.82, 5.03, 0.53, 0.36, 0.33 |
| WordPress (stock) | 467 ms | 0.47, 8.49, 0.29, 0.21, 8.65 |
| Drupal (stock: its page cache is on) | 752 ms | 1.09, 0.72, 0.74, 0.75, 0.87 |
| Astro | 3,574 ms | 3.58, 3.57, 3.53 |
| Phoenix (rendered per request) | 20 ms | 0.02, 0.02, 0.02, 0.02, 0.02 |
| Loco (rendered per request) | 248 ms | 0.46, 0.38, 0.17, 0.25, 0.16 |
| Wrought, SQLite | 108 ms | 0.07, 0.11, 0.11, 0.10, 0.12 |
| Rails (a) plain rendering | 32 ms | 0.17, 0.05, 0.03, 0.03, 0.03 |
| Rails (b) with Rails caching | 41 ms | 0.17, 0.11, 0.04, 0.04, 0.04 |

#### Whole-stack memory and CPU (cgroup accounting, MiB)

| System | Idle | Light load | Peak, load test | Idle after | CPU s / 1,000 pages | CPU s / 1,000 searches |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Wrought, PostgreSQL | 201.2 | 203.0 | 202.2 | 203.6 | 0.136 | 1.786 |
| WordPress (stock) | 502.8 | 507.5 | 698.4 | 667.6 | 155.836 | 141.770 |
| Drupal (stock: its page cache is on) | 280.0 | 284.1 | 412.0 | 452.5 | 35.689 | 244.224 |
| Astro | 12.1 | 15.9 | 19.7 | 19.2 | 0.183 | n/a |
| WordPress with a page cache (Apache mod_cache) | 440.1 | 447.9 | 461.5 | 666.3 | 0.284 | 153.921 |
| Wrought, rendered per request (render dial `request`) | 158.8 | 162.4 | 172.8 | 175.0 | 1.697 | 2.067 |
| Phoenix (rendered per request) | 233.8 | 236.6 | 250.3 | 258.6 | 2.758 | 1.347 |
| Loco (rendered per request) | 79.5 | 85.1 | 106.1 | 85.9 | 1.311 | 1.051 |
| Rails (a) plain rendering | 237.9 | 250.1 | 255.5 | 265.0 | 5.520 | 20.289 |
| Rails (b) with Rails caching | 241.5 | 251.7 | 263.0 | 271.9 | 4.560 | 20.518 |

Containers counted: Wrought, PostgreSQL: engine, wrought-db; WordPress (stock): wp, wp-db; Drupal (stock: its page cache is on): drupal, drupal-db; Astro: caddy; WordPress with a page cache (Apache mod_cache): wp, wp-db; Wrought, rendered per request (render dial `request`): engine, wrought-db; Phoenix (rendered per request): phoenix, phoenix-db; Loco (rendered per request): loco, loco-db; Rails (a) plain rendering: rails, rails-db; Rails (b) with Rails caching: rails, rails-db. `memory.current` includes the page cache the containers' files occupy; the anonymous memory alone: Wrought, PostgreSQL 56.8, WordPress (stock) 184.5, Drupal (stock: its page cache is on) 67.6, Astro 10.9, WordPress with a page cache (Apache mod_cache) 126.7, Wrought, rendered per request (render dial `request`) 16.5, Phoenix (rendered per request) 127.2, Loco (rendered per request) 9.4, Rails (a) plain rendering 104.7, Rails (b) with Rails caching 108.2 MiB idle.

#### Application and database memory, separately (MiB; cgroup total / anonymous)

| System | Container | Idle | Peak, load test (article) | Peak, search |
| --- | --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | engine | 32.9 / 30.4 | 33.7 / 30.6 | 39.8 / 36.7 |
| Wrought, PostgreSQL | wrought-db | 168.3 / 26.5 | 168.6 / 26.5 | 169.6 / 27.7 |
| WordPress (stock) | wp | 368.1 / 76.2 | 564.3 / 256.3 | 596.6 / 288.8 |
| WordPress (stock) | wp-db | 134.7 / 108.3 | 135.5 / 108.6 | 135.0 / 109.0 |
| Drupal (stock: its page cache is on) | drupal | 125.0 / 63.2 | 205.5 / 129.3 | 423.6 / 347.0 |
| Drupal (stock: its page cache is on) | drupal-db | 155.0 / 4.4 | 219.3 / 59.1 | 272.3 / 104.4 |
| Astro | caddy | 12.1 / 10.9 | 19.7 / 18.0 | n/a |
| WordPress with a page cache (Apache mod_cache) | wp | 306.8 / 17.7 | 328.2 / 27.4 | 569.3 / 261.2 |
| WordPress with a page cache (Apache mod_cache) | wp-db | 133.4 / 109.0 | 133.4 / 109.0 | 134.1 / 109.3 |
| Wrought, rendered per request (render dial `request`) | engine | 5.7 / 4.2 | 10.2 / 8.2 | 12.7 / 10.6 |
| Wrought, rendered per request (render dial `request`) | wrought-db | 153.1 / 12.3 | 162.9 / 20.8 | 165.3 / 23.2 |
| Phoenix (rendered per request) | phoenix | 142.3 / 106.7 | 154.4 / 116.3 | 162.2 / 123.9 |
| Phoenix (rendered per request) | phoenix-db | 91.5 / 20.5 | 97.1 / 23.1 | 103.5 / 28.6 |
| Loco (rendered per request) | loco | 4.5 / 2.8 | 7.4 / 5.1 | 7.6 / 5.2 |
| Loco (rendered per request) | loco-db | 75.0 / 6.6 | 98.9 / 27.8 | 102.4 / 27.6 |
| Rails (a) plain rendering | rails | 104.7 / 98.3 | 117.7 / 111.1 | 126.8 / 119.8 |
| Rails (a) plain rendering | rails-db | 133.2 / 6.4 | 138.6 / 12.0 | 138.9 / 11.8 |
| Rails (b) with Rails caching | rails | 106.5 / 99.8 | 119.6 / 112.9 | 129.1 / 122.5 |
| Rails (b) with Rails caching | rails-db | 135.0 / 8.4 | 143.6 / 15.3 | 145.4 / 17.0 |

The total includes the page cache of the files a container read (for a database, its data files); the anonymous figure is what its processes allocated.

| System | CPU s / 1,000 articles | CPU s / 1,000 listings | CPU s / 1,000 searches |
| --- | ---: | ---: | ---: |
| Wrought, PostgreSQL | 0.136 | 0.134 | 1.786 |
| WordPress (stock) | 155.836 | n/a | 141.770 |
| Drupal (stock: its page cache is on) | 35.689 | n/a | 244.224 |
| Astro | 0.183 | n/a | n/a |
| WordPress with a page cache (Apache mod_cache) | 0.284 | n/a | 153.921 |
| Wrought, rendered per request (render dial `request`) | 1.697 | 1.365 | 2.067 |
| Phoenix (rendered per request) | 2.758 | 1.442 | 1.347 |
| Loco (rendered per request) | 1.311 | 0.902 | 1.051 |
| Rails (a) plain rendering | 5.520 | 4.432 | 20.289 |
| Rails (b) with Rails caching | 4.560 | 4.398 | 20.518 |

#### Cold start (container started until the article page answers)

| System | Seconds |
| --- | ---: |
| Phoenix (rendered per request) | 2.66 |
| Loco (rendered per request) | 0.78 |
| Wrought, PostgreSQL | 1.73 |
| WordPress (stock) | 2.59 |
| Drupal (stock: its page cache is on) | 0.71 |
| Rails (a) plain rendering | 2.94 |
| Rails (b) with Rails caching | 3.08 |

#### Smallest memory limit that still serves the load test

| System | Application container | Database container |
| --- | ---: | ---: |
| Wrought, PostgreSQL | 12 MiB | 96 MiB |
| WordPress (stock) | 384 MiB | 128 MiB |
| Drupal (stock: its page cache is on) | 256 MiB | 192 MiB |

#### Page weight, first visit, default theme (headless Chromium)

| System | Page | Requests | Transferred KB | Uncompressed KB | HTML | CSS | JS | Images | Fonts |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Phoenix (rendered per request) | home | 2 | 13.3 | 67.3 | 0.7 | 12.6 | 0 | 0 | 0 |
| Phoenix (rendered per request) | listing | 2 | 13.8 | 69.5 | 1.2 | 12.6 | 0 | 0 | 0 |
| Phoenix (rendered per request) | article | 2 | 14.2 | 69.5 | 1.6 | 12.6 | 0 | 0 | 0 |
| Loco (rendered per request) | home | 2 | 67.3 | 67.3 | 1.5 | 65.7 | 0 | 0 | 0 |
| Loco (rendered per request) | listing | 2 | 69.5 | 69.5 | 3.7 | 65.7 | 0 | 0 | 0 |
| Loco (rendered per request) | article | 2 | 69.5 | 69.5 | 3.8 | 65.7 | 0 | 0 | 0 |
| Wrought, PostgreSQL | home | 4 | 24.9 | 104.1 | 1.2 | 12.1 | 11.6 | 0 | 0 |
| Wrought, PostgreSQL | listing | 4 | 25.2 | 105.6 | 1.4 | 12.1 | 11.6 | 0 | 0 |
| Wrought, PostgreSQL | article | 4 | 25.5 | 105.3 | 1.8 | 12.1 | 11.6 | 0 | 0 |
| Astro | home | 2 | 66.9 | 66.9 | 1.1 | 65.7 | 0 | 0 | 0 |
| Astro | listing | 2 | 67.0 | 67.0 | 1.3 | 65.7 | 0 | 0 | 0 |
| Astro | article | 2 | 68.9 | 68.9 | 3.2 | 65.7 | 0 | 0 | 0 |
| WordPress (stock) | home | 5 | 90.7 | 200.0 | 20.8 | 0 | 16.3 | 0 | 53.6 |
| WordPress (stock) | listing | 5 | 91.2 | 201.8 | 21.4 | 0 | 16.3 | 0 | 53.6 |
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

#### 20 Wrought sites on one PostgreSQL server

| PostgreSQL | PostgreSQL MiB | Engine each MiB | Total MiB | Per site MiB | Connections |
| --- | ---: | ---: | ---: | ---: | ---: |
| defaults (`-c max_connections=200`) | 791.8 | 4.7 | 884.9 | 44.2 | 100 |
| small (`-c shared_buffers=32MB -c work_mem=2MB -c maintenance_work_mem=16MB -c max_connections=200 -c wal_buffers=1MB -c effective_cache_size=256MB`) | 542.9 | 4.6 | 635.3 | 31.8 | 99 |

#### Sites per 64 GB (an estimate, as arithmetic)

- wrought_shared_pg: 64 GiB x 0.8 / 31.8 MiB per site (20 sites: one PostgreSQL 542.9 MiB plus 4.6 MiB per engine, divided by 20) = **1,648** (estimate; idle sites of 200 articles; PostgreSQL grows with data and load)
- wordpress_own_stack: 64 GiB x 0.8 / 502.8 MiB idle stack = **104** (estimate; one application and one database per site, idle)
- drupal_own_stack: 64 GiB x 0.8 / 280.0 MiB idle stack = **187** (estimate; one application and one database per site, idle)
- wrought_own_stack: 64 GiB x 0.8 / 201.2 MiB idle stack = **260** (estimate; one application and one database per site, idle)

Wall time of the core run per target (s): setup 80, wrought 724, build:wrought 145, build:astro 111, build:hugo+zola 14, wordpress 526, drupal 893, astro 78, phoenix 155, loco 126, page weight 19, shared postgres 1,062; total 66 min.

Wall time of the extended run per target (s): setup 96, wrought 14, wordpress 0, drupal 0, page weight 1, wrought_sqlite 37, rails 272; total 7 min.

Re-run of 2026-11-27 (`--targets`, rerun-1-wrought-phoenix-loco.json; its figures replace the core run's for these): serving wrought, phoenix, loco, builds wrought; total 55 min.

Re-run of 2026-11-27 (`--targets`, rerun-2-wrought.json; its figures replace the core run's for these): serving wrought, builds wrought; total 51 min.

Versions: wrought engine 0.7.0, astro 7.4.10, wordpress 7.1.3 (php8.4-apache), mariadb 11.4.13, drupal 11.3.23 (php8.4-apache-bookworm), postgres 17.12, node 22.23.5, caddy 2.11.8, wrk alpine 3.22.8 package, rails 8.1.4.1 (Ruby 4.0.10, Puma, production), hugo v0.170.1+withdeploy, zola zola 0.22.1, phoenix phoenix 1.8.17, bandit 1.15.0, ecto_sql 3.14.0, elixir 1.20.5, OTP 29, loco loco-rs 1.3.1, sea-orm 2.0.4, rustc 1.100.0.

Earlier runs follow.

## Summary of the 2026-11-26 run

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
in this run (140 ms in 0.5's): not the disk, as first thought, but the
batch. Every article page lists related articles (`c:article`), so an edit
re-renders all 233 article pages; the batch read the database for every
page and wrote the edited page wherever it fell in path order. Fixed after
0.7: the edited page goes first, large batches render from one content
snapshot, and the batch commits once. Re-measured on the same machine
(15 edits): median 1.73 s → 106 ms, worst 2.7 s → 186 ms.

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
| Phoenix | 1.8.17 on Elixir 1.20.5 / OTP 29, an app made by `mix phx.new --database postgres` (no asset pipeline, LiveView pages, dashboard, mailer or gettext), Ecto schemas and HEEx templates, `MIX_ENV=prod` OTP release as Phoenix's release guide makes it (`mix phx.gen.release`, `mix phx.digest`, `mix release`, `bin/server`), Bandit, the generated config (pool of 10), PostgreSQL 17.12. Source in `bench/phoenix/`. |
| Loco | 1.3.1 (loco-rs 1.3.1, SeaORM 2.0.4, Rust 1.100.0), an app made by `loco new --db postgres --bg async --assets serverside`, Tera views, SQL through SeaORM raw statements, release build started with `-e production` and the generated production config (pool of 10, JSON logs at info), PostgreSQL 17.12. Its router has no trailing slashes, so its paths are requested without them. Source in `bench/loco/`. |
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
noise does not trip them. They are not benchmark results: the example
site has four articles. A slower CI runner sets its own floors with
`WROUGHT_SPEED_FLOOR_FILE`, `WROUGHT_SPEED_FLOOR_RENDER` and
`WROUGHT_SPEED_FLOOR_SEARCH`.

## Run it yourself

```sh
bench/run.sh                    # core group, about 66 minutes; needs Docker and network
bench/run.sh --group extended   # Rails and Wrought on SQLite, about 7 minutes
bench/run.sh stall 25 1         # 25 PostgreSQL builds with wait events and disk syncs
python3 bench/report.py         # the tables above, from results.json and results-extended.json
bench/run.sh build              # only the cold builds (Markdown, SQLite, PostgreSQL, Astro)
BENCH_ENGINE=path/to/engine bench/run.sh pg <label>   # builds and search on stock,
                                # tuned and socket PostgreSQL for one binary
                                # (results/pg-<label>.json; BENCH_SITE_REV=<git rev>
                                # for an older engine's example site)
BENCH_APP_CPU=2 BENCH_DB_CPU=3 BENCH_TOOL_CPU=4 BENCH_LOAD_CPU=5 bench/run.sh
```

The run removes its containers, volumes and working folder when it ends
(`BENCH_KEEP=1` keeps them); `BENCH_RMI=all` also removes the pulled images.
