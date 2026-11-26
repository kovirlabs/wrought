+++
title = "Content"
description = "Markdown files, front matter, sections and drafts."
weight = 3
+++

## Files become pages

Every `.md` file under `content/` is a page. Its URL comes from its path:

| File | URL | Kind |
| --- | --- | --- |
| `content/_index.md` | `/` | home page (section index of the root) |
| `content/about.md` | `/about/` | page |
| `content/blog/_index.md` | `/blog/` | section index |
| `content/blog/first-post.md` | `/blog/first-post/` | page in the `blog` section |
| `content/docs/guide/index.md` | `/docs/guide/` | section index of `docs/guide` |

Each page is written as `<url>/index.html`, so links never need `.html`. File
and folder names are lower-cased and spaces become `-`; other characters
outside letters, digits, `-`, `_` and `.` are an error. Files and folders
starting with `.` are ignored.

`_index.md` and `index.md` both mean "the index of this folder".

## Front matter

Front matter is TOML between `+++` lines at the very top. Everything is
optional.

```toml
+++
title = "Sharpening a plane iron"
description = "Three stones, one guide, ten minutes."
date = 2026-11-20
tags = ["tools"]
+++
```

| Key | Meaning |
| --- | --- |
| `title` | Page title. Default: the first `#` heading, else the file name |
| `description` | Summary for meta tags and listings |
| `date`, `updated` | `2026-11-20` or a full timestamp |
| `draft` | `true` leaves the page out unless you build with `--drafts` |
| `weight` | Order in listings sorted by weight (lower first) |
| `slug` | Replaces the last part of the URL |
| `template` | Template for this page |
| `mode` | Render mode for this page |
| `tags`, `author` | Shown by the `post_meta` component |
| `extra` | A table of anything; templates read `page.extra.*` |

A section index (`_index.md`) also accepts:

| Key | Meaning |
| --- | --- |
| `paginate` | Pages per listing page; later pages are at `/blog/page/2/` |
| `sort_by` | `date` (newest first), `weight` or `title`. Default: `date` if any page has one, else `weight` |
| `page_template` | Template for the section's pages |

YAML front matter (`---`) is not supported; the error message says so.

## Markdown

Wrought renders CommonMark plus tables, footnotes, strikethrough, task lists
and smart punctuation. Headings get `id`s from their text (`## Getting
started` becomes `id="getting-started"`), so TagAction adds anchor links and
the `toc` component can link to them. Set your own with `## Title {#my-id}`.

A page's **summary**, used in listings, is its first paragraph, or everything
before a `<!-- more -->` line.

Raw HTML in Markdown files is passed through: the person writing the files
also controls the site. Content stored in the database (the `cms` preset) is
always sanitised; see [content types](/docs/content-types/#safe-output).

## Static files

Everything in `static/` is copied to the output unchanged: `static/img/a.png`
is served at `/img/a.png`. A static file that would overwrite a page is an
error. `_engine/` is reserved for files the engine provides.

## Cache tags

Every render records which content it read as cache tags: a page is
`i:<type>:<id>`, where the type is its top-level folder (`blog`) or `page`, and
the id is its path without `.md` (`blog/first-post`). Listings record
`c:<type>`. See [the render dial](/docs/render-dial/).
