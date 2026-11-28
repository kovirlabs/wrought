#!/usr/bin/env python3
"""The benchmark: Wrought against WordPress, Drupal, Astro, Phoenix, Loco,
Rails, Hugo and Zola on equivalent content.

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


# The same page shell as Astro's and Wrought's bench sites: one stylesheet,
# a nav, the article with its date and topics; a list paginated by 10, one
# page per topic, a home page. Hugo and Zola write their own extras (Hugo:
# an RSS feed per list and a sitemap; Zola: a sitemap); neither builds a
# search or tag index the way Wrought's database build does.
SHELL_HEAD = ('<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
              '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
              '<title>{title} · Bench</title>\n<link rel="stylesheet" href="/tagbrush.min.css">\n'
              '</head>\n<body>\n<header><nav aria-label="Site"><ul><li><a href="/">Bench</a></li>'
              '<li><a href="/articles/">Articles</a></li></ul></nav></header>\n<main id="main">\n')
SHELL_FOOT = '</main>\n<footer><p>Benchmark site</p></footer>\n</body>\n</html>\n'


def hugo_site(dest, n):
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "content/articles").mkdir(parents=True)
    (dest / "layouts/_default").mkdir(parents=True)
    (dest / "static").mkdir(parents=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 dest / "static/tagbrush.min.css")
    (dest / "hugo.toml").write_text(
        'baseURL = "https://bench.example.org/"\ntitle = "Bench"\nlanguageCode = "en"\n'
        'disableKinds = ["RSS"]\n[pagination]\npagerSize = 10\n'
        '[taxonomies]\ntopic = "topics"\n[markup.highlight]\ncodeFences = false\n')
    (dest / "content/articles/_index.md").write_text("---\ntitle: Articles\n---\n")
    for a in json.loads((WORK / "data" / f"articles-{n}.json").read_text()):
        fm = (f"---\ntitle: {json.dumps(a['title'])}\nsummary: {json.dumps(a['summary'])}\n"
              f"date: \"{a['date']}\"\ntopics: {json.dumps(a['topics'])}\n---\n\n")
        (dest / "content/articles" / f"{a['slug']}.md").write_text(fm + a["body"] + "\n")
    head = SHELL_HEAD.replace("{title}", "{{ .Title }}")
    lay = dest / "layouts/_default"
    (lay / "baseof.html").write_text(head + '{{ block "main" . }}{{ end }}' + SHELL_FOOT)
    (lay / "single.html").write_text(
        '{{ define "main" }}<article>\n<h1>{{ .Title }}</h1>\n<p><small><time datetime="'
        '{{ .Date.Format "2006-01-02" }}">{{ .Date.Format "2006-01-02" }}</time></small></p>\n'
        '{{ .Content }}\n<p>Topics: {{ range $i, $t := .Params.topics }}{{ if $i }}, {{ end }}'
        '<a href="/topics/{{ lower $t }}/">{{ $t }}</a>{{ end }}</p>\n</article>{{ end }}\n')
    (lay / "list.html").write_text(
        '{{ define "main" }}<h1>{{ .Title }}</h1>\n<ul>{{ range (.Paginate .Pages).Pages }}'
        '<li><a href="{{ .RelPermalink }}">{{ .Title }}</a></li>{{ end }}</ul>\n'
        '<nav aria-label="Pages"><ul>{{ with .Paginator.Prev }}<li><a href="{{ .URL }}">Previous'
        '</a></li>{{ end }}{{ with .Paginator.Next }}<li><a href="{{ .URL }}">Next</a></li>'
        '{{ end }}</ul></nav>{{ end }}\n')
    (lay / "taxonomy.html").write_text(
        '{{ define "main" }}<h1>{{ .Title }}</h1>\n<ul>{{ range .Pages }}<li><a href="'
        '{{ .RelPermalink }}">{{ .Title }}</a></li>{{ end }}</ul>{{ end }}\n')
    (lay / "term.html").write_text(
        '{{ define "main" }}<h1>{{ .Title }}</h1>\n<ul>{{ range .Pages }}<li>{{ .Title }}</li>'
        '{{ end }}</ul>{{ end }}\n')
    (dest / "layouts/index.html").write_text(
        '{{ define "main" }}<h1>Bench</h1>\n<ul>{{ range first 10 (where .Site.RegularPages '
        '"Section" "articles") }}<li><a href="{{ .RelPermalink }}">{{ .Title }}</a></li>{{ end }}'
        '</ul>{{ end }}\n')


def zola_site(dest, n):
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "content/articles").mkdir(parents=True)
    (dest / "templates").mkdir(parents=True)
    (dest / "static").mkdir(parents=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 dest / "static/tagbrush.min.css")
    (dest / "config.toml").write_text(
        'base_url = "https://bench.example.org"\ntitle = "Bench"\ncompile_sass = false\n'
        'build_search_index = false\ngenerate_feeds = false\n'
        'taxonomies = [{ name = "topics" }]\n')
    (dest / "content/_index.md").write_text('+++\ntitle = "Bench"\n+++\n')
    (dest / "content/articles/_index.md").write_text(
        '+++\ntitle = "Articles"\nsort_by = "date"\npaginate_by = 10\n+++\n')
    for a in json.loads((WORK / "data" / f"articles-{n}.json").read_text()):
        fm = (f"+++\ntitle = {toml_str(a['title'])}\ndescription = {toml_str(a['summary'])}\n"
              f"date = {a['date']}\n[taxonomies]\ntopics = {json.dumps(a['topics'])}\n+++\n\n")
        (dest / "content/articles" / f"{a['slug']}.md").write_text(fm + a["body"] + "\n")
    t = dest / "templates"
    head = SHELL_HEAD.replace("{title}", "{% block title %}Bench{% endblock title %}")
    (t / "base.html").write_text(head + "{% block main %}{% endblock main %}" + SHELL_FOOT)
    (t / "page.html").write_text(
        '{% extends "base.html" %}{% block title %}{{ page.title }}{% endblock title %}'
        '{% block main %}<article>\n<h1>{{ page.title }}</h1>\n<p><small><time datetime="'
        '{{ page.date }}">{{ page.date }}</time></small></p>\n{{ page.content | safe }}\n'
        '<p>Topics: {% for t in page.taxonomies.topics %}{% if not loop.first %}, {% endif %}'
        '<a href="/topics/{{ t | slugify }}/">{{ t }}</a>{% endfor %}</p>\n</article>'
        '{% endblock main %}\n')
    (t / "section.html").write_text(
        '{% extends "base.html" %}{% block title %}{{ section.title }}{% endblock title %}'
        '{% block main %}<h1>{{ section.title }}</h1>\n<ul>{% for p in paginator.pages %}'
        '<li><a href="{{ p.permalink | safe }}">{{ p.title }}</a></li>{% endfor %}</ul>\n'
        '<nav aria-label="Pages"><ul>{% if paginator.previous %}<li><a href="'
        '{{ paginator.previous }}">Previous</a></li>{% endif %}{% if paginator.next %}<li><a href="'
        '{{ paginator.next }}">Next</a></li>{% endif %}</ul></nav>{% endblock main %}\n')
    (t / "index.html").write_text(
        '{% extends "base.html" %}{% block main %}<h1>Bench</h1>\n<ul>'
        '{% set s = get_section(path="articles/_index.md") %}{% for p in s.pages | slice(end=10) %}'
        '<li><a href="{{ p.permalink | safe }}">{{ p.title }}</a></li>{% endfor %}</ul>'
        '{% endblock main %}\n')
    (t / "taxonomy_list.html").write_text(
        '{% extends "base.html" %}{% block main %}<h1>Topics</h1>\n<ul>{% for term in terms %}'
        '<li><a href="{{ term.permalink | safe }}">{{ term.name }}</a></li>{% endfor %}</ul>'
        '{% endblock main %}\n')
    (t / "taxonomy_single.html").write_text(
        '{% extends "base.html" %}{% block title %}{{ term.name }}{% endblock title %}'
        '{% block main %}<h1>{{ term.name }}</h1>\n<ul>{% for p in term.pages %}<li>{{ p.title }}'
        '</li>{% endfor %}</ul>{% endblock main %}\n')


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
    times, phases, peaks = [], [], []
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
        # Cold: no previous output (whose compressed copies a build reuses).
        sh(["docker", "exec", "wrought-bench-engine-tool", "rm", "-rf", f"{site}/public"])
        with DiskProbe() as probe, PeakAnon("wrought-bench-engine-tool") as pk:
            t0 = time.monotonic()
            r = engine_exec(["build", "--timings"], db_url, site=site, env=env)
            times.append(time.monotonic() - t0)
        peaks.append(round(pk.peak / 2**20, 1))
        phases.append({**build_phases(r.stderr), "disk_sync": probe.summary()})
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-engine-tool", 1)
    pages = re.search(r"(\d+) pages?", out)
    log(f"wrought ({label}, {cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None, "phases_ms": phases,
            "peak_mib": max(peaks) if peaks else None,
            "output_mb": du_mb(WORK / ("wrought-site-1000/public"))}


class DiskProbe:
    """Write 8 KiB and fdatasync it every 200 ms in bench/work while a
    measurement runs: the disk's own sync latency, independent of the
    systems measured, so a slow run can be told apart from a slow disk."""

    def __init__(self):
        import threading
        self.samples = []
        self._stop = threading.Event()
        self._t = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        path = WORK / ".disk-probe"
        fd = os.open(path, os.O_WRONLY | os.O_CREAT, 0o600)
        buf = os.urandom(8192)
        i = 0
        try:
            while not self._stop.wait(0.2):
                os.pwrite(fd, buf, (i % 256) * 8192)
                t0 = time.monotonic()
                os.fdatasync(fd)
                self.samples.append((time.monotonic() - t0) * 1000)
                i += 1
        finally:
            os.close(fd)
            path.unlink(missing_ok=True)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        self._t.join()

    def summary(self):
        s = sorted(self.samples)
        if not s:
            return None
        return {"syncs": len(s), "p50_ms": round(s[len(s) // 2], 1),
                "p99_ms": round(s[min(len(s) - 1, int(len(s) * 0.99))], 1),
                "max_ms": round(s[-1], 1), "over_1s": sum(x > 1000 for x in s)}


def build_phases(stderr):
    """The top-level phases of `engine build --timings` (milliseconds)."""
    out = {}
    for line in stderr.splitlines():
        m = re.match(r"^  (\S.*?)\s{2,}([\d.]+) ms", line)
        if m:
            out.setdefault(m.group(1).strip(), float(m.group(2)))
    return out


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


def markdown_cold_build(cores=1, copies=True):
    """`engine build` of the Markdown site (no database), three times;
    `copies=False` without the pre-compressed copies (what they cost)."""
    cpus = tool_cpus("wrought-bench-engine-tool", cores)
    env = {"ENGINE_BUILD_JOBS": str(cores), "ENGINE_BUILD_PIN_CPUS": "1"}
    if not copies:
        env["ENGINE_BUILD_PRECOMPRESS"] = "0"
    times, phases, peaks = [], [], []
    out = ""
    for _ in range(3):
        sh(["docker", "exec", "wrought-bench-engine-tool", "rm", "-rf",
            "/site-1000/markdown/public"])
        with PeakAnon("wrought-bench-engine-tool") as pk:
            t0 = time.monotonic()
            r = engine_exec(["build", "--timings"], "", site="/site-1000/markdown", env=env)
            times.append(time.monotonic() - t0)
        peaks.append(round(pk.peak / 2**20, 1))
        phases.append(build_phases(r.stderr))
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-engine-tool", 1)
    pages = re.search(r"(\d+) pages?", out)
    log(f"wrought (markdown{'' if copies else ', no copies'}, {cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None, "phases_ms": phases,
            "peak_mib": max(peaks), "output_mb": du_mb(WORK / "wrought-site-1000/markdown/public")}


RAILS = "http://127.0.0.1:58205"


def start_rails(caching):
    """(Re)start the Rails server; returns seconds until it answered."""
    env = {"BENCH_RAILS_CACHING": "1" if caching else "0"}
    compose("stop", "rails", check=False)
    t0 = time.monotonic()
    compose("up", "-d", "--force-recreate", "--no-deps", "rails", env=env)
    wait_http(f"{RAILS}/up", "rails", timeout=300)
    return time.monotonic() - t0


def setup_rails():
    """Ruby on Rails, production mode: databases, the same 200 articles."""
    (HERE / "rails/public").mkdir(exist_ok=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 HERE / "rails/public/tagbrush.min.css")
    compose("build", "rails", capture=False)
    compose("up", "-d", "rails-db")
    for _ in range(60):
        if compose("exec", "-T", "rails-db", "pg_isready", "-U", "rails", check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)
    compose("run", "--rm", "--no-deps", "rails", "bin/rails", "db:prepare")
    r = compose("run", "--rm", "--no-deps", "rails", "bin/rails",
                f"bench:seed[/bench/data/articles-{N_RUNTIME}.json]")
    log(f"rails: {r.stdout.strip().splitlines()[-1]}")


def cold_start(container, url):
    sh(["docker", "stop", container])
    t0 = time.monotonic()
    sh(["docker", "start", container])
    wait_http(url, container, timeout=300)
    return time.monotonic() - t0


def wait_pg(service, user):
    for _ in range(60):
        if compose("exec", "-T", service, "pg_isready", "-U", user, check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)


def setup_phoenix():
    """Phoenix, MIX_ENV=prod release: migrate and seed with the release's
    own commands (`bin/migrate`, `bin/bench eval`), then start `bin/server`."""
    (HERE / "phoenix/priv/static/assets").mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 HERE / "phoenix/priv/static/assets/tagbrush.min.css")
    compose("build", "phoenix", capture=False)
    compose("up", "-d", "phoenix-db")
    wait_pg("phoenix-db", "phoenix")
    compose("run", "--rm", "--no-deps", "phoenix", "/app/bin/migrate")
    r = compose("run", "--rm", "--no-deps", "phoenix", "/app/bin/bench", "eval",
                f'Bench.Release.seed("/bench/data/articles-{N_RUNTIME}.json")')
    log(f"phoenix: {r.stdout.strip().splitlines()[-1]}")
    compose("up", "-d", "--no-deps", "phoenix")
    wait_http(f"{APPS['phoenix']['base']}/up", "phoenix")


def setup_loco():
    """Loco, release build, `-e production`: migrate and seed with the app's
    own CLI (`bench-cli db migrate`, the bench's `seed_bench` task)."""
    (HERE / "loco/assets/static").mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "crates/engine-core/vendor/tagbrush/tagbrush.min.css",
                 HERE / "loco/assets/static/tagbrush.min.css")
    compose("build", "loco", capture=False)
    compose("up", "-d", "loco-db")
    wait_pg("loco-db", "loco")
    compose("run", "--rm", "--no-deps", "loco", "bench-cli", "db", "migrate", "-e", "production")
    r = compose("run", "--rm", "--no-deps", "loco", "bench-cli", "task", "seed_bench",
                f"path:/bench/data/articles-{N_RUNTIME}.json", "-e", "production")
    log(f"loco: {r.stdout.strip().splitlines()[-1]}")
    compose("up", "-d", "--no-deps", "loco")
    wait_http(f"{APPS['loco']['base']}/_ping", "loco")


# Application frameworks that render every page per request, measured the
# same way: article, listing and search pages under the standard load,
# memory of the application and its database (separately; current and
# anonymous), CPU per 1,000 requests, page size, cold start, edit to
# published.
APPS = {
    "phoenix": {"base": "http://127.0.0.1:58206", "app": "wrought-bench-phoenix",
                "db": "wrought-bench-phoenix-db", "health": "/up"},
    # Loco's router has no trailing slashes (`/articles/x`, not `/articles/x/`).
    "loco": {"base": "http://127.0.0.1:58207", "app": "wrought-bench-loco",
             "db": "wrought-bench-loco-db", "health": "/_ping", "slash": False},
    "rails": {"base": RAILS, "app": "wrought-bench-rails", "db": "wrought-bench-rails-db",
              "health": "/up"},
}


def app_url(cfg, path):
    """`path` (written with the trailing slash) in the app's own form."""
    if cfg.get("slash", True):
        return cfg["base"] + path
    p, _, q = path.partition("?")
    return cfg["base"] + (p.rstrip("/") or "/") + (f"?{q}" if q else "")


def edit_app(cfg):
    base = cfg["base"]
    out = []
    for i in range(5):
        slug = f"article-{i + 5}"
        title = f"Edited app {i} {time.time_ns()}"
        start = time.monotonic()
        status, body, _ = http(
            "PATCH", f"{base}/api/articles/{slug}", json.dumps({"title": title}).encode(),
            {"Authorization": "Bearer bench-api-token", "Content-Type": "application/json"},
        )
        assert status == 200, body
        out.append(until_contains(app_url(cfg, f"/articles/{slug}/"), title, start))
    return out


def measure_app(name, cfg):
    """One request-rendering application (see APPS), already serving."""
    import resources as rs
    article, listing = app_url(cfg, "/articles/article-17/"), app_url(cfg, "/articles/")
    search = app_url(cfg, "/search/?q=WORD")
    for u in (article, listing, app_url(cfg, "/search/?q=iron")):
        wait_http(u, f"{name} {u}")
    out = {"cold_start_s": cold_start(cfg["app"], article),
           "page_bytes": page_bytes(article), "listing_bytes": page_bytes(listing)}
    out["resources"] = rs.profile(name, [cfg["app"], cfg["db"]], article, search, listing)
    urls = [app_url(cfg, p) for p in ("/", "/articles/", "/articles/article-17/")]
    w = rs.page_weight(urls) or {}
    out["page_weight"] = dict(zip(("home", "listing", "article"), [w.get(u) for u in urls]))
    out["edit_s"] = edit_app(cfg)
    return out


def record_app(res, name, a):
    """File an application's figures under the run's common tables."""
    p = a["resources"]
    res["static"][name] = p["page"]
    res["listing"][name] = p.get("listing")
    res["dynamic"][name] = p.get("search")
    res["resources"][name] = p
    res["page_bytes"][name] = a["page_bytes"]
    res.setdefault("listing_bytes", {})[name] = a["listing_bytes"]
    res["edit_s"][name] = a["edit_s"]
    res["cold_start_s"][name] = a["cold_start_s"]
    res.setdefault("page_weight", {})[name] = a["page_weight"]


def app_versions(name):
    """The framework versions inside the image (written at build time)."""
    r = sh(["docker", "run", "--rm", "--entrypoint", "cat", f"wrought-bench-{name}",
            "/app/VERSIONS"], check=False)
    return " ".join(r.stdout.split()) if r.returncode == 0 else None


def measure_rails():
    """Both configurations a Rails developer would ship: (a) plain
    rendering, (b) with Rails' fragment caching (Solid Cache, the default
    store) and conditional GETs."""
    setup_rails()
    out = {}
    for name, caching in (("rails_plain", False), ("rails_cached", True)):
        start_rails(caching)
        # A first view of each page fills the fragment cache (b).
        for u in ("/articles/article-17/", "/articles/"):
            http("GET", f"{RAILS}{u}")
        out[name] = measure_app(name, APPS["rails"])
    compose("stop", "rails", "rails-db", check=False)
    return out


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
    # Ready means a query over TCP answers three times in a row (the
    # image's init step runs a socket-only server first, then restarts).
    ok = 0
    for _ in range(120):
        r = compose("exec", "-T", "-e", "PGPASSWORD=drupal", "drupal-db", "psql", "-h",
                    "127.0.0.1", "-U", "drupal", "-d", "drupal", "-Atc", "SELECT 1", check=False)
        ok = ok + 1 if r.returncode == 0 else 0
        if ok >= 3:
            break
        time.sleep(0.5)
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
    peaks = []
    for _ in range(3):
        sh(["docker", "exec", "wrought-bench-astro-tool", "rm", "-rf", "/work/astro-1000/dist"])
        with PeakAnon("wrought-bench-astro-tool") as pk:
            t0 = time.monotonic()
            r = sh(["docker", "exec", "-w", "/work/astro-1000", "wrought-bench-astro-tool",
                    "npm", "run", "build"])
            times.append(time.monotonic() - t0)
        peaks.append(round(pk.peak / 2**20, 1))
        out = r.stdout + r.stderr
    tool_cpus("wrought-bench-astro-tool", 1)
    pages = re.search(r"(\d+) page\(s\) built", out)
    log(f"astro ({cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "pages": int(pages.group(1)) if pages else None,
            "peak_mib": max(peaks), "output_mb": du_mb(WORK / "astro-1000/dist")}


def cgroup_dir(container):
    """The container's cgroup v2 folder (Docker's systemd driver)."""
    cid = sh(["docker", "inspect", "-f", "{{.Id}}", container]).stdout.strip()
    for d in (f"/sys/fs/cgroup/system.slice/docker-{cid}.scope",
              f"/sys/fs/cgroup/docker/{cid}"):
        if os.path.isdir(d):
            return Path(d)
    return None


def cg_stat(d, name):
    """One field of memory.stat or cpu.stat (bytes or microseconds)."""
    fname = "cpu.stat" if name.endswith("_usec") else "memory.stat"
    for line in (d / fname).read_text().splitlines():
        k, v = line.split()
        if k == name:
            return int(v)
    return None


class PeakAnon:
    """Peak anonymous memory (heap, stacks: what a process allocates, not
    the page cache of files it writes) of a container while it runs a
    command, sampled every 20 ms from its cgroup."""

    def __init__(self, container):
        import threading
        self.d = cgroup_dir(container)
        self.peak = 0
        self._stop = threading.Event()
        self._t = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self._stop.wait(0.02):
            try:
                self.peak = max(self.peak, cg_stat(self.d, "anon") or 0)
            except OSError:
                pass

    def __enter__(self):
        if self.d:
            self._t.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        if self.d:
            self._t.join()


def ssg_cold_build(name, container, cmd, workdir, out_dir, runs=3, cores=1):
    """Cold build (output removed first) of a static site generator, `runs`
    times on `cores` cores: wall time, peak memory, output size."""
    cpus = tool_cpus(container, cores)
    times, peaks = [], []
    out = ""
    for _ in range(runs):
        sh(["docker", "exec", container, "rm", "-rf", out_dir])
        with PeakAnon(container) as pk:
            t0 = time.monotonic()
            r = sh(["docker", "exec", "-w", workdir, container] + cmd)
            times.append(time.monotonic() - t0)
        peaks.append(round(pk.peak / 2**20, 1))
        out = r.stdout + r.stderr
    tool_cpus(container, 1)
    r = sh(["docker", "exec", container, "sh", "-c",
            f"find {out_dir} -name '*.html' | wc -l; du -sb {out_dir} | cut -f1"])
    pages, size = r.stdout.split()
    log(f"{name} ({cores} core(s) {cpus}) cold build of {N_BUILD} articles: "
        f"{[round(t, 2) for t in times]}s, peak {max(peaks)} MiB")
    return {"seconds": statistics.median(times), "runs": times, "cores": cores,
            "html_pages": int(pages), "output_mb": round(int(size) / 1e6, 1),
            "peak_mib": max(peaks), "peaks_mib": peaks, "last_output": out[-400:]}


HUGO_IMAGE = "ghcr.io/gohugoio/hugo:v0.170.1"
ZOLA_IMAGE = "ghcr.io/getzola/zola:v0.22.1"


def setup_hugo_zola():
    """Hugo and Zola tool containers over bench/work (Zola's image has no
    shell: its binary runs in the engine's Ubuntu image)."""
    hugo_site(WORK / "hugo-1000", N_BUILD)
    zola_site(WORK / "zola-1000", N_BUILD)
    first = os.environ.get("BENCH_TOOL_CPU", "8")
    sh(["docker", "rm", "-f", "wrought-bench-hugo", "wrought-bench-zola"], check=False)
    sh(["docker", "run", "-d", "--name", "wrought-bench-hugo", "--cpuset-cpus", first,
        "--memory", "2g", "-u", f"{os.getuid()}:{os.getgid()}", "-v", f"{WORK}:/work",
        "--entrypoint", "sleep", HUGO_IMAGE, "infinity"])
    zbin = WORK / "zola-bin"
    zbin.mkdir(exist_ok=True)
    cid = sh(["docker", "create", ZOLA_IMAGE]).stdout.strip()
    sh(["docker", "cp", f"{cid}:/bin/zola", str(zbin / "zola")])
    sh(["docker", "rm", cid])
    sh(["docker", "run", "-d", "--name", "wrought-bench-zola", "--cpuset-cpus", first,
        "--memory", "2g", "-u", f"{os.getuid()}:{os.getgid()}", "-v", f"{WORK}:/work",
        "--entrypoint", "sleep", "wrought-bench-engine", "infinity"])
    versions = {
        "hugo": sh(["docker", "exec", "wrought-bench-hugo", "hugo", "version"]).stdout.split()[1],
        "zola": sh(["docker", "exec", "wrought-bench-zola", "/work/zola-bin/zola",
                    "--version"]).stdout.strip(),
    }
    return versions


def hugo_zola_builds(cores=1):
    return {
        "hugo": ssg_cold_build("hugo", "wrought-bench-hugo",
                               ["hugo", "--quiet", "--destination", "/work/hugo-1000/public"],
                               "/work/hugo-1000", "/work/hugo-1000/public", cores=cores),
        "zola": ssg_cold_build("zola", "wrought-bench-zola",
                               ["/work/zola-bin/zola", "build", "--output-dir",
                                "/work/zola-1000/public", "--force"],
                               "/work/zola-1000", "/work/zola-1000/public", cores=cores),
    }


def incremental_ssg():
    """Change one article and rebuild: Hugo and Zola have no incremental
    build outside their watch servers, so `build` renders everything again."""
    out = {}
    for name, container, cmd, wd, art in (
        ("hugo", "wrought-bench-hugo", ["hugo", "--quiet", "--destination",
                                         "/work/hugo-1000/public"], "/work/hugo-1000",
         WORK / "hugo-1000/content/articles/article-5.md"),
        ("zola", "wrought-bench-zola", ["/work/zola-bin/zola", "build", "--output-dir",
                                         "/work/zola-1000/public", "--force"], "/work/zola-1000",
         WORK / "zola-1000/content/articles/article-5.md"),
    ):
        txt = art.read_text()
        art.write_text(txt.replace("title", "title", 1).replace("\n\n", "\n\nEdited.\n\n", 1))
        t0 = time.monotonic()
        sh(["docker", "exec", "-w", wd, container] + cmd)
        out[name] = {"seconds": time.monotonic() - t0, "what": "full rebuild (no incremental build)"}
    return out


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
    markdown_site(WORK / "wrought-site-1000/markdown", N_BUILD)
    one = {
        "wrought_markdown": markdown_cold_build(1),
        "wrought_postgres": wrought_cold_build(pg + "1000", "postgres"),
        "astro": astro_setup(),
    }
    four = {
        "wrought_markdown": markdown_cold_build(4),
        "wrought_postgres": wrought_cold_build(pg + "1000", "postgres", cores=4),
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


def restart_db(args, fresh=True):
    """Recreate the database container with `args` (data volume kept);
    `fresh` also empties the `wrought` database."""
    compose("up", "-d", "--force-recreate", "--no-deps", "wrought-db",
            env={"BENCH_PG_ARGS": args})
    for _ in range(60):
        if compose("exec", "-T", "wrought-db", "pg_isready", "-U", "wrought",
                   check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)
    if fresh:
        compose("exec", "-T", "wrought-db", "psql", "-U", "wrought", "-d", "postgres",
                "-c", "DROP DATABASE IF EXISTS wrought WITH (FORCE)",
                "-c", "CREATE DATABASE wrought")


def pg_compare(label):
    """`bench/run.sh pg <label>`: the database-bound numbers for one engine
    binary (BENCH_ENGINE, else a fresh release build): cold builds of 1000
    articles from Markdown and PostgreSQL (stock, tuned, tuned over
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


# Targets and groups (`bench/run.sh --targets a,b` or `--group core`).
SERVING = ["wrought", "wordpress", "drupal", "astro", "phoenix", "loco", "rails"]
BUILDS = ["wrought", "astro", "hugo", "zola"]
GROUPS = {
    "core": {"serve": ["wrought", "wordpress", "drupal", "astro", "phoenix", "loco"],
             "build": BUILDS},
    "extended": {"serve": ["rails"], "build": []},
}
GROUPS["all"] = {"serve": SERVING, "build": BUILDS}
# The expensive measures run only for these.
EXPENSIVE = ("wrought", "wordpress", "drupal")


def results_name(argv):
    """results.json for the core group (the default), results-<group>.json
    for another group, results-targets.json for a --targets selection."""
    if "--targets" in argv:
        return "results-targets.json"
    if "--group" in argv:
        g = argv[argv.index("--group") + 1]
        return "results.json" if g == "core" else f"results-{g.replace(',', '-')}.json"
    return "results.json"


def selection(argv):
    serve, build = [], []
    groups = ["core"]
    targets = None
    for i, a in enumerate(argv):
        if a == "--group":
            groups = argv[i + 1].split(",")
        if a == "--targets":
            targets = argv[i + 1].split(",")
    for g in groups:
        serve += [t for t in GROUPS[g]["serve"] if t not in serve]
        build += [t for t in GROUPS[g]["build"] if t not in build]
    if targets:
        serve = [t for t in SERVING if t in targets]
        build = [t for t in BUILDS if t in targets]
    return serve, build


class Clock:
    """Wall time per target, for results.json."""

    def __init__(self):
        self.spent = {}

    def __call__(self, name):
        clock = self

        class _T:
            def __enter__(self):
                self.t0 = time.monotonic()

            def __exit__(self, *exc):
                clock.spent[name] = round(clock.spent.get(name, 0) + time.monotonic() - self.t0, 1)
        return _T()


def request_mode_site(on):
    """Switch the 200-article site's render dial to `request` (every page
    rendered per request, as Rails and the PHP systems do) or back."""
    cfg = WORK / "wrought-site/site.toml"
    text = cfg.read_text()
    text = text.replace('default = "request"', 'default = "publish"')
    if on:
        text = text.replace('default = "publish"', 'default = "request"')
    cfg.write_text(text)


def wp_page_cache(on):
    """WordPress with the page cache a host would add in front of it:
    Apache's mod_cache with a disk store for anonymous pages (10 minutes),
    the server-level page cache of a plain Apache host. Stock: off."""
    conf = ("CacheQuickHandler on\nCacheEnable disk /\nCacheRoot /var/cache/apache2/mod_cache_disk\n"
            "CacheIgnoreNoLastMod On\nCacheDefaultExpire 600\nCacheIgnoreHeaders Set-Cookie\n")
    if on:
        sh(["docker", "exec", "wrought-bench-wp", "sh", "-c",
            f"printf '{conf}' > /etc/apache2/conf-available/bench-cache.conf && "
            "a2enmod -q cache cache_disk >/dev/null && a2enconf -q bench-cache >/dev/null && "
            "mkdir -p /var/cache/apache2/mod_cache_disk && "
            "chown -R www-data /var/cache/apache2 && apache2ctl -k graceful"])
    else:
        sh(["docker", "exec", "wrought-bench-wp", "sh", "-c",
            "a2disconf -q bench-cache >/dev/null 2>&1; a2dismod -q -f cache_disk cache "
            ">/dev/null 2>&1; apache2ctl -k graceful"], check=False)
    time.sleep(2)


def density(res):
    """Sites per 64 GB, as arithmetic from the measured per-site figures."""
    out = {}
    ram = 64 * 1024
    shared = res.get("shared_postgres", {}).get("small")
    if shared:
        per = shared["per_site_mib"]
        out["wrought_shared_pg"] = {
            "formula": f"64 GiB x 0.8 / {per} MiB per site (20 sites: one PostgreSQL "
                       f"{shared['postgres']['current_mib']} MiB plus {shared['engine_each_mib']}"
                       " MiB per engine, divided by 20)",
            "sites": int(ram * 0.8 / per),
            "note": "estimate; idle sites of 200 articles; PostgreSQL grows with data and load"}
    for name in ("wordpress", "drupal", "wrought"):
        prof = res.get("resources", {}).get(name)
        if prof:
            per = prof["idle"]["current_mib"]
            out[name + "_own_stack"] = {
                "formula": f"64 GiB x 0.8 / {per} MiB idle stack", "sites": int(ram * 0.8 / per),
                "note": "estimate; one application and one database per site, idle"}
    return out


def main():
    serve, build = selection(sys.argv[1:])
    out_path = RESULTS / results_name(sys.argv[1:])
    clock = Clock()
    RESULTS.mkdir(exist_ok=True)
    clean_work()
    WORK.mkdir(exist_ok=True)
    res = {"date": time.strftime("%Y-%m-%d"), "hardware": hardware(),
           "targets": {"serve": serve, "build": build},
           "settings": {"threads": THREADS, "connections": CONNS, "warmup": WARM,
                        "measured": MEASURE, "articles": N_RUNTIME, "build_articles": N_BUILD,
                        "placement": "one dedicated core per role: application, database, "
                                     "tools, load generator (isolcpus host); 2 GB per application, "
                                     "1 GB per database"}}
    with clock("setup"):
        make_data()
        res["binary_mb"] = build_engines()
        wrought_site(WORK / "wrought-site", N_RUNTIME)
        wrought_site(WORK / "wrought-site-1000", N_BUILD)
        (WORK / "sites").mkdir(exist_ok=True)
        log("building images")
        compose("build", "wrought", "drupal", "loadgen", capture=False)
        compose("--profile", "tools", "pull", "--quiet", "wrought-db", "caddy", "wp", "wp-db",
                "wp-cli", "drupal-db", "astro", capture=False)
        start_wrought_tools()

    pg = "postgres://wrought:wrought@wrought-db:5432/wrought"
    with clock("wrought"):
        tok_pg = setup_wrought(pg, "postgres")
    res["versions"] = {
        "wrought": sh([str(HERE / "wrought/engine"), "--version"]).stdout.strip().splitlines()[0],
        "astro": "7.4.10", "wordpress": "7.1.3 (php8.4-apache)", "mariadb": "11.4.13",
        "drupal": "11.3.23 (php8.4-apache-bookworm)", "postgres": "17.12",
        "node": "22.23.5", "caddy": "2.11.8", "wrk": "alpine 3.22.8 package",
        "rails": "8.1.4.1 (Ruby 4.0.10, Puma, production)",
    }

    # Builds of 1000 articles.
    one, four = {}, {}
    if "wrought" in build:
        with clock("build:wrought"):
            markdown_site(WORK / "wrought-site-1000/markdown", N_BUILD)
            one["wrought_markdown"] = markdown_cold_build(1)
            one["wrought_markdown_nocopies"] = markdown_cold_build(1, copies=False)
            one["wrought_postgres"] = wrought_cold_build(pg + "1000", "postgres")
            four["wrought_markdown"] = markdown_cold_build(4)
            four["wrought_markdown_nocopies"] = markdown_cold_build(4, copies=False)
            four["wrought_postgres"] = wrought_cold_build(pg + "1000", "postgres", cores=4)
    if "astro" in build or "astro" in serve:
        with clock("build:astro"):
            a1 = setup_astro()
            if "astro" in build:
                one["astro"] = a1
                four["astro"] = astro_cold_build(4)
    if "hugo" in build or "zola" in build:
        with clock("build:hugo+zola"):
            res["versions"].update(setup_hugo_zola())
            hz = hugo_zola_builds(1)
            for k in ("hugo", "zola"):
                if k in build:
                    one[k] = hz[k]
            res["incremental_build"] = incremental_ssg()
    res["cold_build"], res["cold_build_4_cores"] = one, four
    if "wrought" in build:
        res["cold_build_peak_mib"] = {
            "wrought_markdown": one["wrought_markdown"].get("peak_mib"),
            "wrought_postgres": one["wrought_postgres"].get("peak_mib")}

    # Serving.
    app_pw = None
    with clock("wordpress"):
        if "wordpress" in serve:
            app_pw = setup_wordpress()
    with clock("drupal"):
        if "drupal" in serve:
            setup_drupal()
    compose("up", "-d", "caddy")
    wait_http("http://127.0.0.1:58203/articles/article-17/", "caddy (wrought)")
    static_targets = {
        "wrought_engine": "http://127.0.0.1:58200/articles/article-17/",
        "wrought_caddy": "http://127.0.0.1:58203/articles/article-17/",
        "astro_caddy": "http://127.0.0.1:58204/articles/article-17/",
        "wordpress": "http://127.0.0.1:58201/article-17/",
        "drupal": "http://127.0.0.1:58202/article-17",
    }
    owner = {"wrought_engine": "wrought", "wrought_caddy": "wrought", "astro_caddy": "astro",
             "wordpress": "wordpress", "drupal": "drupal"}
    static_targets = {k: u for k, u in static_targets.items() if owner[k] in serve}
    res["page_bytes"] = {k: page_bytes(u) for k, u in static_targets.items()}
    if "drupal" in serve:
        http("GET", static_targets["drupal"])
        res["drupal_cache_header"] = http("GET", static_targets["drupal"])[2].get("X-Drupal-Cache")
    res["static"] = {}
    for k, u in static_targets.items():
        with clock(owner[k]):
            log(f"static: {k}")
            res["static"][k] = wrk(u)
    dyn = {"wrought_postgres": "http://127.0.0.1:58200/search/?q=WORD",
           "wordpress": "http://127.0.0.1:58201/?s=WORD",
           "drupal": "http://127.0.0.1:58202/search/node?keys=WORD"}
    dyn_owner = {"wrought_postgres": "wrought", "wordpress": "wordpress", "drupal": "drupal"}
    res["dynamic"] = {}
    for k, u in dyn.items():
        if dyn_owner[k] in serve:
            with clock(dyn_owner[k]):
                log(f"dynamic: {k}")
                res["dynamic"][k] = wrk(u, vary=True)
    res["listing"] = {}
    if "wrought" in serve:
        with clock("wrought"):
            log("listing: wrought_engine")
            res["listing"]["wrought_engine"] = wrk("http://127.0.0.1:58200/articles/")
            res["listing_bytes"] = {"wrought_engine": page_bytes("http://127.0.0.1:58200/articles/")}
    res["memory_mib"] = mem_mib()

    log("edit to published")
    res["edit_s"] = {}
    with clock("wrought"):
        if "wrought" in serve:
            res["edit_s"]["wrought_postgres"] = edit_wrought(tok_pg, "pg")
    if "wordpress" in serve:
        with clock("wordpress"):
            res["edit_s"]["wordpress"] = edit_wordpress(app_pw)
    if "drupal" in serve:
        with clock("drupal"):
            res["edit_s"]["drupal"] = edit_drupal()
    if "astro" in serve:
        with clock("astro"):
            res["edit_s"]["astro"] = edit_astro()

    # Resources: whole-stack memory and CPU per target.
    import resources as rs
    stacks = {
        "wrought": (["wrought-bench-engine", "wrought-bench-wrought-db"],
                    static_targets.get("wrought_engine"), dyn["wrought_postgres"]),
        "wordpress": (["wrought-bench-wp", "wrought-bench-wp-db"],
                      static_targets.get("wordpress"), dyn["wordpress"]),
        "drupal": (["wrought-bench-drupal", "wrought-bench-drupal-db"],
                   static_targets.get("drupal"), dyn["drupal"]),
        "astro": (["wrought-bench-caddy"], static_targets.get("astro_caddy"), None),
    }
    res["resources"] = {}
    for name, (containers, page, search) in stacks.items():
        if name in serve:
            with clock(name):
                res["resources"][name] = rs.profile(
                    name, containers, page, search,
                    "http://127.0.0.1:58200/articles/" if name == "wrought" else None)
    if "wordpress" in serve:
        with clock("wordpress"):
            wp_page_cache(True)
            res["resources"]["wordpress_cached"] = rs.profile(
                "wordpress_cached", stacks["wordpress"][0], static_targets["wordpress"],
                dyn["wordpress"])
            res["static"]["wordpress_cached"] = res["resources"]["wordpress_cached"]["page"]
            wp_page_cache(False)
    if "wrought" in serve:
        with clock("wrought"):
            # The request-rendered figure, like for like with the systems
            # that render per request.
            request_mode_site(True)
            compose("up", "-d", "--force-recreate", "--no-deps", "wrought",
                    env={"WROUGHT_DB_URL": pg})
            wait_http("http://127.0.0.1:58200/", "wrought (request mode)")
            res["resources"]["wrought_request"] = rq = rs.profile(
                "wrought_request", stacks["wrought"][0], static_targets["wrought_engine"],
                dyn["wrought_postgres"], "http://127.0.0.1:58200/articles/")
            res["static"]["wrought_request"] = rq["page"]
            res["listing"]["wrought_request"] = rq["listing"]
            res["dynamic"]["wrought_request"] = rq["search"]
            request_mode_site(False)
            compose("up", "-d", "--force-recreate", "--no-deps", "wrought",
                    env={"WROUGHT_DB_URL": pg})
            wait_http("http://127.0.0.1:58200/", "wrought")

    # Application frameworks rendering per request (core: Phoenix, Loco).
    res["apps"] = {}
    res.setdefault("cold_start_s", {})
    for name, setup in (("phoenix", setup_phoenix), ("loco", setup_loco)):
        if name not in serve:
            continue
        with clock(name):
            try:
                setup()
                res["apps"][name] = a = measure_app(name, APPS[name])
                record_app(res, name, a)
                res["versions"][name] = app_versions(name)
            except (Exception, SystemExit) as e:  # noqa: BLE001
                res["apps"][name] = {"error": repr(e)[-500:]}
                log(f"{name} failed: {e!r}")
            compose("stop", name, f"{name}-db", check=False)

    # Page weight in a browser: home, listing, article.
    pages = {
        "wrought": ["http://127.0.0.1:58203/", "http://127.0.0.1:58203/articles/",
                    "http://127.0.0.1:58203/articles/article-17/"],
        "astro": ["http://127.0.0.1:58204/", "http://127.0.0.1:58204/articles/",
                  "http://127.0.0.1:58204/articles/article-17/"],
        "wordpress": ["http://127.0.0.1:58201/", "http://127.0.0.1:58201/page/2/",
                      "http://127.0.0.1:58201/article-17/"],
        "drupal": ["http://127.0.0.1:58202/", "http://127.0.0.1:58202/node",
                   "http://127.0.0.1:58202/article-17"],
    }
    with clock("page weight"):
        urls = [u for t, us in pages.items() if t in serve for u in us]
        w = rs.page_weight(urls) or {}
        res.setdefault("page_weight", {}).update(
            {t: dict(zip(("home", "listing", "article"), [w.get(u) for u in us]))
             for t, us in pages.items() if t in serve})

    out_path.write_text(json.dumps(res, indent=1))  # so far
    # The expensive ones: smallest memory limit, shared PostgreSQL. A
    # failure is recorded and the run goes on.
    res["min_memory_limit"] = {}
    for name in EXPENSIVE:
        if name not in serve:
            continue
        with clock(name):
            app, db = stacks[name][0]
            try:
                res["min_memory_limit"][name] = {
                    "app": rs.min_limit(name, app, stacks[name][1]),
                    "db": rs.min_limit(name, db, stacks[name][1]),
                }
            except Exception as e:  # noqa: BLE001
                res["min_memory_limit"][name] = {"error": repr(e)[-300:]}
                log(f"min limit {name} failed: {e!r}")
    out_path.write_text(json.dumps(res, indent=1))
    if "wrought" in serve:
        with clock("shared postgres"):
            try:
                res["shared_postgres"] = rs.shared_postgres(20)
            except (Exception, SystemExit) as e:  # noqa: BLE001
                res["shared_postgres_error"] = repr(e)[-500:]
                log(f"shared postgres failed: {e!r}")
    res["density_64gb"] = density(res)

    for name, c, u in (("wrought", "wrought-bench-engine", static_targets.get("wrought_engine")),
                       ("wordpress", "wrought-bench-wp", static_targets.get("wordpress")),
                       ("drupal", "wrought-bench-drupal", static_targets.get("drupal"))):
        if name in serve:
            with clock(name):
                res["cold_start_s"][name] = cold_start(c, u)
    res["memory_mib_idle"] = mem_mib()

    if "rails" in serve:
        with clock("rails"):
            rails = measure_rails()
        for name, r in rails.items():
            res["apps"][name] = r
            record_app(res, name, r)

    res["images"] = {
        "wrought": image_info("wrought-bench-engine"),
        "postgres": image_info("postgres:17.12"),
        "wordpress": image_info("wordpress:7.1.3-php8.4-apache"),
        "mariadb": image_info("mariadb:11.4.13"),
        "drupal": image_info("wrought-bench-drupal"),
        "node": image_info("node:22.23.5-alpine3.24"),
        "caddy": image_info("caddy:2.11.8-alpine"),
        "hugo": image_info(HUGO_IMAGE),
        "zola": image_info(ZOLA_IMAGE),
        "phoenix": image_info("wrought-bench-phoenix"),
        "loco": image_info("wrought-bench-loco"),
        "rails": image_info("wrought-bench-rails"),
    }
    res["disk_mb"] = {
        "wrought_public": du_mb(WORK / "wrought-site/public"),
        "astro_dist": du_mb(WORK / "astro/dist"),
        "astro_node_modules": du_mb(WORK / "astro/node_modules"),
    }
    res["wall_s"] = clock.spent
    out_path.write_text(json.dumps(res, indent=1))
    log(f"wrote {out_path}; time per target: {clock.spent}")


def teardown():
    if os.environ.get("BENCH_KEEP"):
        log("BENCH_KEEP set: leaving the containers up")
        return
    sh(["docker", "rm", "-f", "wrought-bench-astro-tool", "wrought-bench-hugo",
        "wrought-bench-zola"], check=False)
    sh(["sh", "-c", "docker ps -aq --filter name=wrought-bench-site | xargs -r docker rm -f"],
       check=False)
    rmi = os.environ.get("BENCH_RMI", "local")
    compose("--profile", "tools", "down", "-v", "--remove-orphans", "--rmi", rmi, check=False)
    clean_work()
    log("removed the containers, volumes and bench/work")


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["build"]:
            build_only()
        elif sys.argv[1:2] == ["stall"]:
            import stall
            stall.main(sys.argv[2:])
        elif sys.argv[1:2] == ["pg"]:
            pg_compare(sys.argv[2] if len(sys.argv) > 2 else "now")
        elif sys.argv[1:2] in ([], ["--group"], ["--targets"]):
            main()
        else:
            raise SystemExit(f"unknown arguments: {sys.argv[1:]}")
    finally:
        teardown()
