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

# cms and app presets:
[database]
url = "postgres://engine@localhost/example"   # or ENGINE_DATABASE_URL

[publish]
target = "disk"

# app preset:
[cache]
fragment_ttl_seconds = 600

[jobs]
workers = 4

[mail]
from = "Example <hello@example.org>"           # SMTP server: ENGINE_SMTP_URL

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
available in every build; `publish` and `request` in `cms` builds. A build
without them accepts the values and then stops with a message saying they are
not in this build.

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
| `requires` | A feature the route needs, checked against this build. `requires = "auth"` also means only signed-in visitors see it (needs `mode = "request"`) |
| `always_static` | Serve the stored file even to signed-in visitors and URLs with a query string |
| `list` | A paginated list of a content type: `[routes.list]` with `type`, `per_page`, `order`, `term` ([details](/docs/content-types/#paginated-lists)) |
| `type` | A content type: one page per published item; `path` has `{slug}` or `{id}` |
| `taxonomy` | A vocabulary: one page per term; `path` has `{term}` |

Wildcard paths such as `/app/*` need `request` mode and are rejected in a
static build. Paths the engine answers itself in a build are reserved:
`/_engine/`, and with accounts `/account/login/`, `/account/logout/`,
`/account/reset/`, `/account/totp/`, `/account/register/`,
`/account/profile/` and `/admin/`.

## [ui]

Chooses the stylesheet and the behaviour script every page loads, and sets
design tokens. See [styling](/docs/styling/).

## [auth]

Accounts (`auth` feature); every key is optional. See [the admin](/docs/admin/).

| Key | Default | Meaning |
| --- | --- | --- |
| `session_hours` | 12 | Sessions end this long after sign-in |
| `idle_minutes` | 120 | ... or after this long without a request |
| `require_totp_for_admins` | false | Administrators must turn on two-factor sign-in before using the admin |
| `public_url` | `[site] base_url` | Base of links in messages (password reset); `ENGINE_PUBLIC_URL` overrides `base_url` too |
| `behind_proxy` | false | Take the client address for rate limits from `X-Forwarded-For` (only behind Caddy or nginx) |
| `cookie_secure` | true | `Secure` cookies with the `__Host-` prefix. Leave on: browsers and curl accept them on `http://localhost` |
| `max_failures_per_account` | 5 | Failed sign-ins per account per 15 minutes |
| `max_failures_per_address` | 30 | Failed sign-ins per client address per 15 minutes |
| `secret_key_file` | none | A file holding the site key that encrypts two-factor secrets; `ENGINE_SECRET_KEY` wins |
| `registration` | `"closed"` | `"open"` lets visitors create accounts at `/account/register/` (needs `roles`) |
| `default_roles` | `[]` | Roles of accounts made by registration (never `admin`) |
| `max_registrations_per_address` | 5 | Accounts made from one client address per hour |

## [cache]

The fragment cache of `request` pages; see
[the render dial](/docs/render-dial/#the-fragment-cache).

| Key | Default | Meaning |
| --- | --- | --- |
| `fragments` | true | Cache `fragment()` output in `request` renders |
| `fragment_ttl_seconds` | 600 | Longest a fragment is kept, even without changes |
| `max_mb` | 64 | Memory budget |

## [jobs] and [mail]

Background jobs (`jobs`) and email (`mail`); see
[jobs and mail](/docs/jobs-and-mail/). Forms are files in `forms/`, not
`site.toml`; see [forms](/docs/forms/).

## [database] and [publish]

Used by `cms` builds; see [content types](/docs/content-types/#the-database)
and [publishing](/docs/publishing/#publish-targets).

| Key | Meaning |
| --- | --- |
| `[database] url` | `postgres://...`; `ENGINE_DATABASE_URL` overrides it |
| `[database] max_connections` | Pool size (default 5); `ENGINE_DATABASE_MAX_CONNECTIONS` overrides it |
| `[publish] target` | `disk` (default). `s3` is planned |

A build without a feature parses its section anyway, so a site written for
a fuller build still loads, and warns that the section is ignored.
