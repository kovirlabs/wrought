#!/usr/bin/env python3
"""Markdown tables from bench/results/results.json (for the docs page).

    python3 bench/report.py > /tmp/tables.md

Sections whose data a run did not measure (targets not selected) are left
out.
"""
import json
import statistics
from pathlib import Path

r = json.loads((Path(__file__).parent / "results/results.json").read_text())


def f(x, nd=0):
    return "n/a" if x is None else f"{x:,.{nd}f}"


def ms(x):
    return "n/a" if x is None else (f"{x:.1f}" if x < 100 else f"{x:,.0f}")


names = {
    "wrought_engine": "Wrought, published file (`engine serve`)",
    "wrought_caddy": "Wrought, published file via Caddy",
    "wrought_request": "Wrought, rendered per request (render dial `request`)",
    "astro_caddy": "Astro output via Caddy",
    "wordpress": "WordPress (stock)",
    "wordpress_cached": "WordPress with a page cache (Apache mod_cache)",
    "drupal": "Drupal (stock: its page cache is on)",
    "wrought_postgres": "Wrought, PostgreSQL",
    "wrought_sqlite": "Wrought, SQLite",
    "astro": "Astro",
    "hugo": "Hugo",
    "zola": "Zola",
    "wrought_markdown": "Wrought, Markdown files",
    "rails_plain": "Rails (a) plain rendering",
    "rails_cached": "Rails (b) with Rails caching",
    "wrought": "Wrought, PostgreSQL",
}
nm = lambda k: names.get(k, k)  # noqa: E731

if r.get("static"):
    print("### Article page\n")
    print("| System | Requests/s | p50 ms | p99 ms | Page bytes |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for k, v in r["static"].items():
        print(f"| {nm(k)} | {f(v['rps'])} | {ms(v['p50_ms'])} | {ms(v['p99_ms'])} | "
              f"{f(r.get('page_bytes', {}).get(k))} |")
if r.get("dynamic"):
    print("\n### Dynamic page (search, a different word per request)\n")
    print("| System | Requests/s | p50 ms | p99 ms |")
    print("| --- | ---: | ---: | ---: |")
    for k, v in r["dynamic"].items():
        print(f"| {nm(k)} | {f(v['rps'])} | {ms(v['p50_ms'])} | {ms(v['p99_ms'])} |")
if r.get("cold_build"):
    print("\n### Cold build of 1000 articles (one core)\n")
    print("| System | Median s | Runs (s) | Peak memory MiB | Output MB | 4 cores, median s |")
    print("| --- | ---: | --- | ---: | ---: | ---: |")
    four = r.get("cold_build_4_cores", {})
    for k, v in r["cold_build"].items():
        f4 = four.get(k)
        print(f"| {nm(k)} | {v['seconds']:.2f} | {', '.join(f'{x:.2f}' for x in v['runs'])} | "
              f"{f(v.get('peak_mib'), 1)} | {f(v.get('output_mb'), 1)} | "
              f"{f(f4['seconds'], 2) if f4 else 'n/a'} |")
    for k in ("wrought_markdown", "wrought_postgres"):
        v = r["cold_build"].get(k)
        if v and v.get("phases_ms"):
            order = sorted(range(len(v["runs"])), key=lambda i: v["runs"][i])
            p = v["phases_ms"][order[len(order) // 2]]
            parts = ", ".join(f"{n} {t:,.0f} ms" for n, t in p.items()
                              if isinstance(t, float) and n not in ("total",) and t >= 5)
            print(f"\n{nm(k)}, phases of a median run: {parts}.")
if r.get("incremental_build"):
    print("\nOne article changed, then built again: " + "; ".join(
        f"{nm(k)} {v['seconds']:.2f} s ({v['what']})" for k, v in r["incremental_build"].items()))
if r.get("edit_s"):
    print("\n### Edit to published\n")
    print("| System | Median | All runs (s) |")
    print("| --- | ---: | --- |")
    for k, v in r["edit_s"].items():
        print(f"| {nm(k)} | {statistics.median(v) * 1000:,.0f} ms | "
              f"{', '.join(f'{x:.2f}' for x in v)} |")

res = r.get("resources", {})
if res:
    print("\n### Whole-stack memory and CPU (cgroup accounting, MiB)\n")
    print("| System | Idle | Light load | Peak, load test | Idle after | "
          "CPU s / 1,000 pages | CPU s / 1,000 searches |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for k, v in res.items():
        print(f"| {nm(k)} | {f(v['idle']['current_mib'], 1)} | "
              f"{f(v['light_peak']['current_mib'], 1)} | {f(v['load_peak']['current_mib'], 1)} | "
              f"{f(v['idle_after']['current_mib'], 1)} | {f(v.get('cpu_s_per_1000_page'), 3)} | "
              f"{f(v.get('cpu_s_per_1000_search'), 3)} |")
    print("\nContainers counted: " + "; ".join(
        f"{nm(k)}: {', '.join(c.replace('wrought-bench-', '') for c in v['containers'])}"
        for k, v in res.items()) + ". `memory.current` includes the page cache the "
        "containers' files occupy; the anonymous memory alone: " + ", ".join(
        f"{nm(k)} {f(v['idle']['anon_mib'], 1)}" for k, v in res.items()) + " MiB idle.")
lim = r.get("min_memory_limit", {})
if lim:
    print("\n### Smallest memory limit that still serves the load test\n")
    print("| System | Application container | Database container |")
    print("| --- | ---: | ---: |")
    for k, v in lim.items():
        if "app" not in v:
            print(f"| {nm(k)} | failed: {v.get('error')} | |")
            continue
        print(f"| {nm(k)} | {f(v['app']['min_mib'])} MiB | {f(v['db']['min_mib'])} MiB |")
pw = r.get("page_weight", {})
if pw:
    print("\n### Page weight, first visit, default theme (headless Chromium)\n")
    print("| System | Page | Requests | Transferred KB | Uncompressed KB | HTML | CSS | JS | "
          "Images | Fonts |")
    print("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for t, pages in pw.items():
        for page, w in pages.items():
            if not w:
                continue
            k = w["kinds"]
            kb = lambda n: f"{k[n]['transferred'] / 1000:.1f}" if n in k else "0"  # noqa: E731
            print(f"| {nm(t)} | {page} | {w['requests']} | {w['transferred'] / 1000:.1f} | "
                  f"{w['uncompressed'] / 1000:.1f} | {kb('html')} | {kb('css')} | {kb('js')} | "
                  f"{kb('images')} | {kb('fonts')} |")
sp = r.get("shared_postgres")
if sp:
    print("\n### 20 Wrought sites on one PostgreSQL server\n")
    print("| PostgreSQL | PostgreSQL MiB | Engine each MiB | Total MiB | Per site MiB | "
          "Connections |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for k, v in sp.items():
        print(f"| {k} (`{v['pg_args']}`) | {f(v['postgres']['current_mib'], 1)} | "
              f"{f(v['engine_each_mib'], 1)} | {f(v['total_mib'], 1)} | {f(v['per_site_mib'], 1)} | "
              f"{v['connections']} |")
dn = r.get("density_64gb")
if dn:
    print("\n### Sites per 64 GB (an estimate, as arithmetic)\n")
    for k, v in dn.items():
        print(f"- {k}: {v['formula']} = **{v['sites']:,}** ({v['note']})")
if r.get("wall_s"):
    print("\nWall time of this run per target (s): " + ", ".join(
        f"{k} {v:,.0f}" for k, v in r["wall_s"].items()) +
        f"; total {sum(r['wall_s'].values()) / 60:.0f} min.")
v = r.get("versions", {})
if v:
    print("\nVersions: " + ", ".join(f"{k} {x}" for k, x in v.items()) + ".")
