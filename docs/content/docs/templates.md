+++
title = "Templates and components"
description = "Layouts, the built-in component library, and overriding both."
weight = 5
+++

Templates use [minijinja](https://docs.rs/minijinja), a Jinja2 dialect. They
are read at build time, so editing one needs no recompiling, and `engine dev`
rebuilds as you save.

## Where templates come from

A template name such as `page.html` is looked up in the site's `templates/`
folder first, then among the templates built into the engine. So a site
overrides any built-in layout or component by adding a file with the same
name:

```text
templates/
  base.html                  replaces the built-in page skeleton
  components/post_meta.html  replaces one component
```

Names must be relative paths without `..`.

## Built-in layouts

| Template | Used for |
| --- | --- |
| `base.html` | The page skeleton: head, skip link, header, `main`, footer |
| `home.html` | The home page (`content/_index.md`): a hero, then the page |
| `page.html` | Pages: one `<article>` with breadcrumbs and the content |
| `section.html` | Section indexes: the text, a list of pages and pagination |
| `404.html` | Written to `404.html`, served for missing pages |

`base.html` defines the blocks `title`, `head`, `header`, `body`, `main` and
`footer`. Extend it and fill the ones you need:

```jinja
{% extends "base.html" %}
{% block main %}
<article>{{ page.content }}</article>
{% endblock %}
```

## What templates can read

| Variable | Contents |
| --- | --- |
| `site` | `[site]` from site.toml: `name`, `base_url`, `nav`, `extra`, ... |
| `page` | The page: `title`, `url`, `content`, `summary`, `date`, `toc`, `extra`, ... |
| `listing` | On section pages: `pages`, `number`, `total`, `prev_url`, `next_url`, `urls` |
| `breadcrumbs` | Ancestor sections, `[{ title, url }]` |
| `route` | `path`, `mode`, `template` |
| `ui` | Stylesheet, script and theme URLs from `[ui]` |
| `engine` | `version`, `preset` |
| `user`, `csrf`, `request` | Only in `request` renders; see [the render dial](/docs/render-dial/#what-request-does) |

Functions: `asset(path)` (below), `component(name, args, ...)`, `component_assets(name)`,
`component_catalogue()`, `video_source(url)`,
`fragment(template, key=..., per_user=...)`
([fragment cache](/docs/render-dial/#the-fragment-cache)),
`section_pages(section, limit=n)`, `get_page(id)` and `tag(name)`; with
database content `list_items`, `get_item`, `get_singleton`, `list_terms`
and `render_block` ([content](/docs/content-types/)); with forms
`form_view(name)` and `my_submissions(form=...)` ([forms](/docs/forms/));
on request pages `table_view(...)` and `chart_svg(...)`
([App UI](/docs/app-components/)). Filters: `date` (`20 November 2026`;
`date("short")` gives `20 Nov 2026`), `absolute_url`, `markdown` (Markdown
from an editor or visitor to safe HTML: raw HTML shown as text, unsafe
links dropped), and minijinja's built-ins (including `urlencode`).

### Linking your own CSS, scripts, fonts and images: `asset()`

Link files of the site's `static/` folder through `asset()`, never by a
hand-written path:

```jinja
<link rel="stylesheet" href="{{ asset('/css/site.css') }}">
<script src="{{ asset('/js/site.js') }}" defer></script>
```

`asset('/css/site.css')` gives `/css/site.3f2a9c1d0b.css`: the file's name
with a hash of its bytes. Builds and `engine serve` write every `static/`
file under both names (the hashed one is a hard link, so no extra disk), and
the hashed name is served `Cache-Control: public, max-age=31536000,
immutable`: browsers and CDNs keep it for a year and never ask again. Edit
the file and its hash, so its URL, changes; `engine serve` sees the edit and
republishes the pages that link it, so a refresh shows it with no purge. A
missing file is a build error. `[ui] css` or `behaviour` given as a site
path (`"/css/site.css"`) is linked the same way. Files linked by their plain
path still work but are revalidated on every use; see
[browser caching](/docs/deployment/#browser-caching). A stylesheet's own
`url(...)` references are relative to it and keep working (they are not
rewritten, so they are revalidated rather than kept for a year).

## Escaping

Auto-escaping is on for `.html`, `.xml` and `.txt` templates: any value you
print is HTML-escaped unless it is marked safe. HTML the engine made arrives
already marked safe, so you print it as it is:

- rendered Markdown: `{{ page.content }}`, `{{ p.summary }}`;
- `markdown` fields of database content, after sanitising:
  `{{ page.body }}` (see [safe output](/docs/content-types/#safe-output));
- `component(...)` and `render_block(...)` output.

Everything else, including every text field from the database, is escaped.
So templates never need `| safe`, and should not use it: on a plain value it
turns escaping off and lets content inject script. `engine build` and
`engine serve` warn about every `| safe` in a site's templates
(`templates/x.html:12: | safe turns off HTML escaping ...`).

## Components

A component is a template in `components/` that you call with arguments:

```jinja
{{ component("card", title="Plain HTML", href="/docs/", text="No classes.") }}
```

The component sees its arguments plus `site`, `page`, `listing`,
`breadcrumbs`, `ui` and `engine`. Every built-in component emits semantic
HTML without classes, so TagBrush.css styles it and TagAction adds behaviour.
See them all in the [component gallery](/components/), and
[the component library](/docs/components/) for the full list, how
components load scripts, manifests and versioning. The table below lists
the ones most layouts use.

| Component | Arguments | Renders |
| --- | --- | --- |
| `header` | `hero`, `sticky` | Site name, `[site] nav` (a phone menu on small screens), optional hero |
| `hero` | `title`, `text`, `label`, `actions` | An `<hgroup>` with buttons |
| `footer` | none | `site.extra.footer_nav` columns and `footer_text` |
| `breadcrumbs` | `items` | `<nav aria-label="Breadcrumb">` |
| `pagination` | none | Links between listing pages |
| `section_nav` | `section`, `title` | A section's pages, current one marked |
| `prev_next` | `section` | Previous and next page in a section |
| `toc` | `title`, `min`, `max` | Links to the page's headings |
| `card`, `cards` | `title`, `href`, `text`, `meta`, `level`; `items` | An `<article>`; a grid of them |
| `page_list` | `pages`, `level` | Pages as cards with date and summary |
| `post_meta` | none | Date, reading time, author and tags |
| `callout` | `title`, `text`, `tone` | An `<aside>`, coloured by `tone` |
| `cta` | `title`, `text`, `label`, `url` | A call to action |
| `accordion` | `items`, `name` | `<details>` elements, one open at a time |
| `quote` | `text`, `cite` | A `<blockquote>` with attribution |
| `section`, `columns`, `grid`, `stack`, `container`, `spacer`, `divider` | see the [library](/docs/components/#layout) | Layout |
| `menu`, `mobile_menu`, `tabs` | `items`, `label`, ... | Navigation |
| `rich_text`, `video_embed`, `timeline` | see the [library](/docs/components/#content) | Content |

Components for other features (listings, forms, accounts, data tables) ship
with those features.
