+++
title = "Command line"
description = "engine new, build, dev, serve, check, and the cms commands: migrate, types, content and rebuild."
weight = 9
+++

Every command that works on a site takes `--site <folder>` (default: the
current folder). `engine --help` and `engine <command> --help` list all flags.

## engine new

```sh
engine new my-site --preset static
```

Creates `my-site/` from an example and names the site after the folder.
Refuses to write into a folder that is not empty. `--preset static` copies the
static blog; `--preset cms` copies the CMS example (needs an engine built with
the `cms` preset); `--preset app` explains that it needs later releases.

`engine --version` shows the preset a binary was built with. A `static`
binary has no `migrate`, `types`, `content` or `rebuild` commands; it says so
and how to get a `cms` build (`cargo build --release --features cms`).

## engine build

```sh
engine build [--output <folder>] [--drafts] [-v]
```

Renders every `build` and `publish` route into the output folder
(`[site] output`, default `public/`). For a site with database content it also
writes the publish [tag index](/docs/publishing/). The build renders into a staging folder and swaps it in at the end,
so a failed build leaves the previous output as it was. It only replaces an
output folder that an earlier build created (it holds `.engine-manifest.json`)
or that is empty.

`-v` lists every route with its mode, template and cache tags.

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
engine serve [--dir <folder>] [--port 4400] [--host 127.0.0.1]
```

Serves a built site with the built-in static file server: folders serve their
`index.html`, missing pages get `404.html` with status 404. For a site with
database content (cms builds), it also runs the publish listener, which
rewrites pages when content changes, and renders a missing `publish` page on
request ([publishing](/docs/publishing/)). It listens on
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

## Database commands (cms preset)

These exist in engines built with the `cms` preset. See
[content types](/docs/content-types/) and [publishing](/docs/publishing/).

| Command | Does |
| --- | --- |
| `engine types sync [--check] [--allow-drop] [--snapshot-only]` | write a migration for changes to `types/*.toml` |
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

The database comes from `ENGINE_DATABASE_URL` or `[database] url`.
