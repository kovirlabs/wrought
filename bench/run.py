#!/usr/bin/env python3
"""The benchmark: Wrought, WordPress, Drupal and Astro on equivalent content.

Run through `bench/run.sh`. Standard library only. Writes
`results/results.json` and `results/results.md` and prints the table.

What is measured (each system with its default configuration, the same CPU
and memory caps, on one machine over loopback):

1. static page: one article page; Wrought from its own server and through
   Caddy, Astro's output through the same Caddy, WordPress and Drupal
   rendering it (Drupal's default internal page cache is on);
2. dynamic page: search results, with a different word on each request so
   no page cache answers (Astro has no server: not applicable);
3. cold publish: time to render every page of 1000 articles (Wrought `engine
   build`, Astro `astro build`; WordPress and Drupal render on request);
4. edit to published: from an API edit of an article's title until the
   article's public page shows it (Astro: edit the file and rebuild);
5. memory of the running containers after the load runs;
6. size of the binary and the container images.
"""

import base64
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
WORK = HERE / "work"
RESULTS = HERE / "results"
COMPOSE = ["docker", "compose", "-f", str(HERE / "compose.yml")]
N_RUNTIME = 200
N_BUILD = 1000
WARM, MEASURE, THREADS, CONNS = "3s", "10s", 1, 32
ADMIN_PW = "bench admin password"
SITE_KEY = "bench-only key, not a secret: 0123456789abcdef"


def log(msg):
    print(f"[bench {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def sh(cmd, check=True, capture=True, env=None, input=None, cwd=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    r = subprocess.run(
        cmd,
        check=False,
        capture_output=capture,
        text=True,
        env=e,
        input=input,
        cwd=cwd,
    )
    if check and r.returncode != 0:
        raise SystemExit(
            f"command failed ({r.returncode}): {' '.join(map(str, cmd))}\n"
            f"{r.stdout or ''}{r.stderr or ''}"
        )
    return r


def compose(*args, **kw):
    return sh(COMPOSE + list(args), **kw)


def http(method, url, body=None, headers=None, timeout=30):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace"), dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)
    except (urllib.error.URLError, ConnectionError, TimeoutError) as e:
        return 0, str(e), {}


def wait_http(url, what, timeout=240, ok=(200,)):
    start = time.time()
    while time.time() - start < timeout:
        status, _, _ = http("GET", url, timeout=10)
        if status in ok:
            return
        time.sleep(1)
    raise SystemExit(f"{what} did not answer {url} within {timeout}s")


# ----- content -----------------------------------------------------------------


def md_to_html(md):
    out = []
    for block in md.split("\n\n"):
        block = block.strip()
        if block.startswith("## "):
            out.append(f"<h2>{block[3:]}</h2>")
        elif block:
            out.append(f"<p>{block}</p>")
    return "\n".join(out)


def make_data():
    (WORK / "data").mkdir(parents=True, exist_ok=True)
    for n in (N_RUNTIME, N_BUILD):
        r = sh([sys.executable, str(HERE / "data/generate.py"), str(n)])
        arts = json.loads(r.stdout)
        for a in arts:
            a["body_html"] = md_to_html(a["body"])
        (WORK / "data" / f"articles-{n}.json").write_text(json.dumps(arts))
    log(f"data: {N_RUNTIME} and {N_BUILD} articles")


def toml_str(s):
    return json.dumps(s)


def wrought_site(dest, n):
    if dest.exists():
        shutil.rmtree(dest)
    src = ROOT / "examples/cms-site"
    rev = os.environ.get("BENCH_SITE_REV")
    if rev:
        # An older engine binary gets the example site of its own release
        # (its migrations and type snapshot).
        old = WORK / f"example-{rev}"
        if not old.exists():
            old.mkdir(parents=True)
            arch = subprocess.run(["git", "-C", str(ROOT), "archive", rev, "examples/cms-site"],
                                  check=True, capture_output=True).stdout
            subprocess.run(["tar", "-x", "-C", str(old)], input=arch, check=True)
        src = old / "examples/cms-site"
    shutil.copytree(
        src,
        dest,
        ignore=shutil.ignore_patterns("public", "data", "media-samples", ".*"),
    )
    cfg = (dest / "site.toml").read_text()
    cfg = cfg.replace("per_page = 5", "per_page = 10")
    (dest / "site.toml").write_text(cfg)
    arts = json.loads((WORK / "data" / f"articles-{n}.json").read_text())
    lines = [
        '[[settings]]\ntagline = "Benchmark site."\n',
        '[[author]]\nslug = "ada"\nname = "Ada Smith"\nstatus = "published"\n',
    ]
    for a in arts:
        lines.append(
            "[[article]]\n"
            f"slug = {toml_str(a['slug'])}\ntitle = {toml_str(a['title'])}\n"
            f"summary = {toml_str(a['summary'])}\nbody = {toml_str(a['body'])}\n"
            f"topics = {json.dumps(a['topics'])}\nauthor = \"ada\"\n"
            'status = "published"\n'
        )
    (dest / "seed/content.toml").write_text("\n".join(lines))
    (dest / "data").mkdir(exist_ok=True)


def astro_site(dest, n):
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(HERE / "astro", dest)
    # The same stylesheet as Wrought's pages (TagBrush, vendored in core).
    (dest / "public").mkdir(exist_ok=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 dest / "public/tagbrush.min.css")
    content = dest / "src/content"
    content.mkdir(parents=True)
    for a in json.loads((WORK / "data" / f"articles-{n}.json").read_text()):
        fm = (
            f"---\ntitle: {json.dumps(a['title'])}\nsummary: {json.dumps(a['summary'])}\n"
            f"date: \"{a['date']}\"\ntopics: {json.dumps(a['topics'])}\n---\n\n"
        )
        (content / f"{a['slug']}.md").write_text(fm + a["body"] + "\n")


# ----- load ----------------------------------------------------------------------


def parse_ms(v):
    m = re.match(r"([\d.]+)(us|ms|s)", v)
    if not m:
        return None
    x = float(m.group(1))
    return {"us": x / 1000, "ms": x, "s": x * 1000}[m.group(2)]


def wrk(url, vary=False):
    # A long timeout so slow responses are measured, not dropped.
    base = ["run", "--rm", "loadgen", "-t", str(THREADS), "-c", str(CONNS), "--timeout", "30s"]
    if vary:
        base += ["-s", "/scripts/vary.lua"]
    compose(*base, "-d", WARM, url)
    r = compose(*base, "-d", MEASURE, "--latency", url)
    out = r.stdout
    res = {"url": url}
    m = re.search(r"Requests/sec:\s+([\d.]+)", out)
    res["rps"] = float(m.group(1)) if m else None
    for pct in ("50", "90", "99"):
        m = re.search(rf"^\s+{pct}%\s+(\S+)", out, re.M)
        res[f"p{pct}_ms"] = parse_ms(m.group(1)) if m else None
    m = re.search(r"Non-2xx or 3xx responses:\s+(\d+)", out)
    res["errors"] = int(m.group(1)) if m else 0
    m = re.search(r"(\d+) requests in", out)
    res["requests"] = int(m.group(1)) if m else None
    m = re.search(r"Socket errors: (.*)", out)
    if m:
        res["socket_errors"] = m.group(1)
    return res


def page_bytes(url):
    status, body, _ = http("GET", url)
    return len(body.encode()) if status == 200 else None


# ----- systems -------------------------------------------------------------------


def engine_exec(args, db_url, input=None, site="/site", env=None):
    extra = []
    for k, v in (env or {}).items():
        extra += ["-e", f"{k}={v}"]
    return compose(
        "exec",
        "-T",
        "-e",
        f"ENGINE_DATABASE_URL={db_url}",
        *extra,
        "wrought-tool",
        "engine",
        *args,
        "--site",
        site,
        input=input,
    )


def tool_cpus(container, n):
    """Give a tool container `n` cores: BENCH_TOOL_CPU and the cores after it."""
    first = int(os.environ.get("BENCH_TOOL_CPU", "8").split("-")[0].split(",")[0])
    cpus = str(first) if n == 1 else f"{first}-{first + n - 1}"
    sh(["docker", "update", "--cpuset-cpus", cpus, container])
    return cpus


def setup_wrought(db_url, label):
    """(Re)create the site's database and publish it; return a token."""
    env = {"WROUGHT_DB_URL": db_url}
    compose("stop", "wrought", check=False)
    engine_exec(["migrate"], db_url)
    engine_exec(["content", "seed"], db_url)
    t0 = time.monotonic()
    engine_exec(["build"], db_url)
    build_s = time.monotonic() - t0
    engine_exec(
        ["user", "create", "--email", "admin@bench.test", "--password-stdin"],
        db_url,
        input=ADMIN_PW + "\n",
    )
    r = engine_exec(
        [
            "api", "token", "create", "--email", "admin@bench.test", "--name", "bench",
            "--scope", "read", "--scope", "write",
        ],
        db_url,
    )
    token = next(line for line in r.stdout.splitlines() if line.startswith("wr_"))
    compose("up", "-d", "--force-recreate", "--no-deps", "wrought", env=env)
    wait_http("http://127.0.0.1:58200/", f"wrought ({label})")
    log(f"wrought ({label}): {N_RUNTIME} articles published in {build_s:.2f}s")
    return token


def wrought_cold_build(db_url, label, cores=1):
    """`engine build` of 1000 articles on a fresh database, three times, with
    `cores` cores for the tool container and as many render threads.

    The bench host boots with `isolcpus`: the kernel never moves threads
    between isolated cores, so the engine is told to pin its render threads
    (ENGINE_BUILD_PIN_CPUS=1); without it every thread would share one core.
    """
    site = "/site-1000"
    cpus = tool_cpus("wrought-bench-engine-tool", cores)
    env = {"ENGINE_BUILD_JOBS": str(cores), "ENGINE_BUILD_PIN_CPUS": "1"}
    times = []
    out = ""
    for _ in range(3):
        if db_url.startswith("postgres"):
            # Its own database, so the running site's stays untouched.
            compose(
                "exec", "-T", "wrought-db", "psql", "-U", "wrought", "-d", "wrought",
                "-c", "DROP DATABASE IF EXISTS wrought1000 WITH (FORCE)",
                "-c", "CREATE DATABASE wrought1000",
            )
        else:
            compose("exec", "-T", "wrought-tool", "rm", "-f", "/site-1000/data/site.db",
                    "/site-1000/data/site.db-wal", "/site-1000/data/site.db-shm")
        engine_exec(["migrate"], db_url, site=site)
        engine_exec(["content", "seed"], db_url, site=site)
        t0 = time.monotonic()
        r = engine_exec(["build"], db_url, site=site, env=env)
        times.append(time.monotonic() - t0)
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-engine-tool", 1)
    pages = re.search(r"(\d+) pages?", out)
    log(f"wrought ({label}, {cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None}


def markdown_site(dest, n):
    """The same articles as Markdown files for Wrought's build mode (v0.1,
    no database): the like-for-like comparison with Astro, which also reads
    Markdown files. Pages as Astro's: article pages, a list paginated by 10,
    one page per topic, a home page."""
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "content/articles").mkdir(parents=True)
    (dest / "content/topics").mkdir(parents=True)
    (dest / "templates").mkdir(parents=True)
    (dest / "site.toml").write_text(
        '[site]\nname = "Bench"\nbase_url = "https://bench.example.org"\noutput = "public"\n'
        '[render]\ndefault = "build"\n')
    (dest / "content/_index.md").write_text('+++\ntitle = "Bench"\n+++\nArticles.\n')
    (dest / "content/articles/_index.md").write_text(
        '+++\ntitle = "Articles"\npaginate = 10\nsort_by = "date"\n+++\n')
    (dest / "templates/topic.html").write_text(
        '{% extends "base.html" %}{% block main %}<h1>{{ page.title }}</h1><ul>'
        '{% for p in section_pages("articles") if page.title in p.tags %}'
        '<li><a href="{{ p.url }}">{{ p.title }}</a></li>{% endfor %}</ul>{% endblock %}\n')
    topics = set()
    for a in json.loads((WORK / "data" / f"articles-{n}.json").read_text()):
        topics.update(a["topics"])
        fm = (f"+++\ntitle = {toml_str(a['title'])}\ndescription = {toml_str(a['summary'])}\n"
              f"date = {a['date']}\ntags = {json.dumps(a['topics'])}\n+++\n\n")
        (dest / "content/articles" / f"{a['slug']}.md").write_text(fm + a["body"] + "\n")
    for t in sorted(topics):
        (dest / "content/topics" / f"{t.lower()}.md").write_text(
            f'+++\ntitle = {toml_str(t)}\ntemplate = "topic.html"\n+++\n')


def markdown_cold_build(cores=1):
    """`engine build` of the Markdown site (no database), three times."""
    cpus = tool_cpus("wrought-bench-engine-tool", cores)
    env = {"ENGINE_BUILD_JOBS": str(cores), "ENGINE_BUILD_PIN_CPUS": "1"}
    times = []
    out = ""
    for _ in range(3):
        t0 = time.monotonic()
        r = engine_exec(["build"], "", site="/site-1000/markdown", env=env)
        times.append(time.monotonic() - t0)
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-engine-tool", 1)
    pages = re.search(r"(\d+) pages?", out)
    log(f"wrought (markdown, {cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None}


def setup_wordpress():
    compose("up", "-d", "wp-db", "wp")
    wait_http("http://127.0.0.1:58201/wp-admin/install.php", "wordpress", ok=(200, 302))
    cli = ["run", "--rm", "wp-cli", "wp"]
    compose(*cli, "core", "install", "--url=http://127.0.0.1:58201", "--title=Bench",
            "--admin_user=admin", f"--admin_password={ADMIN_PW}",
            "--admin_email=admin@bench.test", "--skip-email")
    compose(*cli, "rewrite", "structure", "/%postname%/")
    htaccess = (
        "# BEGIN WordPress\n<IfModule mod_rewrite.c>\nRewriteEngine On\n"
        "RewriteRule .* - [E=HTTP_AUTHORIZATION:%{HTTP:Authorization}]\nRewriteBase /\n"
        "RewriteRule ^index\\.php$ - [L]\nRewriteCond %{REQUEST_FILENAME} !-f\n"
        "RewriteCond %{REQUEST_FILENAME} !-d\nRewriteRule . /index.php [L]\n</IfModule>\n"
        "# END WordPress\n"
    )
    compose("run", "--rm", "--entrypoint", "sh", "wp-cli", "-c",
            "cat > /var/www/html/.htaccess", input=htaccess)
    r = compose(*cli, "eval-file", "/bench/wordpress/import.php",
                f"/bench/data/articles-{N_RUNTIME}.json")
    log(f"wordpress: {r.stdout.strip()}")
    # Application passwords (for the REST edit) need HTTPS unless the site
    # says it is a local one; that changes nothing else.
    compose(*cli, "config", "set", "WP_ENVIRONMENT_TYPE", "local")
    r = compose(*cli, "user", "application-password", "create", "admin", "bench", "--porcelain")
    app_pw = r.stdout.strip().splitlines()[-1]
    # WP-Cron runs on page views by calling the site's own URL; from inside
    # the container that URL (127.0.0.1:58201) is not reachable, so due
    # events would be retried on every view (a database write each time).
    # Run them once, as a reachable site does by itself.
    compose(*cli, "cron", "event", "run", "--due-now")
    wait_http("http://127.0.0.1:58201/article-17/", "wordpress article")
    return app_pw


def drush(*args, input=None):
    return compose("exec", "-T", "-u", "www-data", "drupal", "drush", *args, input=input)


def setup_drupal():
    compose("up", "-d", "--build", "drupal-db", "drupal")
    wait_http("http://127.0.0.1:58202/core/install.php", "drupal", ok=(200, 302))
    for _ in range(30):
        r = compose("exec", "-T", "drupal-db", "pg_isready", "-U", "drupal", check=False)
        if r.returncode == 0:
            break
        time.sleep(1)
    drush("site:install", "standard",
          "--db-url=pgsql://drupal:drupal@drupal-db:5432/drupal",
          "--site-name=Bench", "--account-name=admin", f"--account-pass={ADMIN_PW}", "-y")
    r = drush("php:script", "/bench/drupal/import.php", f"/bench/data/articles-{N_RUNTIME}.json")
    log(f"drupal: {r.stdout.strip()}")
    drush("config:set", "search.settings", "index.cron_limit", "1000", "-y")
    drush("cron")
    drush("pm:enable", "jsonapi", "basic_auth", "-y")
    drush("config:set", "jsonapi.settings", "read_only", "0", "-y")
    drush("cache:rebuild")
    wait_http("http://127.0.0.1:58202/article-17", "drupal article")


def setup_astro():
    astro_site(WORK / "astro", N_RUNTIME)
    astro_site(WORK / "astro-1000", N_BUILD)
    compose("run", "-d", "--name", "wrought-bench-astro-tool", "astro", "sleep", "infinity")
    for d in ("astro", "astro-1000"):
        sh(["docker", "exec", "-w", f"/work/{d}", "wrought-bench-astro-tool",
            "npm", "install", "--no-audit", "--no-fund", "--loglevel=error"])
    sh(["docker", "exec", "-w", "/work/astro", "wrought-bench-astro-tool", "npm", "run", "build"])
    return astro_cold_build(1)


def astro_cold_build(cores=1):
    """`astro build` of 1000 articles, three times, on `cores` cores."""
    cpus = tool_cpus("wrought-bench-astro-tool", cores)
    times = []
    out = ""
    for _ in range(3):
        t0 = time.monotonic()
        r = sh(["docker", "exec", "-w", "/work/astro-1000", "wrought-bench-astro-tool",
                "npm", "run", "build"])
        times.append(time.monotonic() - t0)
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-astro-tool", 1)
    pages = re.search(r"(\d+) page\(s\) built", out)
    log(f"astro ({cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None}


# ----- edit to published -----------------------------------------------------------


def until_contains(url, text, start, timeout=60):
    while time.monotonic() - start < timeout:
        status, body, _ = http("GET", url, timeout=10)
        if status == 200 and text in body:
            return time.monotonic() - start
        time.sleep(0.005)
    raise SystemExit(f"{url} never showed {text!r}")


def edit_wrought(token, label):
    out = []
    for i in range(5):
        slug = f"article-{i + 5}"
        url = f"http://127.0.0.1:58200/api/v1/content/article/{slug}"
        _, body, h = http("GET", url, headers={"Authorization": f"Bearer {token}"})
        version = json.loads(body)["version"]
        title = f"Edited {label} {i} {time.time_ns()}"
        start = time.monotonic()
        status, body, _ = http(
            "PATCH", url, json.dumps({"fields": {"title": title}}).encode(),
            {"Authorization": f"Bearer {token}", "If-Match": f'"{version}"',
             "Content-Type": "application/json"},
        )
        assert status == 200, body
        out.append(until_contains(f"http://127.0.0.1:58200/articles/{slug}/", title, start))
    return out


def edit_wordpress(app_pw):
    auth = "Basic " + base64.b64encode(f"admin:{app_pw}".encode()).decode()
    out = []
    for i in range(5):
        slug = f"article-{i + 5}"
        _, body, _ = http("GET", f"http://127.0.0.1:58201/wp-json/wp/v2/posts?slug={slug}")
        pid = json.loads(body)[0]["id"]
        title = f"Edited wp {i} {time.time_ns()}"
        start = time.monotonic()
        status, body, _ = http(
            "POST", f"http://127.0.0.1:58201/wp-json/wp/v2/posts/{pid}",
            json.dumps({"title": title}).encode(),
            {"Authorization": auth, "Content-Type": "application/json"},
        )
        assert status == 200, body
        out.append(until_contains(f"http://127.0.0.1:58201/{slug}/", title, start))
    return out


def edit_drupal():
    auth = "Basic " + base64.b64encode(f"admin:{ADMIN_PW}".encode()).decode()
    out = []
    for i in range(5):
        nid = i + 5
        _, body, _ = http(
            "GET", f"http://127.0.0.1:58202/jsonapi/node/article?filter[drupal_internal__nid]={nid}",
            headers={"Authorization": auth},
        )
        uuid = json.loads(body)["data"][0]["id"]
        title = f"Edited drupal {i} {time.time_ns()}"
        start = time.monotonic()
        status, body, _ = http(
            "PATCH", f"http://127.0.0.1:58202/jsonapi/node/article/{uuid}",
            json.dumps({"data": {"type": "node--article", "id": uuid,
                                 "attributes": {"title": title}}}).encode(),
            {"Authorization": auth, "Content-Type": "application/vnd.api+json",
             "Accept": "application/vnd.api+json"},
        )
        assert status == 200, body
        out.append(until_contains(f"http://127.0.0.1:58202/article-{nid}", title, start))
    return out


def edit_astro():
    out = []
    for i in range(3):
        p = WORK / "astro/src/content/article-5.md"
        title = f"Edited astro {i} {time.time_ns()}"
        text = re.sub(r'^title: .*$', f"title: {json.dumps(title)}", p.read_text(), count=1, flags=re.M)
        start = time.monotonic()
        p.write_text(text)
        sh(["docker", "exec", "-w", "/work/astro", "wrought-bench-astro-tool", "npm", "run", "build"])
        out.append(until_contains("http://127.0.0.1:58204/articles/article-5/", title, start))
    return out


# ----- memory and sizes ---------------------------------------------------------------


def mem_mib():
    r = sh(["docker", "stats", "--no-stream", "--format", "{{.Name}} {{.MemUsage}}"])
    out = {}
    for line in r.stdout.splitlines():
        name, usage = line.split(" ", 1)
        if not name.startswith("wrought-bench-"):
            continue
        m = re.match(r"([\d.]+)\s*([KMG]i?B)", usage)
        if m:
            v = float(m.group(1)) * {"KiB": 1 / 1024, "KB": 1 / 1024, "MiB": 1, "MB": 1,
                                     "GiB": 1024, "GB": 1024}[m.group(2)]
            out[name] = round(v, 1)
    return out


def image_info(name):
    r = sh(["docker", "image", "inspect", name, "--format", "{{.Size}} {{index .RepoDigests 0}}"],
           check=False)
    if r.returncode != 0:
        r = sh(["docker", "image", "inspect", name, "--format", "{{.Size}} -"], check=False)
    if r.returncode != 0:
        return None
    size, digest = r.stdout.strip().split(" ", 1)
    return {"mb": round(int(size) / 1e6, 1), "digest": digest}


def du_mb(path):
    r = sh(["du", "-sb", str(path)], check=False)
    return round(int(r.stdout.split()[0]) / 1e6, 1) if r.returncode == 0 else None


def hardware():
    def first(cmd, pat):
        r = sh(cmd, check=False)
        m = re.search(pat, r.stdout, re.M)
        return m.group(1).strip() if m else None
    mem_kb = first(["grep", "MemTotal", "/proc/meminfo"], r"(\d+)")
    return {
        "cpu": first(["lscpu"], r"^Model name:\s+(.*)$"),
        "cpus": os.cpu_count(),
        "memory_gb": round(int(mem_kb) / 1024 / 1024, 1) if mem_kb else None,
        "kernel": platform.release(),
        "os": first(["cat", "/etc/os-release"], r'^PRETTY_NAME="(.*)"'),
        "docker": first(["docker", "version", "--format", "{{.Server.Version}}"], r"(.+)"),
        "rustc": first(["rustc", "--version"], r"(.+)"),
    }


# ----- main --------------------------------------------------------------------------------


def clean_work():
    """Remove bench/work (containers leave root-owned files in it)."""
    if WORK.exists():
        sh(["docker", "run", "--rm", "-v", f"{WORK}:/w", "alpine:3.22.8",
            "sh", "-c", "rm -rf /w/* /w/.[!.]*"], check=False)
        shutil.rmtree(WORK, ignore_errors=True)


def build_engines():
    sizes = {}
    log("building the engine: static preset")
    sh(["cargo", "build", "--release", "-p", "engine-cli"], cwd=ROOT, capture=False)
    sizes["static"] = (ROOT / "target/release/engine").stat().st_size
    log("building the engine: app preset")
    sh(["cargo", "build", "--release", "-p", "engine-cli", "--features", "app"], cwd=ROOT,
       capture=False)
    sizes["app"] = (ROOT / "target/release/engine").stat().st_size
    shutil.copy2(ROOT / "target/release/engine", HERE / "wrought/engine")
    return {k: round(v / 1e6, 1) for k, v in sizes.items()}


def cold_builds(pg, astro_setup):
    """Cold builds on one core (as every other measurement) and on four."""
    lite1000 = "sqlite:///site-1000/data/site.db"
    markdown_site(WORK / "wrought-site-1000/markdown", N_BUILD)
    one = {
        "wrought_markdown": markdown_cold_build(1),
        "wrought_postgres": wrought_cold_build(pg + "1000", "postgres"),
        "wrought_sqlite": wrought_cold_build(lite1000, "sqlite"),
        "astro": astro_setup(),
    }
    four = {
        "wrought_markdown": markdown_cold_build(4),
        "wrought_postgres": wrought_cold_build(pg + "1000", "postgres", cores=4),
        "wrought_sqlite": wrought_cold_build(lite1000, "sqlite", cores=4),
        "astro": astro_cold_build(4),
    }
    return one, four


def start_wrought_tools():
    compose("up", "-d", "wrought-db")
    compose("--profile", "tools", "up", "-d", "--no-deps", "wrought-tool")
    time.sleep(3)
    for _ in range(60):
        if compose("exec", "-T", "wrought-db", "pg_isready", "-U", "wrought", check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)


def build_only():
    """`bench/run.sh build`: re-measure only the cold builds and update them
    in results.json (every other number is kept, with its own date)."""
    path = RESULTS / "results.json"
    res = json.loads(path.read_text()) if path.exists() else {}
    clean_work()
    WORK.mkdir(exist_ok=True)
    make_data()
    res["binary_mb"] = build_engines()
    wrought_site(WORK / "wrought-site", N_RUNTIME)
    wrought_site(WORK / "wrought-site-1000", N_BUILD)
    compose("build", "wrought", capture=False)
    compose("--profile", "tools", "pull", "--quiet", "wrought-db", "astro", capture=False)
    start_wrought_tools()
    pg = "postgres://wrought:wrought@wrought-db:5432/wrought"
    res["cold_build"], res["cold_build_4_cores"] = cold_builds(pg, setup_astro)
    res["cold_build_date"] = time.strftime("%Y-%m-%d")
    res.setdefault("versions", {})["wrought"] = sh(
        [str(HERE / "wrought/engine"), "--version"]).stdout.strip().splitlines()[0]
    path.write_text(json.dumps(res, indent=1))
    log(f"updated the cold builds in {path}")


# A tuning profile for the 1 GB database container (the docs' profile scaled
# to its memory): see docs/content/docs/databases.md.
PG_TUNED = (
    "-c shared_buffers=256MB -c effective_cache_size=768MB -c work_mem=8MB "
    "-c maintenance_work_mem=64MB -c random_page_cost=1.1 -c effective_io_concurrency=200 "
    "-c wal_compression=on -c checkpoint_completion_target=0.9 -c max_wal_size=2GB"
)


def pg_url(db, socket=False):
    if socket:
        return f"postgres://wrought:wrought@localhost/{db}?host=/run/postgresql"
    return f"postgres://wrought:wrought@wrought-db:5432/{db}"


def restart_db(args):
    """Recreate the database container with `args` (data volume kept)."""
    compose("up", "-d", "--force-recreate", "--no-deps", "wrought-db",
            env={"BENCH_PG_ARGS": args})
    for _ in range(60):
        if compose("exec", "-T", "wrought-db", "pg_isready", "-U", "wrought",
                   check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)
    compose("exec", "-T", "wrought-db", "psql", "-U", "wrought", "-d", "postgres",
            "-c", "DROP DATABASE IF EXISTS wrought WITH (FORCE)", "-c", "CREATE DATABASE wrought")


def pg_compare(label):
    """`bench/run.sh pg <label>`: the database-bound numbers for one engine
    binary (BENCH_ENGINE, else a fresh release build): cold builds of 1000
    articles from Markdown, SQLite and PostgreSQL (stock, tuned, tuned over
    the Unix socket) on one and four cores, and the dynamic search page on
    each PostgreSQL setup. Writes results/pg-<label>.json."""
    res = {"date": time.strftime("%Y-%m-%d"), "label": label, "builds": {}, "dynamic": {}}
    clean_work()
    WORK.mkdir(exist_ok=True)
    make_data()
    engine = os.environ.get("BENCH_ENGINE")
    if engine:
        shutil.copy2(engine, HERE / "wrought/engine")
    else:
        build_engines()
    res["engine"] = sh([str(HERE / "wrought/engine"), "--version"]).stdout.strip().splitlines()[0]
    wrought_site(WORK / "wrought-site", N_RUNTIME)
    wrought_site(WORK / "wrought-site-1000", N_BUILD)
    markdown_site(WORK / "wrought-site-1000/markdown", N_BUILD)
    compose("build", "wrought", "loadgen", capture=False)
    compose("--profile", "tools", "pull", "--quiet", "wrought-db", capture=False)
    start_wrought_tools()
    b = res["builds"]
    b["wrought_markdown"] = {c: markdown_cold_build(c) for c in (1, 4)}
    lite1000 = "sqlite:///site-1000/data/site.db"
    b["wrought_sqlite"] = {c: wrought_cold_build(lite1000, "sqlite", c) for c in (1, 4)}
    for cfg, args, socket in (("stock", "", False), ("tuned", PG_TUNED, False),
                              ("tuned_socket", PG_TUNED, True)):
        restart_db(args)
        b[f"wrought_postgres_{cfg}"] = {
            c: wrought_cold_build(pg_url("wrought1000", socket), f"postgres {cfg}", c)
            for c in (1, 4)}
        setup_wrought(pg_url("wrought", socket), f"postgres {cfg}")
        log(f"dynamic: wrought postgres {cfg}")
        res["dynamic"][f"wrought_postgres_{cfg}"] = wrk(
            "http://127.0.0.1:58200/search/?q=WORD", vary=True)
    if os.environ.get("BENCH_ASTRO"):
        res["builds"]["astro"] = {1: setup_astro(), 4: astro_cold_build(4)}
    out = RESULTS / f"pg-{label}.json"
    out.write_text(json.dumps(res, indent=1))
    log(f"wrote {out}")


def main():
    RESULTS.mkdir(exist_ok=True)
    clean_work()
    WORK.mkdir(exist_ok=True)
    res = {"date": time.strftime("%Y-%m-%d"), "hardware": hardware(),
           "settings": {"threads": THREADS, "connections": CONNS, "warmup": WARM,
                        "measured": MEASURE, "articles": N_RUNTIME, "build_articles": N_BUILD,
                        "placement": "one dedicated core per role: application, database, "
                                     "tools, load generator (isolcpus host); 2 GB per application, "
                                     "1 GB per database"}}
    make_data()
    res["binary_mb"] = build_engines()
    wrought_site(WORK / "wrought-site", N_RUNTIME)
    wrought_site(WORK / "wrought-site-1000", N_BUILD)
    log("building images")
    compose("build", "wrought", "drupal", "loadgen", capture=False)
    compose("--profile", "tools", "pull", "--quiet", "wrought-db", "caddy", "wp", "wp-db",
            "wp-cli", "drupal-db", "astro", capture=False)
    start_wrought_tools()

    pg = "postgres://wrought:wrought@wrought-db:5432/wrought"
    lite = "sqlite:///site/data/site.db"
    tok_pg = setup_wrought(pg, "postgres")
    res["cold_build"], res["cold_build_4_cores"] = cold_builds(pg, setup_astro)
    app_pw = setup_wordpress()
    setup_drupal()
    compose("up", "-d", "caddy")
    wait_http("http://127.0.0.1:58203/articles/article-17/", "caddy (wrought)")
    wait_http("http://127.0.0.1:58204/articles/article-17/", "caddy (astro)")

    static_targets = {
        "wrought_engine": "http://127.0.0.1:58200/articles/article-17/",
        "wrought_caddy": "http://127.0.0.1:58203/articles/article-17/",
        "astro_caddy": "http://127.0.0.1:58204/articles/article-17/",
        "wordpress": "http://127.0.0.1:58201/article-17/",
        "drupal": "http://127.0.0.1:58202/article-17",
    }
    res["page_bytes"] = {k: page_bytes(u) for k, u in static_targets.items()}
    # Drupal's internal page cache: the second view should be a HIT.
    http("GET", static_targets["drupal"])
    res["drupal_cache_header"] = http("GET", static_targets["drupal"])[2].get("X-Drupal-Cache")
    res["static"] = {}
    for k, u in static_targets.items():
        log(f"static: {k}")
        res["static"][k] = wrk(u)
    dyn = {
        "wrought_postgres": "http://127.0.0.1:58200/search/?q=WORD",
        "wordpress": "http://127.0.0.1:58201/?s=WORD",
        "drupal": "http://127.0.0.1:58202/search/node?keys=WORD",
    }
    res["dynamic"] = {}
    for k, u in dyn.items():
        log(f"dynamic: {k}")
        res["dynamic"][k] = wrk(u, vary=True)
    res["memory_mib"] = mem_mib()

    log("edit to published")
    res["edit_s"] = {
        "wrought_postgres": edit_wrought(tok_pg, "pg"),
        "wordpress": edit_wordpress(app_pw),
        "drupal": edit_drupal(),
        "astro": edit_astro(),
    }

    # Wrought on SQLite: the same site, served from a database file.
    tok_lite = setup_wrought(lite, "sqlite")
    log("dynamic: wrought_sqlite")
    res["dynamic"]["wrought_sqlite"] = wrk(dyn["wrought_postgres"], vary=True)
    res["memory_mib_sqlite"] = mem_mib().get("wrought-bench-engine")
    res["edit_s"]["wrought_sqlite"] = edit_wrought(tok_lite, "sqlite")

    res["images"] = {
        "wrought": image_info("wrought-bench-engine"),
        "postgres": image_info("postgres:17.12"),
        "wordpress": image_info("wordpress:7.1.3-php8.4-apache"),
        "mariadb": image_info("mariadb:11.4.13"),
        "drupal": image_info("wrought-bench-drupal"),
        "node": image_info("node:22.23.5-alpine3.24"),
        "caddy": image_info("caddy:2.11.8-alpine"),
        "wrk": image_info("wrought-bench-loadgen"),
    }
    res["disk_mb"] = {
        "wrought_public": du_mb(WORK / "wrought-site/public"),
        "astro_dist": du_mb(WORK / "astro/dist"),
        "astro_node_modules": du_mb(WORK / "astro/node_modules"),
    }
    res["versions"] = {
        "wrought": sh([str(HERE / "wrought/engine"), "--version"]).stdout.strip().splitlines()[0],
        "astro": "7.4.10", "wordpress": "7.1.3 (php8.4-apache)", "mariadb": "11.4.13",
        "drupal": "11.3.23 (php8.4-apache-bookworm)", "postgres": "17.12",
        "node": "22.23.5", "caddy": "2.11.8", "wrk": "alpine 3.22.8 package",
    }
    (RESULTS / "results.json").write_text(json.dumps(res, indent=1))
    log(f"wrote {RESULTS / 'results.json'}")


def teardown():
    if os.environ.get("BENCH_KEEP"):
        log("BENCH_KEEP set: leaving the containers up")
        return
    sh(["docker", "rm", "-f", "wrought-bench-astro-tool"], check=False)
    rmi = os.environ.get("BENCH_RMI", "local")
    compose("--profile", "tools", "down", "-v", "--remove-orphans", "--rmi", rmi, check=False)
    clean_work()
    log("removed the containers, volumes and bench/work")


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["build"]:
            build_only()
        elif sys.argv[1:2] == ["pg"]:
            pg_compare(sys.argv[2] if len(sys.argv) > 2 else "now")
        else:
            main()
    finally:
        teardown()
