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
workers = 4                                     # optional: sized from the machine

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
| `verify_email` | true | Registered accounts follow an emailed link before they can sign in |

## [cache]

The fragment cache of `request` pages; see
[the render dial](/docs/render-dial/#the-fragment-cache).

| Key | Default | Meaning |
| --- | --- | --- |
| `fragments` | true | Cache `fragment()` output in `request` renders |
| `fragment_ttl_seconds` | 600 | Longest a fragment is kept, even without changes |
| `max_mb` | sized from the machine | Memory budget in MiB; unset: 1/64 of memory, 4 to 256 MiB ([limits](#limits)) |

## [jobs] and [mail]

Background jobs (`jobs`) and email (`mail`), PostgreSQL only; see
[jobs and mail](/docs/jobs-and-mail/). Forms are files in `forms/`, not
`site.toml`; see [forms](/docs/forms/).

## [media], [search] and [api]

Uploads ([media](/docs/media/): `dir`, `max_mb`, `max_megapixels`,
`widths`, `jpeg_quality`), full-text search ([search](/docs/search/):
`language`, `path`, `exclude`, and `pagefind = true` for static sites) and
the JSON API ([api](/docs/api/): `requests_per_minute`,
`bad_tokens_per_minute`, `behind_proxy`).

## [payments]

The shop (`payments` feature, PostgreSQL only): `provider` (`stripe` or
`fake`), `currency`, `product_type` or `products_file`, `success_path`,
`cancel_path`, `webhook_tolerance_seconds`. Keys come from the environment
only; see [payments](/docs/payments/).

## [database] and [publish]

Used by `cms` builds; see [content types](/docs/content-types/#the-database)
and [publishing](/docs/publishing/#publish-targets).

| Key | Meaning |
| --- | --- |
| `[database] url` | `sqlite://data/site.db` (relative to the site folder) or `postgres://...`; `ENGINE_DATABASE_URL` overrides it. See [databases](/docs/databases/) |
| `[database] max_connections` | Pool size, also the number of request renders at once (unset: CPUs + 4, 4 to 32; see [limits](#limits)); `ENGINE_DATABASE_MAX_CONNECTIONS` overrides it |
| `[publish] target` | `disk` (default) or `s3` (the `s3` feature): `bucket`, `prefix`, `endpoint`, `region`, `credentials` and `[publish.cdn]`, see [publishing to a bucket](/docs/buckets/). Keys never go here |

A build without a feature parses its section anyway, so a site written for
a fuller build still loads, and warns that the section is ignored.

## [limits]

Threads, the database pool, caches and background work are sized from what
the engine may use: the CPUs it may run on and its memory, honouring a
container's CPU quota and memory limit. There is no separate "small" mode:
a 1-CPU, 512 MiB container and a 64-CPU server run the same formulas, so a
large machine uses its capacity and a small one is not overrun. `engine
doctor` prints every value, how it was computed and whether it was set
here.

| Limit | Set with | Unset | 1 CPU, 512 MiB | 4 CPUs, 8 GiB | 64 CPUs, 256 GiB |
| --- | --- | --- | --- | --- | --- |
| Async worker threads | `[limits] worker_threads` | one per CPU | 1 | 4 | 64 |
| Blocking threads (file reads, renders) | `[limits] blocking_threads` | 16 per CPU, 32 to 512, at most one per 4 MiB | 32 | 64 | 512 |
| Database connections = renders at once | `[database] max_connections` | CPUs + 4, 4 to 32, at most one per 64 MiB | 5 | 8 | 32 |
| Job workers | `[jobs] workers` | one per CPU, 2 to 16 | 2 | 4 | 16 |
| Image variants at once | `[limits] background_threads` | one per CPU and per 256 MiB, at least 1 | 1 | 4 | 64 |
| Rendered value cache | `[limits] value_cache_mb` (0: off) | 1/256 of memory, 1 to 64 MiB | 2 MiB | 32 MiB | 64 MiB |
| Fragment cache | `[cache] max_mb` | 1/64 of memory, 4 to 256 MiB | 8 MiB | 128 MiB | 256 MiB |
| File ETag cache | `[limits] etag_cache_entries` | 1/1024 of memory at ~200 bytes each, at least 4096 | 4,096 | 41,943 | 1,342,177 |

```toml
[limits]
blocking_threads = 64     # any of the keys above; the rest stay automatic
```

Heavy background work (image variants, compressing the files of a bulk
publish) runs on threads of its own at a lower scheduling priority (nice
10): on an idle machine it still gets every CPU, and when requests arrive
the kernel serves them first. The allocator's arenas (eight per CPU the
process may use) are set before any thread starts; glibc's
`MALLOC_ARENA_MAX` is not needed.
