"""Resource measurements (`bench/run.sh resources`, and part of the full
run): whole-stack memory and CPU from each container's cgroup, the smallest
memory limit that still serves the load test, page weight in a browser,
and 20 Wrought sites sharing one PostgreSQL server.

Every number is the sum over a target's containers (application, database,
web server or PHP), read from cgroup v2: `memory.current` (all memory the
kernel charges to the container, page cache included), `anon` from
`memory.stat` (what the processes allocated), and `usage_usec` from
`cpu.stat`.
"""

import json
import shutil
import subprocess
import threading
import time

import run
from run import cg_stat, cgroup_dir, log, sh

MiB = 2**20


def stack_memory(containers):
    """The stack's memory (sum) and each container's: `current_mib`
    (memory.current, page cache included) and `anon_mib`."""
    cur = anon = 0
    each = {}
    for c in containers:
        d = cgroup_dir(c)
        cc = int((d / "memory.current").read_text())
        ca = cg_stat(d, "anon")
        cur += cc
        anon += ca
        each[c.replace("wrought-bench-", "")] = {"current_mib": round(cc / MiB, 1),
                                                 "anon_mib": round(ca / MiB, 1)}
    return {"current_mib": round(cur / MiB, 1), "anon_mib": round(anon / MiB, 1),
            "each": each}


def stack_cpu_usec(containers):
    return sum(cg_stat(cgroup_dir(c), "usage_usec") for c in containers)


def oom_kills(containers):
    n = 0
    for c in containers:
        d = cgroup_dir(c)
        if d is None:  # the container is gone: it was killed
            n += 1
            continue
        for line in (d / "memory.events").read_text().splitlines():
            k, v = line.split()
            if k == "oom_kill":
                n += int(v)
    return n


class Sampler:
    """The stack's memory every 100 ms while a load runs (its peak)."""

    def __init__(self, containers):
        self.containers = containers
        self.peak = {"current_mib": 0, "anon_mib": 0, "each": {}}
        self._stop = threading.Event()
        self._t = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self._stop.wait(0.1):
            try:
                m = stack_memory(self.containers)
            except (OSError, TypeError, AttributeError):
                continue
            for k in ("current_mib", "anon_mib"):
                self.peak[k] = max(self.peak[k], m[k])
            for c, v in m["each"].items():
                p = self.peak["each"].setdefault(c, {"current_mib": 0, "anon_mib": 0})
                for k in p:
                    p[k] = max(p[k], v[k])

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        self._t.join()


def light_load(url, seconds=10):
    """Steady light load: one connection, one request at a time."""
    r = run.compose("run", "--rm", "loadgen", "-t", "1", "-c", "1", "-d", f"{seconds}s", url)
    m = run.re.search(r"Requests/sec:\s+([\d.]+)", r.stdout)
    return float(m.group(1)) if m else None


def profile(name, containers, page_url, search_url=None, listing_url=None):
    """Memory idle / light load / peak under the standard load / idle again,
    and CPU-seconds per 1,000 requests for a page, for the article listing
    (if given) and for search."""
    out = {"containers": containers}
    for _ in range(20):  # warm-up
        run.http("GET", page_url)
    time.sleep(5)
    out["idle"] = stack_memory(containers)
    with Sampler(containers) as s:
        out["light_rps"] = light_load(page_url)
    out["light_peak"] = s.peak
    cpu0 = stack_cpu_usec(containers)
    with Sampler(containers) as s:
        page = run.wrk(page_url)
    cpu1 = stack_cpu_usec(containers)
    out["load_peak"] = s.peak
    out["page"] = page
    if page.get("requests"):
        out["cpu_s_per_1000_page"] = round((cpu1 - cpu0) / 1e6 / page["requests"] * 1000, 3)
    if listing_url:
        cpu0 = stack_cpu_usec(containers)
        with Sampler(containers) as s:
            lst = run.wrk(listing_url)
        cpu1 = stack_cpu_usec(containers)
        out["listing"] = lst
        out["listing_peak"] = s.peak
        if lst.get("requests"):
            out["cpu_s_per_1000_listing"] = round((cpu1 - cpu0) / 1e6 / lst["requests"] * 1000, 3)
    if search_url:
        cpu0 = stack_cpu_usec(containers)
        with Sampler(containers) as s:
            dyn = run.wrk(search_url, vary=True)
        cpu1 = stack_cpu_usec(containers)
        out["search"] = dyn
        out["search_peak"] = s.peak
        if dyn.get("requests"):
            out["cpu_s_per_1000_search"] = round((cpu1 - cpu0) / 1e6 / dyn["requests"] * 1000, 3)
    time.sleep(10)
    out["idle_after"] = stack_memory(containers)
    log(f"resources {name}: idle {out['idle']['current_mib']} MiB, load peak "
        f"{out['load_peak']['current_mib']} MiB, cpu/1000 page "
        f"{out.get('cpu_s_per_1000_page')} s, search {out.get('cpu_s_per_1000_search')} s")
    return out


LIMITS_MIB = [1024, 512, 384, 256, 192, 128, 96, 64, 48, 32, 24, 16, 12, 8]


def min_limit(name, container, url, vary=False):
    """Step `container`'s memory limit down while the standard load test
    still passes (no errors, no OOM kill); returns the smallest that did."""
    ok = None
    steps = []
    for lim in LIMITS_MIB:
        before = oom_kills([container])
        r = sh(["docker", "update", "--memory", f"{lim}m", "--memory-swap", f"{lim}m",
                container], check=False)
        if r.returncode != 0:
            steps.append({"limit_mib": lim, "result": "refused: " + r.stderr.strip()[-80:]})
            break
        time.sleep(1)
        try:
            res = run.wrk(url, vary=vary) if run.http("GET", url)[0] == 200 else {"rps": None}
        except SystemExit as e:
            res = {"rps": None, "error": str(e)[-200:]}
        killed = oom_kills([container]) - before
        good = (res.get("rps") and res.get("errors", 0) == 0 and not res.get("socket_errors")
                and killed == 0)
        steps.append({"limit_mib": lim, "rps": res.get("rps"), "errors": res.get("errors"),
                      "socket_errors": res.get("socket_errors"), "oom_kills": killed})
        if not good:
            break
        ok = lim
    # Back to the bench's normal limit, restarting if the step killed it.
    sh(["docker", "update", "--memory", "2g", "--memory-swap", "2g", container], check=False)
    st = sh(["docker", "inspect", "-f", "{{.State.Running}}", container]).stdout.strip()
    if st != "true":
        sh(["docker", "start", container])
    for _ in range(120):  # serving again before the next measurement
        if run.http("GET", url)[0] == 200:
            break
        time.sleep(1)
    log(f"min limit {name} ({container}): {ok} MiB")
    return {"min_mib": ok, "steps": steps}


def page_weight(urls):
    """Headless Chromium over `urls` (scripts/check-a11y.sh's Playwright)."""
    tools = run.ROOT / "target/a11y-tools"
    if not (tools / "node_modules/playwright").is_dir():
        log("page weight: target/a11y-tools has no Playwright (run scripts/check-a11y.sh once)")
        return None
    shutil.copy2(run.HERE / "pageweight.mjs", tools / "pageweight.mjs")
    r = subprocess.run(["node", "pageweight.mjs", *urls], cwd=tools, capture_output=True,
                       text=True, timeout=600)
    if r.returncode != 0:
        log(f"page weight failed: {r.stderr[-400:]}")
        return None
    return json.loads(r.stdout)


PG_SMALL = ("-c shared_buffers=32MB -c work_mem=2MB -c maintenance_work_mem=16MB "
            "-c max_connections=200 -c wal_buffers=1MB -c effective_cache_size=256MB")


def shared_postgres(n_sites=20):
    """`n_sites` Wrought sites (the 200-article site, a database each) served
    by `n_sites` `engine serve` containers on one PostgreSQL server, idle
    after a warm-up of every site: total and per-site memory, with
    PostgreSQL at its defaults (max_connections raised to fit) and with
    small settings."""
    results = {}
    for cfg, args in (("defaults", "-c max_connections=200"), ("small", PG_SMALL)):
        run.restart_db(args, fresh=False)
        names = []
        for i in range(n_sites):
            db = f"site{i:02d}"
            run.compose("exec", "-T", "wrought-db", "psql", "-q", "-U", "wrought", "-d",
                        "postgres", "-c", f"DROP DATABASE IF EXISTS {db} WITH (FORCE)",
                        "-c", f"CREATE DATABASE {db}")
            url = run.pg_url(db)
            site = f"/sites/{db}"
            sh(["docker", "exec", "wrought-bench-engine-tool", "sh", "-c",
                f"rm -rf {site} && cp -r /site {site} && rm -rf {site}/public {site}/data/*"])
            for args_ in (["migrate"], ["content", "seed"], ["build"]):
                run.engine_exec(args_, url, site=site)
            name = f"wrought-bench-site{i:02d}"
            sh(["docker", "rm", "-f", name], check=False)
            sh(["docker", "run", "-d", "--name", name, "--network", "wrought-bench_default",
                "--cpuset-cpus", run.os.environ.get("BENCH_APP_CPU", "6"), "--memory", "512m",
                "-e", f"ENGINE_DATABASE_URL={url}", "-e",
                "ENGINE_SECRET_KEY=bench-only key, not a secret, 0123456789abcdef",
                "-e", "ENGINE_PUBLIC_URL=http://127.0.0.1", "-v",
                f"{run.WORK}/sites/{db}:/site", "wrought-bench-engine",
                "engine", "serve", "--site", "/site", "--host", "0.0.0.0", "--port", "4400"])
            names.append(name)
        time.sleep(5)
        for name in names:
            for path in ("/articles/article-17/", "/search/?q=iron"):
                sh(["docker", "run", "--rm", "--network", "wrought-bench_default",
                    "wrought-bench-loadgen", "-t1", "-c1", "-d1s", f"http://{name}:4400{path}"],
                   check=False)
        time.sleep(10)
        db = stack_memory(["wrought-bench-wrought-db"])
        sites = [stack_memory([n]) for n in names]
        conns = run.compose("exec", "-T", "wrought-db", "psql", "-At", "-U", "wrought", "-d",
                            "postgres", "-c", "SELECT count(*) FROM pg_stat_activity "
                            "WHERE backend_type = 'client backend'").stdout.strip()
        total = db["current_mib"] + sum(s["current_mib"] for s in sites)
        results[cfg] = {
            "postgres": db, "sites": n_sites, "connections": int(conns or 0),
            "engine_each_mib": round(sum(s["current_mib"] for s in sites) / n_sites, 1),
            "engine_each_anon_mib": round(sum(s["anon_mib"] for s in sites) / n_sites, 1),
            "total_mib": round(total, 1), "per_site_mib": round(total / n_sites, 1),
            "pg_args": args,
        }
        log(f"shared postgres ({cfg}): {results[cfg]}")
        for name in names:
            sh(["docker", "rm", "-f", name], check=False)
    run.restart_db("", fresh=False)
    return results
