+++
title = "Deployment"
description = "Serve the output folder with Caddy, nginx or the built-in server."
weight = 11
+++

A built site is a folder of files. Anything that serves files can serve it.

## Caddy in front

The intended setup puts Caddy (or nginx) in front: it serves the output folder
directly and passes misses and dynamic routes to the engine. A minimal
Caddyfile for a build-mode site:

```text
example.org {
	root * /srv/site
	encode zstd gzip
	@hidden path /.*
	respond @hidden 404
	@hashed path_regexp [.-][0-9a-f]{8,}[.][0-9A-Za-z]+$
	header @hashed Cache-Control "public, max-age=31536000, immutable"
	@other not path_regexp [.-][0-9a-f]{8,}[.][0-9A-Za-z]+$
	header @other Cache-Control "no-cache"
	file_server
	handle_errors {
		@404 expression {err.status_code} == 404
		rewrite @404 /404.html
		file_server
	}
}
```

The engine's assets carry a hash of their content in their names, so they
can be cached forever; everything else is revalidated (see
[browser caching](#browser-caching); this short version marks a few dated or
numbered names as hashed too, the shipped Caddyfile does not). The
repository's `deploy/` folder has the full Caddyfile, with pre-compressed
copies, and a Docker Compose file that serves `docs/public` this way:

```sh
engine build --site docs
docker compose -f deploy/compose.yml up -d
curl -I http://127.0.0.1:8790/docs/
```

## Caddy in front of a publishing site

For a site with `publish` routes, Caddy serves the files that exist and passes
everything else to `engine serve`, which renders a missing publish page,
writes it, and answers (or returns the 404 page):

```text
http://127.0.0.1:8791 {
	root * /srv/site/public
	@hidden path /.* */.*
	handle @hidden {
		respond 404
	}
	@published file {
		try_files {path} {path}/index.html
	}
	handle @published {
		file_server
	}
	handle {
		reverse_proxy 127.0.0.1:4400
	}
}
```

Put the hidden-file rule in its own `handle`: `handle` blocks run before
`respond` in Caddy's directive order. Mount the site folder, not the output
folder: `engine build` replaces the output folder, and a mount of the old one
would go stale. The repository's `deploy/Caddyfile.publish` (the routing
itself is the `wrought_site` snippet in `deploy/wrought-site.caddy`, shared
with the HTTPS front door below) and `deploy/compose.publish.yml` do this
for `examples/cms-site`:

```sh
engine build --site examples/cms-site
engine serve --site examples/cms-site &
docker compose -f deploy/compose.publish.yml up -d
curl -I http://127.0.0.1:8791/articles/
```

`engine serve` needs the database (`ENGINE_DATABASE_URL`); see
[content types](/docs/content-types/#the-database).

The companion Bloomery project deploys the full stack (Caddy, the engine and
PostgreSQL) to cloud, bare-metal and local Docker hosts.

## HTTPS, HTTP/3 and security headers

`deploy/Caddyfile.tls` is the front door for a real domain. It imports the
same routing (`wrought-site.caddy`) and two snippets from
`deploy/front-door.caddy` that you can also use in a Caddyfile of your own
(`import front-door.caddy`, then `import tls_modern` and
`import security_headers` in the site):

```sh
# deploy/Caddyfile.tls, front-door.caddy and wrought-site.caddy side by side
# in /etc/caddy; the site folder mounted at /srv/site.
WROUGHT_DOMAIN=example.org ENGINE_UPSTREAM=127.0.0.1:4400 caddy run --config /etc/caddy/Caddyfile.tls
```

Caddy gets and renews the certificate itself (ACME); add `email
you@example.org` to the global options to hear about problems. Open TCP 80
and 443 and UDP 443 (HTTP/3).

| Setting | Value | Why |
| --- | --- | --- |
| Protocols | TLS 1.3 preferred, TLS 1.2 the minimum | 1.0 and 1.1 are broken and no supported browser needs them; 1.2 is kept for older clients (some Android and embedded browsers, monitoring tools) |
| TLS 1.2 suites | ECDHE with AES-GCM or ChaCha20-Poly1305 only | forward secrecy and authenticated encryption; no CBC, no RSA key exchange, no SHA-1 |
| TLS 1.3 suites | all three (AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305) | all are AEAD; the server picks AES-GCM where the CPU accelerates AES and ChaCha20 where it does not (a Raspberry Pi 4) |
| Certificate | ECDSA P-256 | smaller and faster handshakes than RSA |
| HTTP | HTTP/3, HTTP/2, HTTP/1.1; `http://` redirects to `https://` | HTTP/3 is announced with `Alt-Svc` and used by browsers that can |
| Session resumption, OCSP stapling | Caddy's defaults (on) | |
| `Strict-Transport-Security` | `max-age=31536000` | see below |
| `X-Content-Type-Options` | `nosniff` | |
| `Referrer-Policy` | `strict-origin-when-cross-origin`, unless the engine set one | the sign-in pages and the admin send `same-origin` |
| `X-Frame-Options`, `Content-Security-Policy` | `SAMEORIGIN` and `frame-ancestors 'self'`, unless the engine set them | no other site can frame yours; the engine's stricter policies on sign-in, admin and the visual editor stay as they are |
| `Permissions-Policy` | camera, microphone, geolocation and topics off, unless set | |

No site-wide script or style policy is set: a site's templates may use
inline code, and only the site knows where its scripts come from. Add a
`Content-Security-Policy` of your own in the site block when you know
them; the engine keeps its own on its pages.

**HSTS, `includeSubDomains` and `preload`.** The header tells browsers to
use HTTPS only, for a year. `includeSubDomains` is off because other
names under your domain (an old `mail.` or `intranet.` host) may not have
HTTPS yet, and the header would lock browsers out of them. `preload` is off
because it is a request to have the domain built into browsers as
HTTPS-only: removal takes months, it requires `includeSubDomains`, and it
needs a submission at hstspreload.org. When every subdomain serves HTTPS,
change the line in `front-door.caddy` to
`max-age=63072000; includeSubDomains; preload` and submit the domain.

`scripts/check-front-door.sh` checks all of this against the shipped files,
with Caddy's internal certificate authority (no public certificate is
requested) in a throwaway container: TLS 1.3 by default, the accepted and
refused TLS 1.2 suites, TLS 1.0 and 1.1 refused, HTTP/2, HTTP/3, the
redirect and the headers.

## Accounts behind Caddy

With accounts, some requests must reach the engine even when a file exists:
the admin and sign-in pages, every `POST`, and (the render dial's rule)
requests from signed-in visitors or with a query string. Put this `handle`
before the one that serves files (`deploy/Caddyfile.publish` has it):

```text
@engine expression `{http.request.uri.query} != "" || ({http.request.method} != "GET" && {http.request.method} != "HEAD") || {http.request.cookie.__Host-wrought_session} != "" || path('/admin', '/admin/*', '/account', '/account/*', '/_engine/ui/*')`
handle @engine {
	reverse_proxy 127.0.0.1:4400
}
```

Session cookies are `Secure`: serve the site over HTTPS (Caddy does it
automatically for a real domain). Set `[auth] behind_proxy = true` so login
and form rate limits see the client's address, and `ENGINE_PUBLIC_URL` (or
`[site] base_url`) so links in emails point at the public address. Without
an SMTP server, password-reset links are written to the engine's log; treat
the log as secret.

## An app behind Caddy

An `app` site is the same picture: Caddy serves the files `engine build` and
publishing wrote, and passes the rest to `engine serve`: `request` pages
(they have no file), form posts (`/_engine/forms/...`), the admin, and
anyone signed in. `deploy/compose.app.yml` runs it for `examples/small-app`
on port 8792 with the same Caddyfile:

```sh
engine build --site examples/small-app
engine serve --site examples/small-app &
docker compose -f deploy/compose.app.yml up -d
curl -sI http://127.0.0.1:8792/ | grep -i via        # none: a file from Caddy
curl -sI http://127.0.0.1:8792/support/ | grep -i x-wrought-fragments
```

What an app needs besides the database:

| Variable | For |
| --- | --- |
| `ENGINE_DATABASE_URL` | the database |
| `ENGINE_SMTP_URL` | sending mail (`smtps://user:password@host:465`); see [jobs and mail](/docs/jobs-and-mail/) |
| `ENGINE_SECRET_KEY` | encrypting two-factor secrets (32+ random characters) |
| `ENGINE_PUBLIC_URL` | links in emails |

Job workers run inside `engine serve`. To scale them separately, run
`engine serve --no-workers` and one or more `engine jobs work` processes
against the same database; they share the queue safely.

## Media, search and the API

Uploads live in `[media] dir`, outside the output folder (so `engine build`
never touches them; back the folder up with the database). Caddy can serve
public files straight from disk, with the same safety headers as the engine
(they keep a file from ever running script on the site):

```text
handle_path /media/* {
	root * /srv/site/data/media/public
	header {
		X-Content-Type-Options nosniff
		Content-Security-Policy "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox"
		Cache-Control "public, max-age=31536000, immutable"
	}
	@pdf path *.pdf
	header @pdf Content-Disposition attachment
	file_server
}
```

Never serve `data/media/private/`: form attachments are only for the admin.
The search page (`/search/?q=...`) has a query string and the API is under
`/api/`, so the `@engine` matcher above already sends them to the engine;
add `path('/api/*')` to it if your API clients send no query string.

## Browser caching

A page refresh is always enough to see an edit, and nothing needs purging,
while browsers and CDNs keep what they safely can. `engine serve` and the
shipped Caddyfiles (`deploy/Caddyfile`, `deploy/Caddyfile.publish`) apply
the same rules:

| Response | `Cache-Control` | Revalidation |
| --- | --- | --- |
| Content-hashed files: the engine's CSS and scripts, component files, the token stylesheet, `static/` files linked through [`asset()`](/docs/templates/#linking-your-own-css-scripts-fonts-and-images-asset) | `public, max-age=31536000, immutable` | never: an edit gives a new URL |
| Media (`/media/<id>/...`) | `public, max-age=31536000, immutable` | never: a new upload gets a new id |
| Published pages, feeds, sitemaps, files linked by a plain path | `no-cache` | every use; `304` from a strong `ETag` (one per pre-compressed encoding, with `Vary: Accept-Encoding`) |
| Pages rendered per request, the same for every visitor (signed out, no form) | `no-cache` with `Vary: Cookie` | every use; `304` without rendering while nothing the page read has changed |
| Signed-in visitors, pages with a form token, admin, account, anything that sets a cookie or names no policy | `private, no-store` | never stored by a shared cache |

**Why `no-cache` and not a short `max-age`.** `no-cache` lets every cache
keep the page but makes it ask before each use; the answer is a `304` of a
few hundred bytes, from an ETag the server already knows (the file's
content hash, kept per file version; for a request page, the ETag of its last
render, kept while none of the cache tags it read has changed). A short
`max-age` (say 60 s with `must-revalidate`) would save that round trip, but
for up to a minute a link click, or the stylesheet of a refreshed page,
would show the old version: the "a refresh is enough" rule would no longer
hold.

**Content-hashed names.** A name is treated as hashed when the segment just
before the extension is 8 or more lowercase hex characters with at least
one digit and one letter (`site.3f2a9c1d0b.css`, `video_embed-8c1e0f2a7d.js`);
`photo-20240101.jpg` and `deadbeef.png` are not. Files from your own
bundler that follow this pattern get the same treatment. Old hashed files
stay on disk after `engine serve` republishes, so a page still open in a
browser keeps its stylesheet; the next `engine build` removes them.

**Request pages and 304s.** For a page rendered per request, the engine
keeps the ETag of its last render together with the cache tags that render
recorded. When a browser revalidates and none of those tags has changed
(the same change feed that evicts the fragment cache; search results pages
are dropped whenever the search index takes a change), the `304` is sent
without rendering and without the database. These ETags live as long as
fragments (`[cache] fragment_ttl_seconds`) and are bounded like the file
ETag cache; a template change starts afresh.

**CDNs.** Hashed files can be cached at the edge for a year. Pages may be
stored by a CDN too, but revalidated on each request. Purging by cache tag
(`Surrogate-Key`) is not part of this release.

## A bucket and a CDN instead of Caddy

With `[publish] target = "s3"` (the `s3` feature) pages and public media
live in an S3-compatible bucket and a CDN serves them; the CDN sends only
the dynamic routes (`/admin/*`, `/account/*`, `/api/*`, `/_engine/forms/*`,
`request` routes) to `engine serve`, with caching off. Caddy is then only
in front of that second origin, or not at all; a static site needs no
server. See [publishing to a bucket](/docs/buckets/#dynamic-routes-behind-the-cdn).

## A small site's database

A `cms` site made with `engine new` has its own `compose.yml` (PostgreSQL
in Docker, on `127.0.0.1`). On a host with several sites, run one
PostgreSQL server for all of them, a database per site, and give each site
its `ENGINE_DATABASE_URL`; back up with `pg_dump` plus the media folder. See
[databases](/docs/databases/).

## Start on demand

An idle site can cost nothing but its socket. `deploy/systemd/` has the
units:

- `wrought-site@.socket` listens for the site (one `ListenStream`, set per
  site in a drop-in) and starts `wrought-site@.service` on the first
  connection. The engine takes the socket systemd passes (`LISTEN_FDS`);
  started by hand it binds its own port, so the same binary and settings
  work both ways. Published pages never need the engine: Caddy serves them
  from the output folder.
- The engine listens before it connects to the database. From its first
  millisecond it answers published files; a page rendered per request, a
  form post or the admin waits for the start (up to 10 s), or, if the
  database is unreachable, gets the outage answer at once (the published
  copy of the page, or `503` with `Retry-After`; a post to a form from the
  site's files goes to the [form spool](/docs/forms/)). It keeps trying the
  database and serves everything once it answers and its migrations are
  checked, without a restart.
- **Idle exit** is the hosting admin's setting, never the site's:
  `ENGINE_IDLE_EXIT_SECONDS` in the service's environment, or
  `/etc/wrought/host.toml` (`ENGINE_HOST_CONFIG`; see
  `deploy/systemd/host.toml.example`), for all sites and per site by
  `ENGINE_SITE_ID`. Nothing in `site.toml` reads it. Off by default. With it
  on, the engine exits with status 0 after that many seconds without a
  request, once requests in flight and running jobs are done, and not while
  the form spool holds posts the database could take now. Posts waiting for
  a database that is down do not keep it awake; the next start writes them.
- `SIGTERM` (systemd, Docker) stops taking connections, lets requests and
  jobs finish and exits 0.
- **Scheduled jobs** of a stopped engine: `wrought-wake@.timer` starts the
  engine every 5 minutes (change `OnCalendar` to suit); its workers enqueue
  every schedule that came due while it slept, once, run them, and the
  engine exits again after the quiet period. A site without schedules does
  not need the timer.

`scripts/check-coldstart.sh` measures the start (process start to the first
byte) and holds it to a budget.

## Sleeping database

For sites that are not web apps, the hosting admin can have each site's own
PostgreSQL server stopped when the site has not used it for a while, and
started again when it is needed: an idle site then holds no database memory
at all. This is the hosting admin's choice, per site, set apart from the
engine's own idle exit; nothing in a site's `site.toml` can turn it on or
off. It is off by default, and a host-wide default never applies to an
engine of the `app` preset (a web app keeps its database running unless
its own `[sites.<id>]` entry says otherwise). Only a server the site has to
itself can sleep: the engine never stops one with other sessions on it
(another site, a backup, a migration, `psql`).

**Switching it on** for the site `blog` in `/etc/wrought/host.toml` (see
`deploy/systemd/host.toml.example`):

```toml
[sites.blog]
db_sleep_seconds = 600        # unused this long: stopped (0: never)
db_wake_command  = ["/usr/local/lib/wrought/wrought-db-hook", "wake",  "systemd:wrought-pg@blog.service"]
db_sleep_command = ["/usr/local/lib/wrought/wrought-db-hook", "sleep", "systemd:wrought-pg@blog.service"]
# db_wake_timeout_seconds = 60  # a wake not answered by then has failed
```

The commands can also go in `[defaults]`, with `{site}` standing for the
site's `ENGINE_SITE_ID`. `deploy/wrought-db-hook` is the shipped hook; it
knows two kinds of server:

- **systemd** (`systemd:<unit>`): one `deploy/systemd/wrought-pg@.service`
  instance per site (its own data directory and port). It stops PostgreSQL
  with `SIGINT`, the fast clean shutdown, and never kills it. The engine's
  user starts and stops it through polkit:
  `deploy/systemd/wrought-db-sleep.rules` allows exactly that, for
  `wrought-pg@*.service` only. Do not enable the unit at boot; the engine
  starts it when it needs it.
- **Docker** (`docker:<container>`, the shipped compose setup): `docker
  start`, and `SIGINT` then a wait for the exit (never `docker stop`, whose
  timeout ends in `SIGKILL`). Give the container `restart: "no"`. The
  engine's user needs access to Docker, which is root-equivalent: prefer
  the systemd kind on a shared host.

Any other pair of commands works too: the wake command starts the server
(the engine then waits until it answers), the sleep command stops it
cleanly; both exit 0 when done.

**When it sleeps:** after `db_sleep_seconds` without a query from a visitor's
request, with no job running, no form post waiting in the spool, no publish
under way, none of the engine's connections in use and no other session on
the server. Background work does not count as use and never wakes it: the
job workers' polling, the publisher's safety poll and the search index
check simply wait. Before stopping it, the engine records in
`<site>/.engine/db-sleep.json` when the next scheduled job is due; it wakes
the database then, and an engine started later (`wrought-wake@.timer`)
reads that time instead of asking the database, so a timer start with
nothing due leaves it asleep. When the engine itself exits for its idle
exit, the database is put to sleep with it.

**What visitors see during a wake:** published pages are served as always,
from the files, whether the database sleeps or not. A page rendered per
request, a signed-in visitor or the admin starts the wake and waits for it
(measured with the Docker hook: medians of 1.1 to 1.5 s, most of it
`docker start` itself; at most 10 s, then the usual outage answer, `503`
with `Retry-After`). A form post is answered
at once from the [form spool](/docs/forms/) and written when the wake it
starts is done. A wake that fails (the command fails, or the server does
not answer within `db_wake_timeout_seconds`) gives the outage answer, is
logged, shown by `engine doctor` ("LAST WAKE FAILED") and in the loopback
health detail (`database.sleep.wake_failures`, `last_error`), and is tried
again on the next request that needs the database (at most every 10 s).
`/_engine/health` reports a sleeping database as `ok` and never wakes it.

**What it saves:** the PostgreSQL server's whole memory while it sleeps
(measured: 33 to 38 MiB for an idle PostgreSQL 17 container serving one
small site, more for a busy one whose caches have filled) and its background work (the
checkpointer, autovacuum, the WAL writer waking up). On an idle site it
wakes for nothing but due jobs: with the engine's own hourly cleanup job,
once an hour. `scripts/check-db-sleep.sh` measures the wake and holds it
to a budget.

## The built-in server

`engine serve` is fine for development and small installs. It:

- listens on `127.0.0.1` unless told otherwise;
- answers only `GET` and `HEAD` for files (with accounts and forms, the
  `/account/`, `/admin/` and `/_engine/forms/` routes also take `POST`, each
  checked for its CSRF token);
- refuses `..`, encoded traversal, backslashes and hidden files, and checks
  that every file it serves is inside the output folder, following symlinks;
- sends `X-Content-Type-Options: nosniff`;
- sends the pre-compressed Brotli or gzip copy of a text file when the
  browser takes it, with `Vary: Accept-Encoding`, an ETag per encoding and
  `304` answers ([pre-compressed copies](/docs/publishing/#pre-compressed-copies)).

## What to commit

Commit `site.toml`, `content/`, `templates/` and `static/`. Do not commit the
output folder; build it where you deploy, or in CI. Nor `.engine/`: the
engine's own state there (the [form spool](/docs/forms/)) belongs to the
running site, not to its source.
