+++
title = "Command line"
description = "engine new, build, dev, serve, check, and the database commands: migrate, types, content, rebuild, user, jobs, media, search and api."
weight = 10
+++

Every command that works on a site takes `--site <folder>` (default: the
current folder). `engine --help` and `engine <command> --help` list all flags.

## engine new

```sh
engine new my-site --preset static [--theme docs|business|dashboard]
```

Creates `my-site/` from an example and names the site after the folder.
Refuses to write into a folder that is not empty. `--preset static` copies the
static blog; `--preset cms` copies the CMS example (needs an engine built with
the `cms` or `app` preset); `--preset app` copies the small app (accounts,
forms, emailed receipts; needs an `app` build).

`--theme` lays a [starter theme](/docs/themes/) over the preset's example:
`docs`, `business` (with any preset) or `dashboard` (cms or app). `engine new
--theme list` lists them.

```sh
engine new handbook --preset static --theme docs
engine new console --preset cms --theme dashboard
```

`engine --version` shows the preset a binary was built with. A `static`
binary has no `migrate`, `types`, `content`, `rebuild`, `user` or `jobs`
commands; it says so and how to get a fuller build (`cargo build --release
--features app`, or `cms`).

## engine build

```sh
engine build [--output <folder>] [--drafts] [-v] [--jobs <n>] [--timings]
```

Renders every `build` and `publish` route into the output folder
(`[site] output`, default `public/`). For a site with database content it also
writes the publish [tag index](/docs/publishing/). The build renders into a staging folder and swaps it in at the end,
so a failed build leaves the previous output as it was. It only replaces an
output folder that an earlier build created (it holds `.engine-manifest.json`)
or that is empty.

`-v` lists every route with its mode, template and cache tags. With the
`search` feature it also rebuilds the full-text index; with `[search]
pagefind = true` it runs [Pagefind](/docs/search/#on-a-static-site-pagefind)
over the result. With `[publish] target = "s3"` (the `s3` feature) it then
uploads what differs to the bucket, deletes what is gone and invalidates
the changed paths at the CDN ([publishing to a bucket](/docs/buckets/)).

Pages render on several threads: `--jobs` (or `ENGINE_BUILD_JOBS`) sets how
many, by default one per CPU the process may use. The output is the same as
with one thread. A site with database content is read once into memory for
the build (one query per content type) instead of several queries per page.
On a host that boots with `isolcpus` the kernel keeps every thread on the CPU
the process started on; there `ENGINE_BUILD_PIN_CPUS=1` pins each render
thread to its own CPU of the allowed set (Linux).

`--timings` prints, after the build, how long each phase took (connecting,
the search index, loading templates, reading the content, the route table,
rendering and writing, the swap, the tag index...) with its share of the
total, on standard error. It is how a slow build is told apart from a slow
disk or database; the benchmark records it for every build.

On PostgreSQL a build waits for no disk sync: the search index is an
unlogged table (derived data, refilled when empty) and the tag index commits
without waiting for the write-ahead log, like the pages it describes, which
are written without `fsync`. After a crash of the machine, run `engine
rebuild`.

## engine dev

```sh
engine dev [--port 4400] [--host 127.0.0.1] [--drafts] [--no-check]
```

Builds, serves the output with the built-in server, watches the site folder
and rebuilds on every save. Open pages reload themselves: the dev server adds
a small script to HTML responses (never to the files on disk). If the build
fails, the error is printed and the last good output stays up.

After each build `engine dev` runs TagCheck, when it can find it, and prints a
summary of problems.

## engine serve

```sh
engine serve [--dir <folder>] [--port 4400] [--host 127.0.0.1] [--no-workers]
```

Serves a built site with the built-in static file server: folders serve their
`index.html`, missing pages get `404.html` with status 404. For a site with
database content (cms builds), it also runs the publish listener, which
rewrites pages when content changes, and renders a missing `publish` page on
request ([publishing](/docs/publishing/); with a bucket target it publishes
to the bucket and renders nothing on request); with accounts it answers
`/account/` (sign-in, sign-out, password reset), `/admin/` and `request`
pages ([the admin](/docs/admin/)); with `forms` it takes form posts
([forms](/docs/forms/)); with `jobs` it runs job workers (`--no-workers`
leaves them to `engine jobs work`, see [jobs and mail](/docs/jobs-and-mail/));
and it reloads templates and Markdown when they change. It listens on
localhost only unless you pass `--host`. For production, put Caddy or nginx in
front, or let them serve the folder directly; see
[deployment](/docs/deployment/).

## engine check

```sh
engine check [--dir <folder>] [--strict] [--browser]
```

Runs [TagCheck](https://github.com/kovirlabs/tagcheck) over the built site: it
finds missing alt text and labels, fake buttons, skipped headings, broken id
references and invented classes. `--browser` adds contrast and overflow checks
in Chromium (needs Playwright installed next to TagCheck). The exit code is
TagCheck's: 0 when clean, 1 when there are problems.

TagCheck is a Node.js program (Node 20 or later), so it is not built into the
engine. `engine check` looks for, in order:

1. the command in the `WROUGHT_TAGCHECK` environment variable;
2. `tagcheck` on your `PATH` (`npm install -g @kovirlabs/tagcheck@0.1.3`);
3. `npx`, which runs the pinned `@kovirlabs/tagcheck@0.1.3`.

Without any of them it says how to install it. Note that the unscoped npm
package `tagcheck` is a different project.

In the Wrought repository, `scripts/check-a11y.sh` runs TagCheck and
axe-core over the example sites, these docs and the admin.

## engine components

```sh
engine components [--site <folder>] [--json]
```

Lists the [component library](/docs/components/) of this build: each
component's version, the features it needs and whether it has a script or
stylesheet; in a site folder also the site's overrides and own components,
and its design tokens. `--json` prints everything, with every component's
arguments, editor hints and example, for tools and the visual editor.

## Database commands (cms and app presets)

These exist in engines built with the `cms` or `app` preset (each with the
feature it needs). See [content types](/docs/content-types/),
[publishing](/docs/publishing/) and [jobs and mail](/docs/jobs-and-mail/).

| Command | Does |
| --- | --- |
| `engine types sync [--check] [--allow-drop] [--snapshot-only]` | write a migration (one per backend) for changes to `types/*.toml` |
| `engine types list` | show types, fields, blocks and vocabularies |
| `engine migrate [--status]` | apply pending engine and site migrations |
| `engine content seed` | create items from `seed/*.toml` (skips existing) |
| `engine content list <type> [--all] [--order o] [--limit n]` | list items |
| `engine content show <type> [<id or slug>]` | print one item as JSON |
| `engine content create <type> --set f=v... [--slug s] [--publish]` | create an item |
| `engine content set <type> [<id or slug>] --set f=v... [--publish/--unpublish] [--expect-version n]` | change an item |
| `engine content delete <type> <id or slug> [--expect-version n]` | delete an item |
| `engine content revisions <type> [<id or slug>]` | list revisions |
| `engine content terms <vocabulary>` | list terms |
| `engine rebuild [--tag <tag>]...` | re-render everything, or the pages carrying a tag |
| `engine user create --email e [--name n] [--role r]... [--password-stdin]` | create an account; the first one is an administrator |
| `engine user list` | list accounts, roles, two-factor state, last sign-in |
| `engine user reset-link --email e` | print a single-use link (one hour) to set the password |
| `engine user set --email e [--role r]... [--no-roles] [--active true/false]` | change roles, block or unblock |
| `engine user totp-off --email e` | turn off two-factor sign-in (lost authenticator) |
| `engine jobs list [--status queued/running/done/dead] [--limit n]` | list background jobs, with counts and last errors |
| `engine jobs retry <id>` | queue a dead job again with fresh attempts |
| `engine jobs work` | run job workers only, without the web server |
| `engine media add <file> [--alt text]` | add a file to the media library (checked and cleaned like an upload); prints its number |
| `engine media list [--limit n]` | list the media library |
| `engine search reindex` | rebuild the full-text index from the published content |
| `engine search query <words> [--type t] [--limit n]` | try a search |
| `engine api token create --email e --name n [--scope s]... [--days n]` | make an API token for an account (printed once) |
| `engine api token list` / `revoke <number>` | list tokens (never their secrets) / revoke one |

The database comes from `ENGINE_DATABASE_URL` or `[database] url`. There are
no default accounts: create the first administrator with

```sh
engine user create --email you@example.org --name "You"
```

which prints a link to choose the password (or pipe one in with
`--password-stdin`).
