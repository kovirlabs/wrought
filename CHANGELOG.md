# Changelog

All notable changes to Wrought are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (before 1.0, a minor version may
break things; breaking changes are called out). The reasons behind each
change are in [DECISIONS.md](DECISIONS.md).

## [Unreleased]

## [0.6.0] - unreleased

<!-- LEAD TO FINALISE: check each item against what was merged, add the
date, and move anything that did not land back to "Unreleased". -->

### Added
- `s3` feature (`engine-s3`): an S3-compatible publish target
  (`[publish] target = "s3"`), with CDN invalidation of the changed paths
  after each publish; tested against MinIO.
- `oidc` feature (`engine-oidc`): single sign-on with OpenID Connect on top
  of `auth`.
- `payments` feature (`engine-payments`, PostgreSQL only): checkout and
  webhooks through a pluggable provider, and the Commerce components
  (product card, product grid, cart, checkout summary, order history).
- The rest of the built-in component library, a component gallery in the
  docs, and component versioning with upgrade notes.
- Starter themes (documentation, business site, app dashboard) for
  `engine new`.
- An optional compiled-template path with askama; minijinja stays the
  default.
- Accessibility checks in CI: `scripts/check-a11y.sh` runs TagCheck and
  axe-core over the examples, the docs gallery and the admin pages.
- CI for Forgejo Actions and GitHub Actions: formatting, clippy, tests on
  SQLite and PostgreSQL, the statement catalogue, the feature matrix,
  `cargo audit`, `cargo deny` and the accessibility checks.
- `deny.toml` (licences, advisories, bans, sources) and `.cargo/audit.toml`.
- Signed release binaries: `scripts/release.sh` and a release workflow
  build the static, cms and app presets for x86_64 and aarch64 Linux,
  with SHA-256 checksums and SSH signatures.
- Project files: `SECURITY.md`, `CONTRIBUTING.md`, `GOVERNANCE.md`, an
  architecture guide (`ARCHITECTURE.md`, docs "Architecture") and this
  changelog.

### Changed
- `engine build` renders pages in parallel.
- sqlx 0.8 → 0.9.1 and the bundled SQLite 3.46 → 3.54.1 (libsqlite3-sys
  0.39.1), fixing RUSTSEC-2026-0459. The minimum Rust version is now 1.94
  (required by sqlx 0.9).

### Fixed
- A flaky test in the suite (see DECISIONS.md, v0.6).

### Security
- Bundled SQLite updated to 3.54.1 (RUSTSEC-2026-0459, CVE-2026-119375:
  uninitialised memory returned when a database is opened with a crafted
  `-wal` file).
- ctutils 0.4.2 → 0.4.8 (RUSTSEC-2026-0385, non-constant-time comparisons,
  reached through sqlx-postgres' SCRAM authentication).

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
