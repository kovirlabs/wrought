+++
title = "Wrought"
description = "One Rust engine for static sites and web apps."
+++

Wrought is one open source Rust engine for both static sites and full web apps.
Each route chooses **when** it renders, on a three-position *render dial*, and
each capability is a Cargo feature you switch on at build time. With nothing
switched on, Wrought is a static site generator; switch on accounts, admin and
jobs and it becomes a web app.

This site is built by Wrought itself, in build mode, from Markdown files in the
repository's `docs/` folder.

## What works today

Version 0.5 is the **polish** milestone. A default build is still a static
site generator:

- `build` render mode: every page is rendered to a file on disk.
- Markdown content with TOML front matter, sections, pagination and drafts.
- minijinja templates with auto-escaping and a library of built-in components.
- The Kovir Labs Tag stack bundled in the binary: TagBrush.css for styling and
  TagAction for behaviour, served from your own site.
- `engine new`, `engine build`, `engine dev` with live reload, `engine serve`
  and `engine check`.

Built with the `cms` preset it adds a data layer on
[SQLite or PostgreSQL](/docs/databases/),
[content types](/docs/content-types/) declared in TOML (with migrations,
revisions, taxonomy, blocks and safe rendering), the `publish` render
mode (when content changes, [only the pages that show it](/docs/publishing/)
are rewritten), accounts and a server-rendered [admin](/docs/admin/) with
roles, conflict-safe editing and review before publishing, a
[media library](/docs/media/) that checks and cleans uploads and makes
responsive images, and [full-text search](/docs/search/) with results
that need no JavaScript.

The `app` preset adds the `request` render mode's
[fragment cache](/docs/render-dial/#the-fragment-cache),
[forms](/docs/forms/) with a spam guard, background
[jobs and email](/docs/jobs-and-mail/), and the
[Accounts and App UI components](/docs/app-components/), and a
[JSON API](/docs/api/) with scoped tokens. How it compares with WordPress,
Drupal and Astro on one machine: [benchmarks](/docs/benchmarks/).
