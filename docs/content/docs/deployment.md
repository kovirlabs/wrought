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
would go stale. The repository's `deploy/Caddyfile.publish` and
`deploy/compose.publish.yml` do this for `examples/cms-site`:

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

## A site on SQLite

A SQLite site is one folder: `site.toml`, templates, the database file and
the media folder. Point `ENGINE_DATABASE_URL` (or `[database] url`) at
`sqlite:///srv/site/data/site.db`, run one `engine serve` (SQLite has one
writer; the engine waits for the write lock rather than failing), and back
up with `sqlite3 data/site.db ".backup backup.db"` plus the media folder.
Jobs and mail need PostgreSQL; see [databases](/docs/databases/).

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
output folder; build it where you deploy, or in CI.
