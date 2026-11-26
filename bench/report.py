#!/usr/bin/env python3
"""Markdown tables from bench/results/results.json (for the docs page).

    python3 bench/report.py > /tmp/tables.md
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
    "wrought_engine": "Wrought (`engine serve`)",
    "wrought_caddy": "Wrought output via Caddy",
    "astro_caddy": "Astro output via Caddy",
    "wordpress": "WordPress",
    "drupal": "Drupal",
    "wrought_postgres": "Wrought, PostgreSQL",
    "wrought_sqlite": "Wrought, SQLite",
    "astro": "Astro",
    "wrought_markdown": "Wrought, Markdown files",
    "rails_plain": "Rails (a) plain rendering",
    "rails_cached": "Rails (b) with Rails caching",
}
print("### Static page\n")
print("| System | Requests/s | p50 ms | p99 ms | Page bytes |")
print("| --- | ---: | ---: | ---: | ---: |")
for k, v in r["static"].items():
    print(f"| {names[k]} | {f(v['rps'])} | {ms(v['p50_ms'])} | {ms(v['p99_ms'])} | {f(r['page_bytes'].get(k))} |")
print("\n### Dynamic page (search, a different word per request)\n")
print("| System | Requests/s | p50 ms | p99 ms |")
print("| --- | ---: | ---: | ---: |")
for k in ("wrought_postgres", "wrought_sqlite", "wordpress", "drupal", "rails_plain", "rails_cached"):
    v = r["dynamic"].get(k)
    if not v:
        continue
    print(f"| {names[k]} | {f(v['rps'])} | {ms(v['p50_ms'])} | {ms(v['p99_ms'])} |")
print("\n### Cold publish of 1000 articles\n")
print("| System | 1 core, median s | Runs (s) | 4 cores, median s | Pages |")
print("| --- | ---: | --- | ---: | ---: |")
four = r.get("cold_build_4_cores", {})
for k, v in r["cold_build"].items():
    f4 = four.get(k)
    print(f"| {names[k]} | {v['seconds']:.1f} | {', '.join(f'{x:.1f}' for x in v['runs'])} | "
          f"{f'{f4['seconds']:.1f}' if f4 else 'n/a'} | {v['pages']} |")
print("| Rails, WordPress, Drupal | not applicable (render on request) | | | |")
print("\n### Edit to published\n")
print("| System | Median | All runs (s) |")
print("| --- | ---: | --- |")
for k in ("wrought_sqlite", "wrought_postgres", "wordpress", "drupal", "astro", "rails_plain", "rails_cached"):
    v = r["edit_s"].get(k)
    if not v:
        continue
    print(f"| {names[k]} | {statistics.median(v) * 1000:,.0f} ms | {', '.join(f'{x:.2f}' for x in v)} |")
m = r["memory_mib"]
print("\n### Memory after the load runs (MiB)\n")
print("| System | Application | Database | Total |")
print("| --- | ---: | ---: | ---: |")
rows = [
    ("Wrought, PostgreSQL", m.get("wrought-bench-engine"), m.get("wrought-bench-wrought-db")),
    ("Wrought, SQLite", r.get("memory_mib_sqlite"), 0),
    ("WordPress", m.get("wrought-bench-wp"), m.get("wrought-bench-wp-db")),
    ("Drupal", m.get("wrought-bench-drupal"), m.get("wrought-bench-drupal-db")),
    ("Astro (Caddy serving its files)", m.get("wrought-bench-caddy"), 0),
]
for name in ("rails_plain", "rails_cached"):
    rr = r.get("rails", {}).get(name)
    if rr:
        rows.append((names[name], rr["static"].get("memory_mib_under_load"), rr.get("memory_mib_db")))
for n, a, d in rows:
    print(f"| {n} | {f(a, 1)} | {f(d, 1) if d else '—'} | {f((a or 0) + (d or 0), 1)} |")
if r.get("rails"):
    print("\nRails idle (after start and one view): " + ", ".join(
        f"{names[k]} {f(v.get('memory_mib_idle'), 1)} MiB" for k, v in r["rails"].items()))
if r.get("cold_start_s"):
    print("\n### Cold start (container started until a page answers)\n")
    print("| System | Seconds |")
    print("| --- | ---: |")
    for k, v in r["cold_start_s"].items():
        print(f"| {names.get(k, k)} | {v:.1f} |")
i = r["images"]
print("\n### Size\n")
print(f"- Wrought binary: {r['binary_mb']['static']} MB (static preset), {r['binary_mb']['app']} MB (app preset, both databases)")
print(f"- Container images: Wrought {i['wrought']['mb']} MB (Ubuntu base + binary), "
      f"WordPress {i['wordpress']['mb']} MB + MariaDB {i['mariadb']['mb']} MB, "
      f"Drupal {i['drupal']['mb']} MB (with Drush) + PostgreSQL {i['postgres']['mb']} MB, "
      f"Astro build: Node {i['node']['mb']} MB + node_modules {r['disk_mb']['astro_node_modules']} MB; "
      f"served by Caddy {i['caddy']['mb']} MB")
print(f"- Output for 200 articles: Wrought public/ {r['disk_mb']['wrought_public']} MB, Astro dist/ {r['disk_mb']['astro_dist']} MB")
print(f"\nDrupal's page cache answered `X-Drupal-Cache: {r.get('drupal_cache_header')}` for the static page.")
