# Spec: Rust Web Engine with Render Dials and Feature Toggles

One open source Rust engine serves both static sites and full web apps. Each route chooses when it renders, and each capability is a feature you switch on at build time. A site with nothing switched on is a static site generator; switch on accounts, admin and jobs and it becomes a web app. Working name: Wrought ("a well-wrought web"), after wrought iron: hand-worked, repairable and built to last for decades. Its companion repo is Bloomery, named after the furnace where iron is first made: the OpenTofu and Ansible templates that build the infrastructure Wrought runs in.

## Goals

- **One stack for everything:** content sites, documentation, shops and dashboards share one core, one data layer and one template system.
- **HTML first:** server-rendered semantic HTML; JavaScript only as small modules or htmx where a page needs it.
- **Pay only for what you use:** features are Cargo features, so code that's switched off isn't compiled, shipped or attackable.
- **Fast reads by default:** published pages are files on disk served by any web server; dynamic routes render in milliseconds.
- **Community owned:** permissive licence, open governance, no required cloud service or vendor.
- **Runs anywhere:** a single binary plus Postgres or SQLite; fine on a Raspberry Pi or a Proxmox VM.

Non-goals (for now): a JavaScript front-end framework, a freeform visual page builder (the visual editor below is block-based), multi-region clustering.

## The render dial

Every route picks one of three render modes. The mix decides whether the site acts static, hybrid or app-like.

| Mode | When HTML is made | Stored | Typical use |
| --- | --- | --- | --- |
| `build` | At `engine build` | Files on disk | Docs, marketing pages, blogs with no CMS |
| `publish` | When content changes (Postgres NOTIFY or file watch) | Files on disk, swapped atomically | CMS-driven sites, catalogues, news |
| `request` | On every request | Fragment cache in memory | Accounts, carts, dashboards, search, anything personal |

Rules:

- A route may fall back from `publish` to `request` when its file is missing; the render then writes the file.
- Any request with a session cookie or a query string runs in `request` mode unless the route opts out.
- Every render records cache tags (`i:<type>:<id>`, `c:<type>`, `s:<singleton>`). `publish` uses them to know which files to rewrite; `request` uses them to evict fragments.

Publish target: `build` and `publish` output goes to local disk by default, or to an S3-compatible bucket (`[publish] target = "s3"`) served by a CDN, with CDN invalidation of changed paths after each publish. See the deployment spec for the cloud setup.

## Feature toggles

Features are Cargo features, and each one adds routes, tables and admin screens only when it's on.

| Feature | Adds | Needs |
| --- | --- | --- |
| `core` (always on) | Router, templates, static files, build mode, config | — |
| `content` | Content types, blocks, taxonomy, revisions, slugs | `db` |
| `db-sqlite` / `db-postgres` | Data layer, migrations | — |
| `publish` | Publish mode, tag index, NOTIFY listener, atomic writes | `content` |
| `auth` | Accounts, sessions, argon2 passwords, password reset | `db` |
| `roles` | Roles, permissions, per-field access | `auth` |
| `admin` | Server-rendered admin UI for content and users | `auth`, `content` |
| `workflow` | Draft → review → published, preview, notifications | `admin` |
| `forms` | Form builder, validation, submissions, spam guard | `db` |
| `jobs` | Background jobs and schedules on Postgres (`SKIP LOCKED`) | `db-postgres` |
| `mail` | SMTP sending with templates | `jobs` |
| `media` | Uploads, image resizing to disk | `db` |
| `search` | Postgres full-text search, or Pagefind index at build | `content` |
| `api` | JSON API for content types, tokens | `auth` |
| `payments` | Checkout and webhooks through a pluggable provider | `auth`, `jobs` |
| `oidc` | Single sign-on via OpenID Connect | `auth` |

Presets bundle common sets: `static` (core only), `cms` (content, publish, admin, workflow, media), `app` (cms plus auth, roles, forms, jobs, mail, api).

## Architecture

- **Language and runtime:** Rust stable, `tokio`, `axum` for HTTP.
- **Data:** `sqlx` with compile-time checked queries; Postgres for multi-user editing, SQLite for single-editor or static sites.
- **Templates:** `minijinja` by default (editable without recompiling); `askama` optional for compiled speed.
- **Cache:** `moka` in-memory fragment cache with tag-based eviction.
- **Front door:** Caddy or nginx serves the output folder and passes misses and dynamic routes to the engine. A built-in static file server covers dev and small installs.
- **Interactivity:** htmx and plain ES modules; islands of any JS library are allowed but never required.
- **Concurrency safety:** optimistic locking with a `version` column on every editable row; saves run in one transaction with their revision row.

Request flow: web server → file on disk (hit) → otherwise engine → route handler → data loaders (record tags) → template → response, written to disk if the route is in `publish` mode.

## Visual editor

An optional TypeScript app gives editors visual, in-place editing. Developers define which blocks exist; editors arrange and fill them. The editor never owns the HTML: it saves a block tree, and Wrought renders it.

- **Feature:** `visual-editor`, which needs `admin` and `content`.
- **What it edits:** a page is an ordered tree of typed blocks (hero, text, image, product grid, and so on), saved as JSON through the admin API with the usual version check.
- **Preview:** an iframe rendered by Wrought itself from draft data, so what editors see is exactly what ships.
- **Interaction:** click a block in the preview to edit its fields in a side panel; edit text in place; drag to reorder; add blocks from a library.
- **Design guardrails:** styling options come only from the site's design tokens (colours, fonts, spacing), so editors can't break the design.
- **Tech:** TypeScript with Svelte or Lit for the shell, Tiptap (ProseMirror) for rich text.
- **Packaging:** built at release time and embedded in the Rust binary as static assets; running a site never needs Node.
- **Blocks:** each block type = a type file (fields) + a template (markup) + optional editor hints (icon, preview thumbnail, field layout).
- **Rendering modes:** unaffected; publish and build modes render block trees like any other content.

## Built-in component library

Wrought ships a library of ready-made components inside the binary, so most sites and apps need no developer. Each component is a block type plus a template plus optional behaviour, usable from templates and from the visual editor.

| Group | Components | Needs feature |
| --- | --- | --- |
| Layout | Section, columns, grid, stack, container, spacer, divider | `core` |
| Navigation | Header, menu, mobile menu, breadcrumbs, footer, pagination, tabs | `core` |
| Content | Hero, rich text, image, gallery, video embed, quote, card, accordion, call to action, timeline | `core` |
| Listings | Content list, filtered list, tag cloud, related items, search box and results | `content`, `search` |
| Forms | Contact form, newsletter signup, multi-step form, file upload | `forms` |
| Accounts | Login, register, password reset, profile, account menu | `auth` |
| App UI | Data table (sort, filter, paginate), stat card, chart, alerts, modal, toast, empty state | `request` routes |
| Commerce | Product card, product grid, cart, checkout summary, order history | `payments` |

Design rules:

- **HTML first:** every component renders complete, semantic HTML on the server and works without JavaScript.
- **Progressive enhancement:** interactive parts (tabs, modal, data table sorting, mobile menu) are small, dependency-free custom elements (web components) that upgrade the server HTML.
- **Theming:** styling comes from design tokens as CSS custom properties; a site sets colours, fonts, radius and spacing once, and every component follows.
- **Overridable:** a site can override any component by placing a template with the same name in its own `templates/components/`; the built-in one is the fallback.
- **Accessible:** target WCAG 2.2 AA, with keyboard support, focus handling and ARIA patterns, tested in CI with TagCheck (axe-core as a second opinion).
- **Lean:** only components for features compiled in are included; JavaScript for a component loads only on pages that use it.
- **Versioned:** components follow semantic versioning; breaking markup changes only in major releases, with an upgrade note.

### Default styling and behaviour: the Tag stack

Unless a site needs something custom, components use the Kovir Labs Tag stack, so plain semantic HTML looks and behaves right with no classes and no framework.

| Layer | Default | Role in Wrought |
| --- | --- | --- |
| Styling | TagBrush | Classless CSS for semantic tags; Wrought's design tokens map onto TagBrush's custom properties |
| Behaviour | TagAction | Expected behaviour for tags (dialog, details, table sort, forms, password toggle, code copy, time, nav, heading anchors and more), replacing most bespoke component scripts |
| Checks | TagCheck | Semantic HTML checker with ADA checks for contrast and visibility; runs on every render in \`engine dev\`, as \`engine check\` over the built site, in CI, and in the visual editor to warn editors before they publish |

- **Components lean on tags:** templates emit semantic elements (`dialog`, `details`, `table`, `nav`, `form`) and let TagBrush and TagAction do the work; custom elements are only for what the Tag stack doesn't cover.
- **Bundled:** pinned versions of TagBrush and TagAction ship inside the binary and are served from the site's own origin, with no CDN required.
- **Swappable:** styling and behaviour are providers set in `site.toml` (`[ui] css = "tagbrush"`, `behaviour = "tagaction"`); a site can switch to another classless library, its own CSS, or none.
- **Upstream first:** fixes found while building Wrought components go back to the Tag repos, so both projects improve together.

Starter themes: two or three complete themes (documentation, business site, app dashboard) built only from these components, shipped with `engine new`.

## Project layout

A Cargo workspace keeps each feature in its own crate, so contributors can work on one area without touching the core.

```
engine/
  crates/
    engine-core/      # router, config, render modes, tags, templates
    engine-db/        # sqlx pool, migrations, sqlite + postgres
    engine-content/   # content types, blocks, taxonomy, revisions
    engine-publish/   # NOTIFY listener, tag index, atomic writer
    engine-auth/      # accounts, sessions, roles
    engine-admin/     # admin UI (minijinja + htmx)
    engine-forms/ engine-jobs/ engine-mail/ engine-media/ engine-search/ engine-api/
    engine-cli/       # the `engine` binary
  examples/
    static-blog/  cms-site/  small-app/
  docs/           # built with the engine itself, build mode
```

## Site configuration

A site is a folder with `site.toml`, `templates/`, `content/` (for file-based content) and `migrations/`.

```toml
[site]
name = "Example"
base_url = "https://example.org"
output = "public"

[database]
url = "postgres://engine@localhost/example"

[render]
default = "publish"

[[routes]]
path = "/"
template = "home.html"
mode = "publish"

[[routes]]
path = "/account/*"
mode = "request"
requires = "auth"
```

Content types are declared in `types/*.toml` (fields, relations, validation). The engine generates migrations and admin forms from them, the way Drupal's content types worked.

## Command line

- `engine new <name> --preset static|cms|app` creates a site from an example.
- `engine dev` runs with live reload of templates and content.
- `engine build` renders every `build` and `publish` route to the output folder.
- `engine serve` runs the server, the publish listener and jobs.
- `engine migrate` applies schema changes; `engine types sync` generates migrations from type files.
- `engine rebuild [--tag <tag>]` re-renders everything, or only pages with a tag.

## Security baseline

- Auto-escaping templates; CSRF tokens on every form; secure, HttpOnly, SameSite session cookies.
- Argon2id passwords, login rate limits, optional TOTP 2FA for admin roles.
- Features not compiled in have no routes, so nothing to probe.
- `cargo audit` and `cargo deny` in CI; signed release binaries; a SECURITY.md with a private reporting address.

## Prototype roadmap

1. **v0.1, static:** `core`, build mode, minijinja templates, Markdown content, `engine new/dev/build`.
   - [ ] Builds the project's own docs site
2. **v0.2, publish:** `db-postgres`, `content`, `publish` with NOTIFY and tag index.
   - [ ] An edit rewrites only the affected pages
3. **v0.3, admin:** `auth`, `roles`, `admin`, revisions with optimistic locking.
   - [ ] Two editors saving the same item get a conflict, not an overwrite
4. **v0.4, app:** `request` mode with fragment cache, `forms`, `jobs`, `mail`.
   - [ ] The `small-app` example runs logins, a form and an emailed receipt
5. **v0.5, polish:** `media`, `search`, `api`, SQLite parity, benchmarks published.
6. **v0.6, completion:** full component library, starter themes, S3 publish target, `oidc`, `payments` (provider trait), Postgres query optimisation, CI with accessibility checks, community files.
   - [ ] `engine new` with every preset and theme builds and passes TagCheck
7. **v0.7, visual editor:** `visual-editor` as specified above.
   - [ ] An editor rearranges blocks in the preview and publishes without touching a template
8. **v0.8, framework:** what a single developer or small agency gets from Rails or Django, so Wrought can replace them for custom apps.
   - **App crate API:** a site can be its own Rust crate that registers handlers, queries, jobs, hooks and CLI tasks on the engine; compile-time crates are the plugin model.
   - **Generators:** `engine generate` scaffolds types, routes, handlers, forms, jobs and mailers.
   - **SQLite everywhere:** `jobs`, `mail` and real-time run on SQLite as well as Postgres, so a small install needs no Postgres or Redis.
   - **Real-time:** server-sent events (and WebSockets where needed) with htmx, driven by the same change feed and cache tags as publish.
   - **Developer experience:** rich dev error pages, query log with N+1 detection, `engine routes`, `engine doctor`, `engine console` (query and content shell), mail previews in the browser, seeds, fixtures, a test client and factories.
   - [ ] A custom app with its own handler, job and mailer is scaffolded, tested and running on SQLite alone
9. **v0.9, agency:** the rest of the Rails and Django toolkit, plus first-class payment gateways.
   - **Internationalisation:** translations, locale-aware dates and numbers, time zones, translated content and URLs.
   - **Model lifecycle:** validations, hooks/signals, data migrations, rollback, outbound webhooks, audit log.
   - **Contrib set:** RSS/Atom feeds, redirects manager, flash messages, admin list filters, bulk actions and inline editing, CSV import/export, object-level permissions.
   - **Agency operations:** multi-tenancy (many client sites from one install), encrypted credentials, feature flags, health-check endpoint, structured logs and error-reporting hooks, OpenAPI docs for the API.
   - **Inbound email:** route received mail to handlers.
   - **Payment gateways:** first-class Stripe, PayPal, Givebutter and Anedot integrations behind the `payments` provider trait.
   - [ ] One install serves two client sites in two languages, each taking a payment through a different gateway

## Community and open source plan

- **Licence:** dual MIT / Apache-2.0, the Rust ecosystem norm, so anyone can use it, including commercially.
- **Home:** primary repo on Codeberg (Forgejo, non-profit hosted) with a GitHub mirror for discoverability; your internal Forgejo stays the dev workspace.
- **Governance:** start with a simple BDFL model and a CONTRIBUTING.md; move to a small maintainer team once there are regular contributors.
- **Onboarding:** "good first issue" labels per crate, an architecture doc, and the three example sites.
- **Showing it off:** the docs site built with the engine, a public benchmark against WordPress, Drupal and Astro, and short write-ups on This Week in Rust, Lobsters and the Fediverse.
- **Branding:** under the Kovir Labs umbrella.

## Open questions

- **Name:** confirm \`wrought\` (or \`bloomery\`) is free on crates.io and Codeberg, then claim it with a placeholder 0.0.1 release.
- **Default templates:** minijinja or askama as the default?
- **Content in files vs database:** should `build` mode read Markdown files, the database, or both from day one?
- **Plugin model:** compile-time crates only, or also runtime plugins (WebAssembly) later?
- **First real site:** which of your sites becomes the reference user?
