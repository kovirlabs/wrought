+++
title = "Site configuration"
description = "Every key site.toml accepts in version 0.2."
weight = 2
+++

A site is a folder with a `site.toml`. Unknown keys are errors, so a typo is
reported instead of ignored.

```toml
[site]
name = "Example"                  # required
base_url = "https://example.org"  # for canonical links and sitemap.xml
output = "public"                 # build output folder, inside the site
description = "Shown in search results and the home page hero."
author = "Your Name"
language = "en"                   # <html lang>
nav = [ { label = "Blog", url = "/blog/" } ]

[site.extra]                      # anything; templates read site.extra.*
footer_text = "© Example"
favicon = "/favicon.svg"

[render]
default = "build"                 # build | publish | request

[[routes]]
path = "/contact/"
template = "contact.html"
title = "Contact"

[ui]
css = "tagbrush"
behaviour = "tagaction"

[ui.tokens]
primary = "#b45309"

# cms preset only:
[database]
url = "postgres://engine@localhost/example"   # or ENGINE_DATABASE_URL

[publish]
target = "disk"

[[routes]]
path = "/articles/{slug}/"
type = "article"
template = "article.html"
```

## [site]

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | required | Site name, shown in the header and page titles |
| `base_url` | empty | Absolute address. Enables `sitemap.xml` and canonical links |
| `output` | `public` | Output folder. Must be inside the site, not `content` or `templates` |
| `description` | none | Default meta description |
| `author` | none | Default author, shown in the footer and on posts |
| `language` | `en` | Value of `<html lang>` |
| `nav` | `[]` | Header links, `{ label, url }` |
| `extra` | `{}` | Free-form values for your templates |

The built-in footer reads `site.extra.footer_text` and `site.extra.footer_nav`
(columns of `{ title, links = [{ label, url }] }`); the base layout reads
`site.extra.favicon`.

## [render]

`default` is the render mode for every page that does not set its own. See
[the render dial](/docs/render-dial/). In this release only `build` is
available in every build, `publish` in `cms` builds; `request` is accepted by
the parser and then rejected with a message saying it is not in this build.

## [[routes]]

Routes come from content files automatically. Add a `[[routes]]` entry to:

- render a page that has **no content file**, from a template alone (this
  site's [component gallery](/components/) is one);
- **override** the template, mode or title of a content page with the same path.

| Key | Meaning |
| --- | --- |
| `path` | URL, starting with `/`. `/about` and `/about/` are the same route |
| `template` | Template name, e.g. `contact.html`. Required without a content file |
| `mode` | Render mode for this route |
| `title`, `description` | Page title and description for template-only pages |
| `requires` | A feature the route needs, e.g. `auth`; checked against this build |
| `type` | A content type: one page per published item; `path` has `{slug}` or `{id}` |
| `taxonomy` | A vocabulary: one page per term; `path` has `{term}` |

Wildcard paths such as `/account/*` need `request` mode and are rejected in a
static build.

## [ui]

Chooses the stylesheet and the behaviour script every page loads, and sets
design tokens. See [styling](/docs/styling/).

## [database] and [publish]

Used by `cms` builds; see [content types](/docs/content-types/#the-database)
and [publishing](/docs/publishing/#publish-targets).

| Key | Meaning |
| --- | --- |
| `[database] url` | `postgres://...`; `ENGINE_DATABASE_URL` overrides it |
| `[database] max_connections` | Pool size (default 5) |
| `[publish] target` | `disk` (default). `s3` is planned |

A `static` build parses both sections, so a site written for a fuller build
still loads, and warns that they are ignored.
