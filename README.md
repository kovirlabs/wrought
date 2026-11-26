# Wrought

One open source Rust engine for static sites and web apps. Each route chooses
when it renders (the *render dial*: `build`, `publish` or `request`), and each
capability is a Cargo feature switched on at build time. With nothing switched
on, Wrought is a static site generator.

The full design is in [`wrought/`](wrought/) (the spec). This is
**version 0.5, the "polish" milestone**. The default build is still the
static site generator of 0.1 (`core`, build mode, minijinja templates,
Markdown content, `engine new / dev / build / serve / check`), with no
database, media or account code compiled in. The `cms` preset adds a data
layer on **SQLite or PostgreSQL** (the URL picks), content types, the
`publish` render mode (0.2), accounts, roles and the admin with
conflict-safe editing (0.3), review before publishing, a **media library**
(uploads checked by content, metadata stripped, responsive variants) and
**full-text search** (0.5). The `app` preset adds the `request` render
mode's fragment cache, forms with a spam guard and file uploads, background
jobs and email (PostgreSQL), the Accounts and App UI components (0.4) and a
**JSON API** with scoped tokens (0.5). Acceptance for 0.5:
`crates/engine-cli/tests/polish.rs` drives media, search, the API and form
uploads through the real binary, on both backends. A
[benchmark](docs/content/docs/benchmarks.md) compares Wrought with
WordPress, Drupal and Astro (`bench/run.sh`). Significant decisions are
logged in [DECISIONS.md](DECISIONS.md).

## Quick start

```sh
cargo build --release
./target/release/engine new my-site --preset static
cd my-site
../target/release/engine dev          # http://127.0.0.1:4400/
```

A CMS site on SQLite, nothing else to install (or PostgreSQL: see
[`examples/cms-site`](examples/cms-site/README.md)):

```sh
cargo build --release --features cms
export ENGINE_DATABASE_URL=sqlite://data/site.db     # relative to the site
E=./target/release/engine S=examples/cms-site
$E migrate --site $S
$E media add --site $S $S/media-samples/harbour-at-dusk.jpg --alt "A harbour at dusk"
$E media add --site $S $S/media-samples/forge-yard.jpg --alt "A forge yard at sunset"
$E content seed --site $S && $E build --site $S
$E user create --site $S --email you@example.org   # prints a link to set the password
$E serve --site $S     # then open http://localhost:4400/admin/ and /search/
```

A web app (also needs an SMTP server for mail; Mailpit runs locally):

```sh
cargo build --release --features app
docker compose -f deploy/compose.mailpit.yml up -d --wait
export ENGINE_SMTP_URL=smtp://127.0.0.1:51025 ENGINE_SECRET_KEY="$(openssl rand -base64 48)"
E=./target/release/engine S=examples/small-app
$E migrate --site $S && $E content seed --site $S && $E build --site $S
$E serve --site $S     # then open http://localhost:4400/register/
```

Read the docs by running them:

```sh
cargo run -p engine-cli -- dev --site docs
```

## Repository layout

```text
crates/
  engine-core/     config, render modes, routes, cache tags, templates,
                   Markdown content, build mode, the Renderer, static server
    builtin/       built-in layouts and the `core` component set
    vendor/        bundled TagBrush.css and TagAction (see VENDOR.toml)
  engine-db/       `db-postgres`, `db-sqlite`: pools, the statement catalogue
                   and backend-neutral queries, migrations, test DBs
  engine-content/  `content`: types/*.toml, blocks, taxonomy, revisions,
                   slugs, optimistic locking, sanitising, `types sync`, seeds
  engine-publish/  `publish`: tag index, NOTIFY listener, publisher,
                   atomic disk target, render-on-miss
  engine-auth/     `auth` + `roles`: accounts, Argon2id, sessions, CSRF,
                   password reset, rate limits, TOTP, roles, field access
  engine-admin/    `admin`: the admin UI (minijinja + bundled htmx), with
                   `workflow` (review, preview) and the forms screens
  engine-forms/    `forms`: form definitions, validation, submissions, spam guard
  engine-jobs/     `jobs`: job queue and schedules on Postgres (SKIP LOCKED)
  engine-mail/     `mail`: SMTP (lettre) with minijinja templates, via jobs
  engine-media/    `media`: uploads checked by content, metadata stripping,
                   responsive variants, `/media/`, `media()`
  engine-search/   `search`: Postgres tsvector / SQLite FTS5, search components
  engine-api/      `api`: JSON API for content types, scoped tokens
  engine-cli/      the `engine` binary
examples/
  static-blog/     the `static` preset copied by `engine new`
  cms-site/        the `cms` preset: content on SQLite or PostgreSQL, publish
                   mode, admin, media, search
  small-app/       the `app` preset: accounts, forms, receipts, App UI
docs/              the documentation site, built with the engine
deploy/            Caddy front doors, Postgres, Mailpit (Docker Compose)
bench/             the benchmark against WordPress, Drupal and Astro
scripts/           test-sqlite.sh, test-postgres.sh, check-queries.sh,
                   check-features.sh
wrought/           the specification
```

## Status

| Area | v0.5 |
| --- | --- |
| Render modes | `build` everywhere; `publish` and `request` in `cms`/`app` builds; `request` pages reuse fragments from an in-memory cache (moka) evicted by the same cache tags and change feed as `publish`, never shared across accounts |
| Accounts | Argon2id, server-side sessions, `__Host-` Secure/HttpOnly/SameSite cookies, CSRF on every form, login rate limits, registration (opt-in) with email verification, password reset by email, optional TOTP for every account with QR code and secrets encrypted at rest |
| Admin | Content of every type, taxonomy, revisions, conflict screen, accounts, roles with per-field access, review before publishing with preview, forms and submissions with a form builder, media library, jobs screen, API tokens |
| Forms | Files or admin-made; server-side validation; stored submissions; file uploads (private media); honeypot, time trap and rate limit; receipts and notifications by email; contact, newsletter and multi-step components |
| Jobs and mail | Postgres queue (`FOR UPDATE SKIP LOCKED`), leases, retries with backoff, dead jobs, schedules, graceful shutdown; SMTP via lettre with text + HTML templates; Postgres only |
| Media | Content sniffing (JPEG, PNG, GIF, WebP, PDF; SVG/HTML refused), size and pixel limits, engine-made names, EXIF/XMP stripped, responsive variants, sandboxed serving, `media` content fields |
| Search | Postgres `tsvector` or SQLite FTS5, results without JavaScript; Pagefind for static sites (external tool) |
| API | `/api/v1/` content CRUD, hashed scoped revocable tokens, roles and field rights, mandatory version checks, pagination, rate limits, one error shape |
| Components | Core layout/navigation/content set with image and gallery; Listings (content list, filtered list, tag cloud, related items, search box and results); Accounts; App UI; Forms with file upload |
| Features | `static` (default, core only), `cms`, `app` presets; every allowed combination compiles (`scripts/check-features.sh`) |
| Data | SQLite or PostgreSQL via sqlx; statements checked against both by a catalogue test (`scripts/check-queries.sh`) |
| Publish target | Local disk with atomic renames; S3 + CDN invalidation planned |
| Styling / behaviour | TagBrush.css 2.0.1 and TagAction 0.2.0 bundled, served from the site's origin; htmx 2.0.11 bundled with `admin` |
| Checks | `engine check` and `engine dev` run TagCheck 0.1.3 as an external Node.js tool |

Next: the visual editor, the rest of the component library, payments,
OIDC, the S3 publish target, starter themes and the project's community
files.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test                      # the default (static) engine; no database code
scripts/test-sqlite.sh          # every crate and feature on SQLite (no container)
scripts/test-postgres.sh        # every crate and feature, with PostgreSQL and Mailpit
scripts/check-queries.sh        # every engine statement prepares on both backends
scripts/check-features.sh       # every allowed feature combination compiles
```

Tests that need a database read `ENGINE_TEST_DATABASE_URL` (`sqlite:` or a
`postgres://` URL; mail tests also `ENGINE_TEST_SMTP_URL` and
`ENGINE_TEST_MAILPIT_URL`) and are skipped without it, so `cargo test
--workspace --all-features` also passes on a machine with no database.
`scripts/test-postgres.sh` starts the `wrought-postgres` (port 55432) and
`wrought-mailpit` (SMTP 51025, API 58025) containers and runs everything
against them. Builds need no database: queries are checked by the catalogue
test, not at compile time (see [databases](docs/content/docs/databases.md)).

Serve the built docs through Caddy, as in production:

```sh
cargo run -p engine-cli -- build --site docs
docker compose -f deploy/compose.yml up -d    # http://127.0.0.1:8790/
docker compose -f deploy/compose.yml down
```

With publish routes, Caddy serves existing files and passes misses to
`engine serve` (`deploy/compose.publish.yml`, port 8791; see
[deployment](docs/content/docs/deployment.md)).

## Licence

Wrought is dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. The bundled TagBrush.css and
TagAction are Apache-2.0; their licence texts are in
`crates/engine-core/vendor/` and are written to every built site under
`/_engine/licenses/`. htmx (bundled by the `admin` feature) is 0BSD; see
`crates/engine-admin/vendor/` (served at `/_engine/ui/htmx-LICENSE.txt`).
