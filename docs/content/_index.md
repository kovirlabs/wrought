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

Version 0.1 is the **static** milestone:

- `build` render mode: every page is rendered to a file on disk.
- Markdown content with TOML front matter, sections, pagination and drafts.
- minijinja templates with auto-escaping and a library of built-in components.
- The Kovir Labs Tag stack bundled in the binary: TagBrush.css for styling and
  TagAction for behaviour, served from your own site.
- `engine new`, `engine build`, `engine dev` with live reload, `engine serve`
  and `engine check`.

The `publish` and `request` render modes, the database, accounts and the admin
UI follow in later releases; see [the render dial](/docs/render-dial/).
