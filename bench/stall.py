#!/usr/bin/env python3
"""`bench/run.sh stall [N] [CORES]`: the cold build of 1000 articles on
PostgreSQL, N times (default 20) on a fresh database each time, with
`engine build --timings` and the disk's own sync latency beside each run.

This is how the v0.7 build stall was found (DECISIONS.md, 2026-11-26): it
prints each run's wall time, its phases and the slowest disk sync during
the run, then min / median / max and how many runs stalled (took over
twice the fastest), and writes results/stall-<date>-<cores>c.json.
"""
import json
import statistics
import time

import run


def main(args):
    n = int(args[0]) if args else 20
    cores = int(args[1]) if len(args) > 1 else 1
    run.clean_work()
    run.WORK.mkdir(exist_ok=True)
    run.make_data()
    engine = run.os.environ.get("BENCH_ENGINE")
    if engine:
        run.shutil.copy2(engine, run.HERE / "wrought/engine")
    else:
        run.build_engines()
    run.wrought_site(run.WORK / "wrought-site", run.N_RUNTIME)
    run.wrought_site(run.WORK / "wrought-site-1000", run.N_BUILD)
    run.compose("build", "wrought", capture=False)
    run.compose("--profile", "tools", "pull", "--quiet", "wrought-db", capture=False)
    run.start_wrought_tools()
    db_url = run.pg_url("wrought1000")
    run.tool_cpus("wrought-bench-engine-tool", cores)
    env = {"ENGINE_BUILD_JOBS": str(cores), "ENGINE_BUILD_PIN_CPUS": "1"}
    rows = []
    for i in range(n):
        run.compose("exec", "-T", "wrought-db", "psql", "-U", "wrought", "-d", "wrought",
                    "-c", "DROP DATABASE IF EXISTS wrought1000 WITH (FORCE)",
                    "-c", "CREATE DATABASE wrought1000")
        run.engine_exec(["migrate"], db_url, site="/site-1000")
        run.engine_exec(["content", "seed"], db_url, site="/site-1000")
        with run.DiskProbe() as probe:
            t0 = time.monotonic()
            r = run.engine_exec(["build", "--timings"], db_url, site="/site-1000", env=env)
            took = time.monotonic() - t0
        p = run.build_phases(r.stderr)
        disk = probe.summary()
        rows.append({"wall_s": took, "phases_ms": p, "disk_sync": disk})
        top = {k: v for k, v in p.items() if k in ("search index", "build", "tag index")}
        run.log(f"run {i + 1}: {took:.2f} s {json.dumps(top)} slowest disk sync "
                f"{disk and disk['max_ms']} ms")
    walls = [r["wall_s"] for r in rows]
    stalls = sum(w > 2 * min(walls) for w in walls)
    summary = {"runs": n, "cores": cores, "min_s": min(walls),
               "median_s": statistics.median(walls), "max_s": max(walls),
               "stalls": stalls, "engine": run.sh([str(run.HERE / "wrought/engine"),
                                                    "--version"]).stdout.splitlines()[0]}
    run.log(f"min {min(walls):.2f} s, median {statistics.median(walls):.2f} s, "
            f"max {max(walls):.2f} s; stalled (over twice the fastest): {stalls}/{n}")
    out = run.RESULTS / f"stall-{time.strftime('%Y-%m-%d')}-{cores}c.json"
    out.write_text(json.dumps({"summary": summary, "runs": rows}, indent=1))
    run.log(f"wrote {out}")
