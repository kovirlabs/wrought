# Changelog

All notable changes to Wrought are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (before 1.0, a minor version may
break things; breaking changes are called out). The reasons behind each
change are in [DECISIONS.md](DECISIONS.md).

## [Unreleased]

### Added
- `engine build --timings`: the time of each phase of a build.
- Resource benchmarks: whole-stack memory and CPU per 1,000 requests from
  cgroups, smallest workable memory limit, page weight in headless
  Chromium, 20 sites on one PostgreSQL, a density estimate; Hugo and Zola
  build targets; target groups (`--group core|extended|all`,
  `--targets`).
- `scripts/check-memory.sh` and the `memory-budget` CI job: idle memory of
  the `cms` preset on PostgreSQL must stay under 28 MiB RSS (measured
  23.8 MiB, 4.0 MiB of it heap).
- Benchmark targets Phoenix 1.8.17 (Elixir, OTP release) and Loco 1.3.1
  (Rust, release build) on PostgreSQL in the core serving group: article,
  listing and search under load, application and database memory
  separately, CPU per 1,000 requests, cold start, page weight, edit to
  published; Wrought rendered per request beside them.
- `bench/run.sh stall [N] [CORES]`: repeat the 1000-article PostgreSQL
  build with phase timings and the disk's own sync latency beside each run.

- Pre-compressed publish output: every text file gets `.br` (Brotli 5)
  and `.gz` (gzip 6) copies, written in the build's atomic swap and by every
  publish, removed with the file, never stale. Caddy serves them
  (`precompressed br gzip` in the shipped Caddyfiles, no more per-request
  `encode` for files); `engine serve` serves them with a strong
  content-derived ETag per encoding, `Vary: Accept-Encoding`, `304`s and
  `Cache-Control: no-cache` for HTML and other documents; the bucket target
  uploads them with `Content-Encoding`.

### Fixed
- The PostgreSQL build could still stall for 5–8 s in a memory-capped
  container: the search index's commit waited for a write-ahead-log sync
  (PostgreSQL had pruned dead catalog rows in that transaction), although
  the index is unlogged. Writes to the search index now commit without
  waiting for the sync. In the bench stack, 25 builds: worst 9.6 s → 2.9 s,
  stalls 4 → 0.
- Opening the forms feature wrote its stamp key on every build and server
  start (a commit and a disk sync); it is read first and made by
  `engine migrate`.
- Intermittent build stalls on PostgreSQL (one build in three took 7–14 s
  longer): the build waited for write-ahead-log syncs that took seconds on a
  disk slow to sync. The search index is now an unlogged table written in
  one statement (only changed documents), and the build's tag index commits
  without waiting for the sync. 1000 articles, one core, 30 builds: median
  2.98 s → 1.70 s, worst 16.9 s → 3.3 s.
- Edit to published on PostgreSQL took about a second when the edit
  touched many pages: the edited page is now written first, large batches
  render from one content snapshot, and the batch records its index and
  cursor in one transaction (median 1.73 s → 0.11 s on the benchmark site).

### Changed
- Benchmarks page and README: the complete run of 2026-11-27 (core and
  extended groups), with Phoenix, Loco and Rails beside Wrought rendered
  per request, Hugo and Zola beside Wrought's build with and without
  compressed copies, WordPress with and without a page cache, application
  and database memory separately with their anonymous part.
- Pre-compressed copies are written with Google's Brotli (one-shot) and
  libdeflate instead of the Rust ports: the same Brotli bytes, gzip copies
  0.6 % smaller, and a quarter less time compressing (1000-article build on
  one core: 0.79 → 0.66 s locally). `ENGINE_BUILD_PRECOMPRESS=0` builds
  without copies, for measuring what they cost.
- A publish no longer rewrites a page whose bytes did not change.
- `static/` files named like a pre-compressed copy (`app.css.gz`) are
  refused by `engine build`.
- PostgreSQL migration `search/0002_unlogged` makes `engine_search`
  unlogged. Standbys and physical backups do not contain it; the engine
  rebuilds it when it finds it empty.

## [0.7.0] - 2026-11-26 "visual editor"

### Added
- `visual-editor` feature (in the `cms` and `app` presets): a block editor
  at `/admin/editor/<type>/<id>/` for an item's block fields. Block library
  driven by block type files and component manifests, rich text (Tiptap)
  reading and writing Markdown, reordering by buttons, Alt+arrow keys and
  drag and drop (also inside the preview), copy, remove, undo, a live
  preview rendered by the engine from the unsaved blocks, click-to-edit in
  the preview, design choices from the site's tokens, save and publish with
  the version check and a conflict dialog. Svelte 5 + Tiptap 3, built from
  `editor/` into a committed, embedded bundle (`scripts/build-editor.sh`;
  CI and releases rebuild and compare it). Keyboard-operable; checked with
  axe and TagCheck; `scripts/check-editor.sh` drives it in headless
  Chromium.
- `POST /admin/api/items/<type>/<id>/preview/` (render posted blocks without
  saving, validated and sanitised exactly like a save) and
  `PUT /admin/api/items/<type>/<id>/blocks/` (save all block fields in one
  revision, optional status), plus the editor's preview frame.
- Stable block ids (`id` in every block; kept, or assigned by the store).
- `token` field type: a design token name from a group (`color`,
  `background`, `font`, `spacing`), printed as `data-*` attributes that the
  engine's token stylesheet maps onto the tokens; `token_assets()`.
- Block type files take `description`, `[editor]` hints and field
  `default`s; a component block without `[[fields]]` takes them from the
  manifest. `GET /admin/api/items/...` now lists each block field's allowed
  types and their fields and merged editor hints.

### Changed
- The `markdown` template filter passes HTML the engine already made safe
  through unchanged (a block's Markdown field drawn by a component such as
  `rich_text` showed its tags as text).
- The OIDC test client's redirect ports moved from 59111-59119 to
  31111-31119 (`deploy/dex/config.yml`), below the kernel's ephemeral port
  range.

### Fixed
- An intermittent failure of the test suites from a cold start: the OIDC
  end-to-end test listened on a fixed port inside the ephemeral range, which
  any outgoing connection could hold ("Address already in use"). Fixed
  ports are now outside it, checked by the test helper.

## [0.6.0] - 2026-11-26 "completion"

### Added
- `s3` feature (`engine-s3`, in the `cms` and `app` presets): an
  S3-compatible publish target (`[publish] target = "s3"`) for `engine build`
  and publish mode. Uploads only objects whose bytes changed, deletes stale
  ones, then invalidates exactly the changed paths at the CDN (CloudFront or
  Google Cloud CDN behind a provider trait; `log`, `file` and `none` too).
  SigV4 signing written in-house; credentials from the environment or the
  machine's role only. Public media are copied to the bucket under
  `media/`. Tested against MinIO.
- `oidc` feature (in `engine-auth`): single sign-on with OpenID Connect:
  authorization code flow with PKCE, state and nonce, discovery and JWKS,
  ID-token signature and claim checks with `ring`, identities matched by
  issuer and subject only (an unlinked identity is never signed in to an
  account with the same email), linking and unlinking on the profile page.
  Tested against Dex.
- `payments` feature (`engine-payments`, PostgreSQL only): a provider
  trait, a Stripe Checkout adapter, an in-process fake provider, signed
  webhooks with replay protection and idempotency, an order state machine
  applied by jobs (with `processing` and `returned` for bank debits, and
  unmatched events kept for later record types), a server-side cart,
  integer minor units throughout, an orders screen in the admin, and the
  Commerce components (product card, product grid, cart, checkout summary,
  order history). Example site `examples/shop`.
- The rest of the built-in component library: Layout (section, columns,
  grid, stack, container, spacer, divider), Navigation (menu, mobile menu,
  tabs), Content (rich text, video embed, timeline) and Forms (file
  upload). Every component has a manifest (semantic version, group,
  features, typed arguments, editor hints, example); `engine components
  [--json]`, `/admin/api/components` and `/admin/api/tokens` expose the
  catalogue and design tokens as data; block types can render through a
  component. Component scripts and styles load only on pages that use them.
  A versioning policy and a gallery of every component in the docs.
- Starter themes for `engine new --theme`: `docs`, `business` (any preset)
  and `dashboard` (cms, app), built only from components.
- Block trees as JSON in the admin (`/admin/api/items/...`) with the
  version check, for the visual editor.
- `engine build --jobs` / `ENGINE_BUILD_JOBS`: parallel rendering
  (`ENGINE_BUILD_PIN_CPUS=1` for `isolcpus` hosts).
- PostgreSQL over a Unix socket (`?host=/run/postgresql`), and a server
  tuning profile in the docs.
- Accessibility checks: `scripts/check-a11y.sh` runs TagCheck and axe-core
  (light, dark, phone) over the examples, the starter themes, the docs with
  the gallery and the signed-in admin.
- CI for Forgejo Actions and GitHub Actions (`.github/workflows/`):
  formatting, clippy, tests on SQLite and PostgreSQL, the statement
  catalogue, the feature matrix, `cargo audit`, `cargo deny`, accessibility;
  container jobs in a separate workflow that pull requests cannot start.
- `deny.toml` and `.cargo/audit.toml`.
- Signed release binaries: `scripts/release.sh` and a tag-only release
  workflow (SSH signatures, SHA-256 sums, clean builds, `--locked`).
- Project files: `SECURITY.md`, `CONTRIBUTING.md`, `GOVERNANCE.md`, an
  architecture guide and this changelog.

### Changed
- Whole-site renders (`engine build`, full rebuilds) read the database once
  per content type instead of several queries per page; the tag index and
  the search index are written in one transaction each; list pages use a
  new partial index (`engine types sync` adds it to existing sites) and
  related items a covering term index. A 1000-article build on PostgreSQL
  and on SQLite takes a fraction of the time it did (see the benchmark).
- Publish batches record their tag index entries in one transaction.
- Job bookkeeping commits without waiting for the WAL
  (`synchronous_commit = off` per transaction; never for content).
- Prepared statements: up to 512 kept per connection; one connection opened
  at start.
- sqlx 0.8 → 0.9.1 and the bundled SQLite 3.46 → 3.54.1 (libsqlite3-sys
  0.39.1). The minimum Rust version is now 1.94 (required by sqlx 0.9).
- Test containers are named `wrought-test-*`.
- Image decoding and resizing are optimised in debug and test builds too.

### Fixed
- Intermittent PostgreSQL test failures: the publisher's and job workers'
  `LISTEN` connections each held a connection of the shared pool for good,
  starving requests and publish batches.
- Signing in from real browsers failed ("Form expired"):
  `Referrer-Policy: no-referrer` made browsers send `Origin: null`; now
  `same-origin`.
- The search index was rebuilt with one commit per document.

### Security
- Credentials are only sent to checked endpoints: HTTPS (plain HTTP only to
  loopback), exact host allow-lists for Google bearer tokens, CloudFront and
  Stripe, OIDC endpoints on the issuer's host, no user info in URLs, no
  redirects followed by clients that carry credentials.
- One admin gate (session, admin access, TOTP enrolment) for the HTML admin
  and its JSON endpoints, with a test over every admin route.
- Release signing keys only reach tag builds, behind a protected
  environment; container jobs never run for fork pull requests.
- Bundled SQLite updated to 3.54.1 (RUSTSEC-2026-0459); ctutils 0.4.2 →
  0.4.8 (RUSTSEC-2026-0385).

### Deferred
- An optional `askama` compiled-template path (see DECISIONS.md: needs a
  public library API for site crates, planned with v0.8).

## [0.5.0] - 2026-11-26 "polish"

### Added
- `media` feature (`engine-media`): uploads checked by their content (JPEG,
  PNG, GIF, WebP, PDF; SVG and HTML refused), size and pixel limits,
  engine-made file names, EXIF/XMP stripped, responsive variants, sandboxed
  serving under `/media/`, a media library in the admin, `media` content
  fields, `image` and `gallery` components with `srcset`.
- File upload fields in forms (stored privately, downloaded through the
  admin).
- `search` feature (`engine-search`): PostgreSQL `tsvector` or SQLite FTS5,
  fed from the publish change feed; search box and results, content list,
  filtered list, tag cloud and related items components, all working
  without JavaScript; Pagefind for static sites as an external tool.
- `api` feature (`engine-api`): JSON API for content types under
  `/api/v1/`, hashed, scoped, revocable tokens managed in the admin,
  roles and per-field rights, mandatory version checks (`If-Match`),
  pagination, rate limits and one error shape.
- SQLite parity: every feature that needs a database runs on SQLite as
  well as PostgreSQL (jobs and mail stay PostgreSQL-only, with a clear
  error).
- Email verification for registrations, TOTP for members, a jobs screen in
  the admin.
- `bench/`: a one-command benchmark against WordPress, Drupal and Astro,
  with results on the docs site.

### Changed
- Engine queries moved from `sqlx::query!` to a statement catalogue
  (`engine_db::statements!`) checked against both backends by a test;
  `.sqlx/` offline data and `scripts/sqlx-prepare.sh` removed, and builds
  need no database.
- The `cms` preset compiles both database backends plus `media` and
  `search`; `app` adds `api`.
- `related_items` is two indexed lookups instead of a self-join (a
  1000-article build went from 7.2 s to 4.2 s).

## [0.4.0] - 2026-11-26 "app"

### Added
- `request` render mode with a fragment cache (moka): `fragment()` in
  templates, eviction by cache tags from the publish change feed, shared
  fragments rendered without user data, per-user fragments keyed by
  account; `X-Wrought-Fragments` header.
- `forms` feature (`engine-forms`): forms from files or built in the
  admin, server-side validation, stored submissions, honeypot, time trap
  and rate limits, CSV export; contact, newsletter and multi-step
  components.
- `jobs` feature (`engine-jobs`): PostgreSQL job queue with
  `FOR UPDATE SKIP LOCKED`, leases, retries with backoff, dead jobs,
  schedules and graceful shutdown; `engine jobs` commands.
- `mail` feature (`engine-mail`): SMTP through lettre with text and HTML
  templates, sent from jobs.
- `workflow`: review before publishing, draft preview, notifications.
- Accounts components (login, register, password reset, profile, account
  menu), opt-in registration, and the App UI components.
- The `app` preset and `engine new --preset app` (`examples/small-app`).

### Changed
- TOTP secrets are encrypted at rest (`ENGINE_SECRET_KEY`); QR codes are
  drawn on the server.

### Fixed
- Flaky database tests: a readiness race on fresh PostgreSQL volumes, an
  unbounded number of test connections, and overlapping runs dropping each
  other's databases.

## [0.3.0] - 2026-11-26 "admin"

### Added
- `auth` feature (`engine-auth`): accounts with Argon2id, server-side
  sessions, `__Host-` Secure/HttpOnly/SameSite cookies, CSRF on every form,
  login rate limits, password reset, optional TOTP.
- `roles`: roles with per-type and per-field rights, enforced on the
  server.
- `admin` feature (`engine-admin`): server-rendered admin (minijinja,
  bundled htmx 2.0.11) for content, taxonomy, revisions, users and roles;
  a conflict screen for stale saves that keeps the editor's changes.
- A first `request` mode (`requires = "auth"`, the session and query-string
  rule), `engine user` commands, paginated database listings, template
  reload in `engine serve`.

### Security
- Engine-made HTML reaches templates as safe values and every `| safe` was
  removed; the built-in `page_list` component printed a plain-text summary
  unescaped (stored XSS). Builds warn about `| safe` in site templates.

## [0.2.0] - 2026-11-26 "publish"

### Added
- `db-postgres` feature (`engine-db`): connection pool, migration runner
  with checksums and an advisory lock, throwaway test databases.
- `content` feature (`engine-content`): content types from `types/*.toml`
  (fields, relations, taxonomy, blocks, singletons), `engine types sync`,
  optimistic locking with revisions in the same transaction, stable slugs,
  sanitised rendering, seed files, template functions.
- `publish` render mode (`engine-publish`): change log and NOTIFY
  listener, tag index, atomic disk target, render on miss,
  `engine rebuild [--tag]`.
- The `cms` preset (`examples/cms-site`), a Caddy front door with fallback
  to the engine, and a PostgreSQL Compose file.

## [0.1.0] - 2026-11-26 "static"

### Added
- `engine-core` and the `engine` binary: `engine new`, `dev`, `build`,
  `serve` and `check`.
- Build mode with minijinja templates, Markdown content with TOML front
  matter, sections and pages, cache tags, an atomic output swap.
- The core component set, with TagBrush.css 2.0.1 and TagAction 0.2.0
  bundled and served from the site's origin; TagCheck as an external
  checker.
- The documentation site, built with the engine.

[Unreleased]: https://github.com/kovirlabs/wrought/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/kovirlabs/wrought/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kovirlabs/wrought/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/kovirlabs/wrought/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/kovirlabs/wrought/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/kovirlabs/wrought/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kovirlabs/wrought/releases/tag/v0.1.0
