#!/usr/bin/env python3
"""`bench/run.sh stall [N] [CORES]`: the cold build of 1000 articles on
PostgreSQL, N times (default 20) on a fresh database each time, with
`engine build --timings` and the disk's own sync latency beside each run.

This is how the v0.7 build stall was found (DECISIONS.md, 2026-11-26): it
prints each run's wall time, its phases and the slowest disk sync during
the run, then min / median / max and how many runs stalled (took over
twice the fastest), and writes results/stall-<date>-<cores>c.json.

While each build runs it also records (`Watch`) what PostgreSQL's
processes wait for (`pg_stat_activity` every 50 ms) and the database
container's cgroup: dirty and writeback page cache, memory and I/O
pressure, memory.events, io.stat; plus the host's Dirty / Writeback. A
stalled run keeps the full timeline in the JSON file, so the cause of a
stall can be read from the file rather than guessed.
"""
import json
import os
import statistics
import subprocess
import threading
import time

import run

DB = "wrought-bench-wrought-db"
WAITS = ("SELECT to_char(clock_timestamp(), 'HH24:MI:SS.MS'), pid, backend_type, "
         "coalesce(state, ''), coalesce(wait_event_type, ''), coalesce(wait_event, ''), "
         "left(regexp_replace(query, '\\s+', ' ', 'g'), 50) FROM pg_stat_activity "
         "WHERE pid <> pg_backend_pid() AND (state = 'active' OR wait_event_type IN "
         "('IO', 'LWLock', 'Lock', 'BufferPin', 'IPC'))")


def _kv(path):
    out = {}
    try:
        for line in path.read_text().splitlines():
            k, _, v = line.partition(" ")
            out[k] = v
    except OSError:
        pass
    return out


class Watch:
    """PostgreSQL wait events and the database container's cgroup state
    while a build runs, every 50 ms."""

    def __init__(self):
        self.d = run.cgroup_dir(DB)
        self.waits = []
        self.cg = []
        self._stop = threading.Event()
        self._p = None
        self._threads = []

    def _read_waits(self):
        for line in self._p.stdout:
            line = line.strip()
            if line and "|" in line:
                self.waits.append(line)

    def _sample(self):
        t0 = time.monotonic()
        while not self._stop.wait(0.05):
            m = _kv(self.d / "memory.stat")
            mi = _kv(run.Path("/proc/meminfo"))
            self.cg.append({
                "t": round(time.monotonic() - t0, 2), "clock": time.strftime("%H:%M:%S"),
                "dirty_kib": int(m.get("file_dirty", 0)) // 1024,
                "writeback_kib": int(m.get("file_writeback", 0)) // 1024,
                "file_kib": int(m.get("file", 0)) // 1024,
                "anon_kib": int(m.get("anon", 0)) // 1024,
                "current_kib": int((self.d / "memory.current").read_text()) // 1024,
                "io_pressure": (self.d / "io.pressure").read_text().split("\n")[1].split()[-1],
                "mem_pressure": (self.d / "memory.pressure").read_text().split("\n")[1].split()[-1],
                "events": (self.d / "memory.events").read_text().replace("\n", " ").strip(),
                "io_stat": (self.d / "io.stat").read_text().strip(),
                "host_dirty_kib": int(mi.get("Dirty:", "0 kB").split()[0]),
                "host_writeback_kib": int(mi.get("Writeback:", "0 kB").split()[0]),
            })

    def __enter__(self):
        # `\watch` repeats the query read from psql's input.
        self._p = subprocess.Popen(
            ["docker", "exec", "-i", DB, "psql", "-U", "wrought", "-d", "postgres", "-At",
             "-q"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True)
        self._p.stdin.write(WAITS + "\n\\watch 0.05\n")
        self._p.stdin.flush()
        for f in (self._read_waits, self._sample):
            t = threading.Thread(target=f, daemon=True)
            t.start()
            self._threads.append(t)
        time.sleep(0.2)
        return self

    def __exit__(self, *exc):
        self._stop.set()
        self._p.kill()
        self._p.wait()
        for t in self._threads:
            t.join(timeout=5)

    def summary(self):
        """Counts of each wait event seen (one per 50 ms sample and process),
        and the peaks of the cgroup state."""
        counts = {}
        for w in self.waits:
            f = w.split("|")
            if len(f) >= 6 and f[4]:
                k = f"{f[2]}: {f[4]}:{f[5]}"
                counts[k] = counts.get(k, 0) + 1
        peak = lambda k: max((s[k] for s in self.cg), default=None)  # noqa: E731
        return {"wait_samples": dict(sorted(counts.items(), key=lambda x: -x[1])),
                "peak_dirty_kib": peak("dirty_kib"), "peak_writeback_kib": peak("writeback_kib"),
                "peak_host_dirty_kib": peak("host_dirty_kib"),
                "peak_host_writeback_kib": peak("host_writeback_kib"),
                "max_io_pressure_total": peak("io_pressure"),
                "events_end": self.cg[-1]["events"] if self.cg else None}


def checkpoints_since(since):
    r = run.sh(["docker", "logs", "--since", since, DB], check=False)
    return [ln for ln in (r.stdout + r.stderr).splitlines()
            if "checkpoint" in ln or "unlogged" in ln.lower()]


def main(args):
    n = int(args[0]) if args else 20
    cores = int(args[1]) if len(args) > 1 else 1
    label = os.environ.get("BENCH_STALL_LABEL", "")
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
    pg_args = os.environ.get("BENCH_PG_ARGS", "")  # passed on by compose
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
        since = time.strftime("%Y-%m-%dT%H:%M:%S")
        start_clock = time.strftime("%H:%M:%S")
        with run.DiskProbe() as probe, Watch() as w:
            t0 = time.monotonic()
            r = run.engine_exec(["build", "--timings"], db_url, site="/site-1000", env=env)
            took = time.monotonic() - t0
        p = run.build_phases(r.stderr)
        disk = probe.summary()
        row = {"wall_s": took, "start": start_clock, "phases_ms": p, "disk_sync": disk,
               "timings": r.stderr.splitlines()[-30:], "watch": w.summary(),
               "pg_log": checkpoints_since(since)}
        rows.append(row)
        top = {k: v for k, v in p.items() if k in ("search index", "build", "tag index")}
        run.log(f"run {i + 1}: {took:.2f} s {json.dumps(top)} slowest disk sync "
                f"{disk and disk['max_ms']} ms; waits {json.dumps(row['watch']['wait_samples'])}")
        row["_w"] = w
    walls = [r["wall_s"] for r in rows]
    fastest = min(walls)
    for r in rows:
        w = r.pop("_w")
        if r["wall_s"] > 2 * fastest:
            # The whole timeline of a stalled run.
            r["wait_timeline"] = w.waits
            r["cgroup_timeline"] = w.cg
    stalls = sum(w > 2 * fastest for w in walls)
    summary = {"runs": n, "cores": cores, "label": label, "pg_args": pg_args,
               "min_s": min(walls), "median_s": statistics.median(walls), "max_s": max(walls),
               "stalls": stalls, "over_5s": sum(w > 5 for w in walls),
               "engine": run.sh([str(run.HERE / "wrought/engine"),
                                 "--version"]).stdout.splitlines()[0]}
    run.log(f"min {min(walls):.2f} s, median {statistics.median(walls):.2f} s, "
            f"max {max(walls):.2f} s; stalled (over twice the fastest): {stalls}/{n}")
    suffix = f"-{label}" if label else ""
    out = run.RESULTS / f"stall-{time.strftime('%Y-%m-%d')}-{cores}c{suffix}.json"
    out.write_text(json.dumps({"summary": summary, "runs": rows}, indent=1))
    run.log(f"wrote {out}")
