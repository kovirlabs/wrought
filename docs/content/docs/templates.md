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
<article>{{ page.content | safe }}</article>
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

Functions: `component(name, ...)`, `section_pages(section, limit=n)`,
`get_page(id)` and `tag(name)`. Filters: `date` (`20 November 2026`;
`date("short")` gives `20 Nov 2026`) and `absolute_url`.

## Escaping

Auto-escaping is on for `.html`, `.xml` and `.txt` templates: any value you
print is HTML-escaped unless it is marked safe. Rendered Markdown is printed
with `{{ page.content | safe }}`, because it is HTML you wrote.
Component output is already safe.

## Components

A component is a template in `components/` that you call with arguments:

```jinja
{{ component("card", title="Plain HTML", href="/docs/", text="No classes.") }}
```

The component sees its arguments plus `site`, `page`, `listing`,
`breadcrumbs`, `ui` and `engine`. Every built-in component emits semantic
HTML without classes, so TagBrush.css styles it and TagAction adds behaviour.
See them all in the [component gallery](/components/).

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

Components for later features (forms, accounts, data tables) ship with those
features.
