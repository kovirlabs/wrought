+++
title = "The render dial"
description = "Build, publish and request: when each route makes its HTML."
weight = 4
+++

Every route picks one of three render modes. The mix decides whether a site
acts static, hybrid or app-like.

| Mode | When HTML is made | Stored | Typical use | Available |
| --- | --- | --- | --- | --- |
| `build` | At `engine build` | Files on disk | Docs, marketing pages, blogs | 0.1 |
| `publish` | When content changes | Files on disk, swapped atomically | CMS sites, catalogues, news | planned, 0.2 |
| `request` | On every request | Fragment cache in memory | Accounts, carts, dashboards, search | planned, 0.4 |

## Setting the mode

From most to least specific:

1. `mode` on a `[[routes]]` entry with the page's path;
2. `mode` in the page's front matter;
3. `[render] default` in `site.toml` (itself `build` by default).

## Modes that are not in this build

`publish` and `request` are real values: the parser accepts them, so sites can
be written for later releases. Validation then stops with a message such as:

```text
error: route `/account/` uses render mode `request`, which is not in this
build: this engine was compiled with the `static` preset, which only supports
`build`. Set `mode = "build"`, or use a build with the `request` feature
(planned for a later release).
```

## Features and presets

Capabilities are Cargo features, so code that is switched off is not compiled
or shipped. This release has one preset, `static` (the always-on core). The
`cms` and `app` presets follow as their features land; `engine --version`
shows what a binary contains.

## Cache tags

Every render records the data it read, as tags:

| Tag | Meaning | Recorded when |
| --- | --- | --- |
| `i:<type>:<id>` | one item | a page renders itself, or a template calls `get_page(id)` |
| `c:<type>` | a collection | a section listing renders, or a template calls `section_pages(...)` |
| `s:<name>` | a singleton | every page records `s:site`; templates can call `tag("menu")` |

In `build` mode nothing needs them yet: `engine build -v` prints them per
route, and the build writes them to `.engine-manifest.json` in the output
folder. `publish` mode will use them to decide which files to rewrite after an
edit, and `request` mode to evict cached fragments.
