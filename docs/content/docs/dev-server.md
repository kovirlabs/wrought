+++
title = "The dev server"
description = "engine dev serves the whole app with live reload, error pages with the template line and the request's queries, a query log with N+1 warnings, and mail captured in the browser."
weight = 24
+++

`engine dev` is the command to work on a site with. It builds the site,
serves it, rebuilds on every save and reloads open pages. For a site with a
database it runs the same server as `engine serve` (request pages, accounts,
forms, the admin, job workers, live regions and an app crate's handlers),
plus the developer tools on this page.

```sh
engine dev [--port 4400] [--host 127.0.0.1] [--drafts] [--no-check] [--n-plus-one 5]
```

The tools exist only under `engine dev`. They are switched on by the
command, never by a setting: nothing in `site.toml`, the environment or the
site's code can switch them on for `engine serve`, which answers exactly
as it always has (a bare `500`, the error in the log; see
[deployment](/docs/deployment/)). `engine dev` listens on `127.0.0.1` by
default; its pages show queries and errors, so do not expose it with
`--host 0.0.0.0` on a network you do not trust.

## Every request's queries

Each request that runs a query, serves a page or fails is printed to the
terminal with its time and its queries: the statement (its *shape*: values
are bound parameters, and are never recorded), how long it took and how
many rows it returned:

```text
GET /updates/ 200 in 6.3 ms, 2 queries (1.4 ms)
     0.9 ms    3 rows  SELECT id, title, ... FROM content_update WHERE ...
     0.5 ms     1 row  SELECT ... FROM engine_users WHERE ...
```

The same is on the request's page, `/_engine/dev/requests/<id>`, with the
call site of each query (the engine's statement name and the Rust file and
line that ran it, or `App::query("name")` for an app crate's named query).
`/_engine/dev/` lists the last hundred requests.

Every HTML page also gets a small link in its bottom right corner (the
overlay): how many queries the page ran, how long they took of the whole
request, and a warning when it looks like an N+1. It is added to the
response, never to the files on disk.

## N+1 warnings

When one request runs the same statement shape `--n-plus-one` times or
more (default 5), the terminal, the overlay and the request's page say it
is likely an N+1: a query per item of a list, where one query for the whole
list would do. The warning names where the statement first ran from:

```text
GET /many/ 200 in 21.1 ms, 6 queries (9.6 ms)
    2.2 ms    2 rows  SELECT id, slug, status, version, ... FROM content_update WHERE ...
    ...
  warning: likely N+1: the same statement ran 6 times in this request: SELECT id, slug, ...
           first from crates/engine-content/src/store.rs:752
```

(A content type's queries are built per type, so they have no catalogue
name: the call site is the store function that ran them. The template that
called it is the page's.)

Fix it in a template by listing once (`list_items` with a filter, a named
query that joins) instead of looking up each item; in a handler, by one
query with `= ANY($1)` and the ids as an array.

## Error pages

A request that fails with a `500` under `engine dev` shows:

- the **error chain**, from the outermost context to the database's or the
  template engine's own message;
- for a template error, the **template file and line**, with the lines
  around it (the innermost template: an error inside an include or a
  component names that file);
- the **request**: method, address and headers (cookies and authorization
  hidden);
- the **queries** the request ran before it failed, the failing one with
  the database's error.

A client that asks for JSON (`Accept: application/json`) gets the same
chain as text. Handler errors (`HandlerResult` and `?`) and `request`-mode
pages show it; a handler that builds its own `500` response has nothing to
show but the terminal's line.

A `404` lists the site's routes nearest to the address asked for (a typo in
a path, a missing trailing slash, a pattern route such as
`/updates/{slug}/`), and all of them under a fold: the same list as
[`engine routes`](/docs/generators/#engine-routes).

A live region whose stream is refused (`403` from `/_engine/live`) gets an
explanation: which of its tags are public and which are not, and the rule
([live regions](/docs/realtime/)).

## Mail in the browser

Under `engine dev` mail is never sent. Messages go through the job queue
and the templates as in production, and the job worker keeps them instead
of handing them to an SMTP server, whatever `ENGINE_SMTP_URL` says. They
are listed at `/_engine/dev/mail/`, newest first (the last 200), with
their headers as they would have gone out, the text part and the HTML part
(in a sandboxed frame: no scripts). The terminal prints a line for each.

The same page lists every mail template (the site's `templates/mail/` and
the built-in ones) with a preview: rendered with sample data, sent to
nobody. The sample data of `templates/mail/<name>.txt` is
`templates/mail/<name>.sample.json`, a JSON object, if there is one (the
engine's built-in templates have their own; `engine generate mailer` writes
one); without it the template is rendered with `site_name`, `site_url` and
`to_name` only.

## Rebuilds and app crates

Templates, content files and `static/` are picked up on save: pages are
written again (published pages re-rendered, `build` pages rebuilt) and the
open pages reload. `site.toml`, `types/`, `forms/` and `migrations/` need a
restart of `engine dev`, which it says.

An [app crate](/docs/app-crates/) runs from its own compiled binary, so a
change to its Rust code needs that binary built again; `engine dev` says so
when `src/` changes. Run it through Cargo and let `cargo watch` (or
`watchexec`) restart it on every save:

```sh
cargo install cargo-watch      # once
cargo watch -x 'run -- dev'    # in the crate's folder
```

## Cost in production

None. `engine serve` sets no layer, adds no route and keeps no log: each
query checks one process-wide flag that only `engine dev` sets (one atomic
load, measured within the noise of `scripts/check-speed.sh`).
