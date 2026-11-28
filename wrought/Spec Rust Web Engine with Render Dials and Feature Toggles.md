# Spec: Rust Web Engine with Render Dials and Feature Toggles

One open source Rust engine serves both static sites and full web apps. Each route chooses when it renders, and each capability is a feature you switch on at build time. A site with nothing switched on is a static site generator; switch on accounts, admin and jobs and it becomes a web app. Working name: Wrought ("a well-wrought web"), after wrought iron: hand-worked, repairable and built to last for decades. Its companion repo is Bloomery, named after the furnace where iron is first made: the OpenTofu and Ansible templates that build the infrastructure Wrought runs in.

## Goals

- **One stack for everything:** content sites, documentation, shops and dashboards share one core, one data layer and one template system.
- **HTML first:** server-rendered semantic HTML; JavaScript only as small modules or htmx where a page needs it.
- **Pay only for what you use:** features are Cargo features, so code that's switched off isn't compiled, shipped or attackable.
- **Fast reads by default:** published pages are files on disk served by any web server; dynamic routes render in milliseconds.
- **Community owned:** permissive licence, open governance, no required cloud service or vendor.
- **Runs anywhere:** a single binary, plus PostgreSQL for the database features; fine on a Raspberry Pi or a Proxmox VM.

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
| `db-postgres` | Data layer (PostgreSQL), migrations | — |
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
- **Data:** `sqlx`, PostgreSQL only: one SQL dialect, one code path. Static sites need no database at all.
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
    engine-db/        # sqlx pool (PostgreSQL), migrations
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
5. **v0.5, polish:** `media`, `search`, `api`, SQLite parity (SQLite was removed again in v0.8), benchmarks published.
6. **v0.6, completion:** full component library, starter themes, S3 publish target, `oidc`, `payments` (provider trait), Postgres query optimisation, CI with accessibility checks, community files.
   - [ ] `engine new` with every preset and theme builds and passes TagCheck
7. **v0.7, visual editor:** `visual-editor` as specified above.
   - [x] An editor rearranges blocks in the preview and publishes without touching a template
8. **v0.8, framework:** what a single developer or small agency gets from Rails or Django, so Wrought can replace them for custom apps.
   - **App crate API:** a site can be its own Rust crate that registers handlers, queries, jobs, hooks and CLI tasks on the engine; compile-time crates are the plugin model.
   - **Generators:** `engine generate` scaffolds types, routes, handlers, forms, jobs and mailers.
   - **One database:** `jobs`, `mail` and real-time run on the site's PostgreSQL, so an install needs no Redis or second store. (This item said "SQLite everywhere" until the decision below.)
   - **Real-time:** server-sent events (and WebSockets where needed) with htmx, driven by the same change feed and cache tags as publish.
   - **Developer experience:** rich dev error pages, query log with N+1 detection, `engine routes`, `engine doctor`, `engine console` (query and content shell), mail previews in the browser, seeds, fixtures, a test client and factories.
   - **Small host:** a Raspberry Pi class machine (4 to 8 GB RAM, NVMe storage) serves several sites without noticeable latency. Decided 2026-11-26.
     - **Disk is first class.** Published files on fast local storage remain the main serving strategy for the whole stack. NVMe is assumed (a Pi takes an NVMe HAT); the design is not bent to suit SD cards.
     - **Pre-compressed files:** every published text file is written with gzip and Brotli copies beside it, in the same atomic swap. The front door serves the compressed copy first. An edit recreates the copies for exactly the files it changes; nothing is compressed per request.
     - **One database engine: PostgreSQL, with no-setup installs kept.** Direction set 2026-11-26; decided and done 2026-11-28 (SQLite removed): every query is written, tested and tuned once, for PostgreSQL. The split is by flavour: the `static` preset is built from files and needs no database at all, so it is the no-setup install; the `cms` and `app` presets come with PostgreSQL, set up for the user either in Docker (a compose file written by `engine new`, started with one `docker compose up`) or pointed at a dedicated server, and growth is a change of connection address with no change of SQL dialect. SQLite support is removed; a `site.toml` that still names a SQLite database is refused with a message saying what to do. `engine new --preset cms|app` writes the site's own `compose.yml` (PostgreSQL in Docker, local port, generated password) and points `[database] url` at it. A site can start as `static` and add PostgreSQL later without rewriting content. An embedded PostgreSQL (PGlite) was evaluated as a no-setup option for `cms` and not adopted (DECISIONS.md, "PGlite evaluation"). On a small host, one PostgreSQL server is shared by all sites (a database per site), sized by the machine-scaled limits.
     - **Start on demand:** the engine can be started by the first request that needs it (socket activation) and exit after a quiet period; published pages are served with the engine stopped. Cold start target: under 200 ms. For `cms` sites the engine's own PostgreSQL sleeps with it: it is started when the engine wakes (an editor logs in, a form is submitted, a scheduled publish is due) and shut down cleanly after the quiet period, so an idle site uses no database memory. Public form posts always go straight to the database when it is up, with no extra disk write on that path. Only when the database is asleep or unreachable does a fallback handler catch the post: it is saved durably to a spool on disk, acknowledged to the visitor at once, and written to the database in order, exactly once, as soon as the database is up; the same handler covers a database outage. `app` sites and shared database servers stay running by default. The idle watchdog is a host-level setting: the hosting admin turns it on or off and sets the quiet period, for the whole host and per site, separately for the engine and for its PostgreSQL; a site owner cannot override the host's choice. The wake-up time is measured and published.
     - **The rule:** never trade performance for resource saving or to fit smaller hardware; always use the resource-efficient and fast method. The same binary must suit a hosting company's fleet, a large agency's servers, a NUC, a consumer PC and a single-board computer.
     - **Limits scaled to the machine:** worker threads, connection pools, caches and queues are capped in bytes and sized automatically from the resources actually available (CPU count and memory, honouring container limits), so a large server uses its capacity and a small one is not overrun. There is no separate "small" mode to choose; every limit can be overridden in configuration, and `engine doctor` prints the values chosen and why. Heavy background work (images, bulk publish) is throttled so it cannot starve serving.
     - **Fast encryption:** the shipped front-door configuration uses TLS 1.3, elliptic-curve certificates, modern AEAD cipher suites (AES-GCM where the CPU accelerates it, ChaCha20-Poly1305 where it does not), session resumption and HTTP/3.
     - **Browser caching without manual purges:** a page refresh is always enough to see an update.
       - Assets (CSS, JS, fonts, images, media variants) are served from content-hashed URLs with `Cache-Control: public, max-age=31536000, immutable`; changing a file changes its URL, and publish rewrites the references.
       - HTML and other un-hashed documents (feeds, sitemaps) are served `Cache-Control: no-cache` with a strong content-derived `ETag`, so browsers revalidate every time and get a `304` when nothing changed. Each compressed encoding has its own `ETag`, with `Vary: Accept-Encoding`.
       - Request-rendered pages derive their `ETag` from the versions of the cache tags they depend on, so revalidation is answered without rendering; personalised responses are `private`.
       - Shared caches and CDNs receive the page's cache tags as surrogate keys (`Cache-Tag` / `Surrogate-Key`) and are purged by tag on publish, with a short `s-maxage` plus `stale-while-revalidate` as the fallback where tag purge is unavailable.
       - No service worker caches pages by default.
     - [x] After any edit, one refresh in a browser that had the page and its assets cached shows the new content, with unchanged assets served from cache and no manual purge (`scripts/check-caching.sh`, through `engine serve` and Caddy; surrogate keys and CDN purge by tag still to do)
     - **Disk to memory:** rely on the operating system's page cache first; measure before adding anything. Add engine-side work (zero-copy file sends, read-ahead of hot files after publish) only where a benchmark on NVMe shows a gain.
     - **ARM builds:** `aarch64` binaries are released with the others and run the full test suite.
     - [ ] Ten sites on one Raspberry Pi 5 with NVMe: memory, p50/p99 response time and power at the wall are measured on real hardware and published
     - [ ] Ten sites sharing one PostgreSQL server on a small host: total database memory is measured and published
   - [ ] A custom app with its own handler, job and mailer is scaffolded, tested and running on PostgreSQL alone (no Redis, no second store)
9. **v0.9, agency:** the rest of the Rails and Django toolkit, plus first-class payment gateways.
   - **Internationalisation:** translations, locale-aware dates and numbers, time zones, translated content and URLs.
   - **Model lifecycle:** validations, hooks/signals, data migrations, rollback, outbound webhooks, audit log.
   - **Contrib set:** RSS/Atom feeds, redirects manager, flash messages, admin list filters, bulk actions and inline editing, CSV import/export, object-level permissions.
   - **Agency operations:** multi-tenancy (many client sites from one install), encrypted credentials, feature flags, health-check endpoint, structured logs and error-reporting hooks, OpenAPI docs for the API.
   - **Inbound email:** route received mail to handlers.
   - **Payment gateways:** first-class Stripe (including ACH) and PayPal integrations behind the `payments` provider trait, for orders, donations, subscriptions and ticketing.
   - [ ] One install serves two client sites in two languages, each taking a payment through a different gateway
10. **v1.0, release:** everything through v0.9 finished, stable and documented. No new features.
    - The rename (engine: Hammerwright; serving stack: Ingotyard), crates and signed binaries published for x86-64 and `aarch64`.
    - A stability promise: semantic versioning, a documented upgrade path between releases, and database migrations that run forward without manual steps.
    - Benchmarks re-measured on the released binary and published with their method.
    - [ ] A new user installs from the published release and has a `cms` site live by following the docs alone
    - **Known performance gaps carried past v1.0.** Measured on the benchmark machine with isolated cores (DECISIONS.md, "Known performance gaps carried past v1.0"). The owner decided on 2026-11-27 that there are no further tuning passes before v1.0; the speed guard (`scripts/check-speed.sh`, CI) protects the current numbers from regressions.
      - Article rendered per request: about 1,360 to 1,670 requests a second, against Loco's 1,801.
      - Listing: about 1,630 a second, against Loco's 2,778.
      - Search: about 1,115 a second, against Loco's 2,202. Loco and Phoenix do no ranking, no snippets and no result count.
      - Building 1,000 Markdown articles on one core: 1.14 s with the pre-compressed copies, 0.66 s without, against Hugo's 0.78 s and Zola's 0.64 s.
      - Unexplained: the search-index phase of a build creeping from 0.74 s to 1.25 s over 25 builds, seen in the benchmark stack only; and a 19 % difference in the article figure between two otherwise comparable runs.

### After v1.0: closing the gap to enterprise systems

Decided 2026-11-27: v1.0 ships first; the milestones below follow it. They list what enterprise platforms (Adobe Experience Manager, Sitecore, Drupal with a CDN, WordPress VIP, headless CMS plus a front-end host) provide that Wrought and Ingotyard do not yet. The order is by how much each gap blocks a growing customer; it can change with demand. The small-host rule still applies to all of it: nothing here may make a single small install heavier or slower, so each item is a feature toggle or a separate Ingotyard component.

11. **v1.1, scale-out and high availability.** Gap: one engine process and one database per site; a failed machine means downtime.
    - **Several engine nodes:** any number of engine processes behind a load balancer, with sessions, jobs, locks and the fragment cache held in PostgreSQL or shared storage, never in one process's memory.
    - **One publisher at a time:** a leader lock so exactly one node publishes; every node serves. Published output goes to shared or replicated storage (object storage, or a replicated folder) with the same atomic swap.
    - **Database resilience:** streaming replication with automatic failover, read replicas for request-rendered pages, and connection pooling (PgBouncer or equivalent) in the shipped Ingotyard configuration.
    - **Zero-downtime deploys:** rolling restarts and migrations that old and new binaries can both run against.
    - **Backup and restore:** scheduled backups with point-in-time recovery for the database, media and published output, and a restore that is tested automatically.
    - **Multi-region delivery:** published files replicated to more than one region behind a CDN, with tag purge reaching all of them.
    - [ ] Under load, one engine node and then the primary database are killed; no published page fails, and editing resumes without manual steps
    - [ ] A restore from backup to an empty machine is run in CI and produces the same published output
12. **v1.2, editorial at scale.** Gap: the workflow is draft, review, published for one item at a time.
    - **Configurable approvals:** multi-step workflows per content type and section, with assignments, due dates, comments and notifications.
    - **Releases:** a set of changes across many items scheduled, previewed as a whole site, published together and rolled back together; scheduled unpublish and embargo.
    - **Translation workflow:** on top of v0.9 internationalisation: per-locale status, side-by-side translation, fallbacks, export and import in XLIFF for translation vendors.
    - **Asset library:** a full digital asset manager: metadata, rights and expiry dates, usage tracking ("where is this used"), focal points, renditions, bulk upload, folders and search.
    - **Content reuse across sites:** shared content and components between sites in one install, with local overrides.
    - **Environments:** content and configuration moved between development, staging and production in a controlled way.
    - **Quality checks before publish:** broken links, missing alt text, accessibility and SEO checks as gates in the workflow.
    - [ ] A twenty-page campaign in two languages is approved by two roles, previewed whole, published at a set time and rolled back in one action
13. **v1.3, identity, governance and compliance.** Gap: OIDC sign-in and an audit log exist (v0.6, v0.9); enterprise buyers ask for much more on paper and in the product.
    - **Enterprise sign-in:** SAML alongside OIDC, SCIM user provisioning, group-to-role mapping, enforced 2FA and session policies.
    - **Fine-grained permissions:** roles scoped per site, section, locale and workflow step.
    - **Audit and retention:** an append-only audit log that can be exported to a SIEM; retention rules, legal hold, and content archiving.
    - **Privacy tooling:** export and erasure of a person's data, consent records, data-residency controls.
    - **Data protection:** encryption at rest for database, media and backups; secrets from a key-management service or vault.
    - **Supply chain:** a software bill of materials and reproducible builds with every release.
    - **Evidence:** an independent penetration test, a published accessibility conformance report for the admin and the default themes, and the controls a hosted offering needs for SOC 2 and ISO 27001 (organisational work, tracked here so it is not forgotten).
    - [ ] A security questionnaire from a large customer can be answered from published documents
14. **v1.4, personalisation, experiments and insight.** Gap: pre-built files are the same for every visitor.
    - **Segments on pre-built pages:** variants built ahead per segment and chosen at the front door or CDN edge, plus small request-rendered fragments inside a published page, so personalisation does not turn every page into a request-rendered one.
    - **Experiments:** A/B and multivariate tests with variant assignment, results and a clean way to end a test.
    - **Analytics:** first-party, privacy-respecting page and conversion statistics in the admin, with no third-party script needed.
    - **Search at scale:** facets, synonyms, typo tolerance and relevance tuning; an optional external search engine behind the same interface.
    - **Marketing connections:** CRM, email-marketing and consent-platform integrations through the app crate API.
    - [ ] A page shows two variants to two segments while its unpersonalised parts are still served as files
15. **v1.5, hosting platform (Ingotyard).** Gap: Ingotyard is a serving stack an administrator sets up by hand; hosting companies need a product they can run a fleet on. This list is drawn from comparison with commercial platforms and must be checked against the Ingotyard repository before work starts.
    - **Control plane:** an API and panel to create, clone, suspend, move and delete sites; custom domains and certificates automated at scale.
    - **Tenant isolation:** per-site resource limits and quotas (CPU, memory, disk, bandwidth) enforced by the host, so one site cannot starve the rest.
    - **Metering and billing hooks:** usage per site exported for a host's billing system; white-label branding.
    - **Edge protection:** web application firewall rules, rate limiting, bot and denial-of-service controls in the shipped front door.
    - **CDN integration:** tag purge wired to several CDN providers, not only one.
    - **Observability:** metrics, traces and logs in OpenTelemetry form, shipped dashboards and alerts, per-site health and uptime.
    - **Fleet updates:** staged rollouts of engine and stack updates with automatic rollback.
    - **Environments for customers:** one-step staging copies and push to live.
    - **Deployment targets:** a Kubernetes operator or Helm chart and Terraform modules beside the Docker and dedicated-server setups.
    - [ ] A host creates one hundred sites through the API, updates them all in a staged rollout, and bills from the exported usage
16. **v1.6, ecosystem and support.** Gap: enterprises buy the surrounding assurances as much as the software.
    - **Migration tools:** importers for WordPress and Drupal content, media, users and redirects, since moving an existing site is the main adoption cost.
    - **Extension registry:** a public index of themes, components and app crates with versioning and security review. Open question: whether runtime extensions (for example WebAssembly) are needed alongside compile-time crates for hosts that cannot recompile per site.
    - **Release policy:** long-term-support releases, security backports, and a written deprecation policy.
    - **Commercial backing:** paid support with response-time commitments, a partner programme for agencies and hosts, and training material.
    - [ ] An existing WordPress site with a thousand posts is imported, published and passes a link check without hand editing
17. **Later, more gateways:** Givebutter, Anedot and Donorbox (embed plus sync), then GoCardless, Square, Braintree and Authorize.net, as set out under "Payments and donations".

## Payments and donations

Decided 2026-11-26. Wrought handles both commerce (orders) and donations, as two record types behind the `payments` provider trait.

| Gateway | Integration | Notes |
| --- | --- | --- |
| Stripe | Full provider: checkout, refunds, subscriptions, ACH Direct Debit | Orders and donations |
| PayPal | Full provider: orders, refunds, subscriptions | Orders and donations |
| Givebutter | Their widget or hosted page, webhooks in, API to reconcile and to push contacts and offline gifts | Donations only; US and USD only; cannot charge from our server |
| Anedot | Their link or iframe, webhooks in | Donations only; US only; no public REST API |
| GoCardless | Full provider: bank debit (ACH, SEPA, Bacs), recurring mandates | Orders and donations |
| Square | Full provider | Orders and donations |
| Braintree | Full provider: cards, Venmo, ACH | Orders and donations |
| Authorize.net | Full provider: cards, eCheck (ACH) | Orders and donations |
| Donorbox | Their embed or hosted page, webhooks in, API to reconcile | Donations only |

The integration column for GoCardless, Square, Braintree, Authorize.net and Donorbox is the intended shape, to be confirmed against each provider's current documentation before it is built; a provider that cannot charge from our server falls back to embed plus sync. Bank debits settle over days and can be returned, so orders and donations have pending and returned states.

- **Accounts:** one API key per client site; no platform accounts (Stripe Connect, PayPal partner).
- **Recurring:** Wrought manages subscriptions on Stripe and PayPal (create, pause, cancel); it mirrors recurring plans from Givebutter and Anedot.
- **Tax receipts:** left to the payment platforms.
- **Campaign finance:** donations carry the compliance fields Anedot uses (employer, occupation and the rest of its payload), stored and available on Wrought's own donation forms too.
- **Webhook trust:** Givebutter webhooks are confirmed by fetching the transaction back from its API; Anedot's HMAC is accepted and de-duplicated on request ID. Donations can arrive with no order created first.
- **Ticketing:** events and tickets are in scope, sold through Stripe and PayPal and mirrored from Givebutter and Anedot.
- **Third-party scripts:** a provider embed (such as Givebutter's widget) is the one allowed exception to serving everything from the site's own origin, with a matching content-security-policy entry added only on pages that use it.

## Community and open source plan

- **Licence:** dual MIT / Apache-2.0, the Rust ecosystem norm, so anyone can use it, including commercially.
- **Home:** a public repository at github.com/kovirlabs/wrought.
- **Governance:** start with a simple BDFL model and a CONTRIBUTING.md; move to a small maintainer team once there are regular contributors.
- **Onboarding:** "good first issue" labels per crate, an architecture doc, and the three example sites.
- **Showing it off:** the docs site built with the engine, a public benchmark against WordPress, Drupal and Astro, and short write-ups on This Week in Rust, Lobsters and the Fediverse.
- **Branding:** under the Kovir Labs umbrella.

## Open questions

- **Name:** confirm \`wrought\` (or \`bloomery\`) is free on crates.io, then claim it with a placeholder 0.0.1 release.
- **Default templates:** minijinja or askama as the default?
- **Content in files vs database:** should `build` mode read Markdown files, the database, or both from day one?
- **Plugin model:** compile-time crates only, or also runtime plugins (WebAssembly) later?
- **First real site:** which of your sites becomes the reference user?
