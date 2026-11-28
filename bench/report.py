#!/usr/bin/env python3
"""Markdown tables from bench/results/results.json (for the docs page).

    python3 bench/report.py > /tmp/tables.md

Sections whose data a run did not measure (targets not selected) are left
out.
"""
import json
import statistics
from pathlib import Path

HERE = Path(__file__).parent
r = json.loads((HERE / "results/results.json").read_text())
# The extended group's run (Rails, Wrought on SQLite), if there is one, is
# merged in, each figure from its own run.
ext_path = HERE / "results/results-extended.json"
ext = json.loads(ext_path.read_text()) if ext_path.exists() else {}
for key in ("static", "listing", "dynamic", "edit_s", "cold_start_s", "resources", "apps",
            "page_bytes", "listing_bytes", "page_weight"):
    if ext.get(key):
        r.setdefault(key, {}).update(ext[key])


def f(x, nd=0):
    return "n/a" if x is None else f"{x:,.{nd}f}"


def ms(x):
    return "n/a" if x is None else (f"{x:.1f}" if x < 100 else f"{x:,.0f}")


names = {
    "wrought_engine": "Wrought, published file (`engine serve`)",
    "wrought_request_pg": "Wrought, rendered per request (render dial `request`)",
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
    "phoenix": "Phoenix (rendered per request)",
    "loco": "Loco (rendered per request)",
    "wrought_markdown_nocopies": "Wrought, Markdown files, no compressed copies",
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
if r.get("listing"):
    print("\n### Listing page (/articles/, 10 per page)\n")
    print("| System | Requests/s | p50 ms | p99 ms | Page bytes |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for k, v in r["listing"].items():
        if v:
            print(f"| {nm(k)} | {f(v['rps'])} | {ms(v['p50_ms'])} | {ms(v['p99_ms'])} | "
                  f"{f(r.get('listing_bytes', {}).get(k))} |")
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
apps = {k: v for k, v in res.items() if "each" in v.get("idle", {})}
if apps:
    print("\n### Application and database memory, separately (MiB; cgroup total / anonymous)\n")
    print("| System | Container | Idle | Peak, load test (article) | Peak, search |")
    print("| --- | --- | ---: | ---: | ---: |")
    for k, v in apps.items():
        for c, m in v["idle"]["each"].items():
            lp = v["load_peak"]["each"].get(c, {})
            sp = v.get("search_peak", {}).get("each", {}).get(c, {})
            cell = lambda x: f"{f(x.get('current_mib'), 1)} / {f(x.get('anon_mib'), 1)}" if x else "n/a"  # noqa: E731
            print(f"| {nm(k)} | {c} | {cell(m)} | {cell(lp)} | {cell(sp)} |")
    print("\nThe total includes the page cache of the files a container read (for a "
          "database, its data files); the anonymous figure is what its processes allocated.")
    print("\n| System | CPU s / 1,000 articles | CPU s / 1,000 listings | CPU s / 1,000 searches |")
    print("| --- | ---: | ---: | ---: |")
    for k, v in apps.items():
        print(f"| {nm(k)} | {f(v.get('cpu_s_per_1000_page'), 3)} | "
              f"{f(v.get('cpu_s_per_1000_listing'), 3)} | {f(v.get('cpu_s_per_1000_search'), 3)} |")
cs = r.get("cold_start_s", {})
if cs:
    print("\n### Cold start (container started until the article page answers)\n")
    print("| System | Seconds |")
    print("| --- | ---: |")
    for k, v in cs.items():
        print(f"| {nm(k)} | {v:.2f} |")
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
for label, rr in (("core", r), ("extended", ext)):
    if rr.get("wall_s"):
        print(f"\nWall time of the {label} run per target (s): " + ", ".join(
            f"{k} {v:,.0f}" for k, v in rr["wall_s"].items()) +
            f"; total {sum(rr['wall_s'].values()) / 60:.0f} min.")
for rr in r.get("reruns", []):
    t = rr.get("targets") or {}
    w = rr.get("wall_s") or {}
    print(f"\nRe-run of {rr.get('date')} (`--targets`, {rr.get('file')}; its figures replace the "
          f"core run's for these): serving {', '.join(t.get('serve', [])) or 'none'}, builds "
          f"{', '.join(t.get('build', [])) or 'none'}; total {sum(w.values()) / 60:.0f} min.")
v = r.get("versions", {})
if v:
    print("\nVersions: " + ", ".join(f"{k} {x}" for k, x in v.items()) + ".")
