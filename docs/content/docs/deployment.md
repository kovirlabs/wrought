+++
title = "Deployment"
description = "Serve the output folder with Caddy, nginx or the built-in server."
weight = 8
+++

A built site is a folder of files. Anything that serves files can serve it.

## Caddy in front

The intended setup puts Caddy (or nginx) in front: it serves the output folder
directly and, in later releases, passes misses and dynamic routes to the
engine. A minimal Caddyfile for a build-mode site:

```text
example.org {
	root * /srv/site
	encode zstd gzip
	@hidden path /.*
	respond @hidden 404
	header /_engine/* Cache-Control "public, max-age=31536000, immutable"
	file_server
	handle_errors {
		@404 expression {err.status_code} == 404
		rewrite @404 /404.html
		file_server
	}
}
```

The vendored TagBrush and TagAction files have their version in the file name,
so they can be cached forever. The repository's `deploy/` folder has this
Caddyfile and a Docker Compose file that serves `docs/public` this way:

```sh
engine build --site docs
docker compose -f deploy/compose.yml up -d
curl -I http://127.0.0.1:8790/docs/
```

The companion Bloomery project deploys the full stack (Caddy, the engine and
PostgreSQL) to cloud, bare-metal and local Docker hosts.

## The built-in server

`engine serve` is fine for development and small installs. It:

- listens on `127.0.0.1` unless told otherwise;
- answers only `GET` and `HEAD`;
- refuses `..`, encoded traversal, backslashes and hidden files, and checks
  that every file it serves is inside the output folder, following symlinks;
- sends `X-Content-Type-Options: nosniff`.

## What to commit

Commit `site.toml`, `content/`, `templates/` and `static/`. Do not commit the
output folder; build it where you deploy, or in CI.
