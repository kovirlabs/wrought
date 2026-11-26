# Wrought

One open source Rust engine for static sites and web apps. Each route chooses
when it renders (the *render dial*: `build`, `publish` or `request`), and each
capability is a Cargo feature switched on at build time. With nothing switched
on, Wrought is a static site generator.

The full design is in [`wrought/`](wrought/) (the spec). This is
**version 0.3, the "admin" milestone**. The default build is still the
static site generator of 0.1 (`core`, build mode, minijinja templates,
Markdown content, `engine new / dev / build / serve / check`), with no
database or account code compiled in. Built with the `cms` preset it adds
PostgreSQL, content types declared in TOML, the `publish` render mode (a
content change rewrites only the pages that show it; v0.2), and now
accounts, roles with per-field access, and a server-rendered admin with
revisions and optimistic locking: two editors saving the same item get a
conflict screen, not an overwrite (acceptance test:
`crates/engine-cli/tests/admin.rs`, through the real HTTP routes).
Significant decisions are logged in [DECISIONS.md](DECISIONS.md).

## Quick start

```sh
cargo build --release
./target/release/engine new my-site --preset static
cd my-site
../target/release/engine dev          # http://127.0.0.1:4400/
```

A CMS site (needs Docker for PostgreSQL; see
[`examples/cms-site`](examples/cms-site/README.md)):

```sh
cargo build --release --features cms
docker compose -f deploy/compose.postgres.yml up -d --wait
export ENGINE_DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought
E=./target/release/engine S=examples/cms-site
$E migrate --site $S && $E content seed --site $S && $E build --site $S
$E user create --site $S --email you@example.org   # prints a link to set the password
$E serve --site $S     # then open http://localhost:4400/admin/
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
  engine-db/       `db-postgres`: connection pool, migration runner, test DBs
  engine-content/  `content`: types/*.toml, blocks, taxonomy, revisions,
                   slugs, optimistic locking, sanitising, `types sync`, seeds
  engine-publish/  `publish`: tag index, NOTIFY listener, publisher,
                   atomic disk target, render-on-miss
  engine-auth/     `auth` + `roles`: accounts, Argon2id, sessions, CSRF,
                   password reset, rate limits, TOTP, roles, field access
  engine-admin/    `admin`: the admin UI (minijinja + bundled htmx)
  engine-cli/      the `engine` binary
examples/
  static-blog/     the `static` preset copied by `engine new`
  cms-site/        the `cms` preset: PostgreSQL content, publish mode, admin
docs/              the documentation site, built with the engine
deploy/            Caddy front doors and Postgres (Docker Compose)
scripts/           test-postgres.sh, sqlx-prepare.sh
.sqlx/             offline data for sqlx's compile-time checked queries
wrought/           the specification
```

## Status

| Area | v0.3 |
| --- | --- |
| Render modes | `build` everywhere; `publish` and `request` (per request, no fragment cache yet) in `cms` builds |
| Accounts | Argon2id, server-side sessions, `__Host-` Secure/HttpOnly/SameSite cookies, CSRF on every form, login rate limits, password reset (link logged until `mail`), optional TOTP |
| Admin | Content of every type (forms from `types/*.toml`), taxonomy, revisions with restore, conflict screen, accounts, roles with per-field access; works without JS, htmx bundled |
| Cache tags | Recorded on every render; `publish` pages' tags kept in a tag index in the database, which decides what an edit rewrites |
| Features | `static` (default, core only) and `cms` (`db-postgres`, `content`, `publish`, `auth`, `roles`, `admin`) presets |
| Data | PostgreSQL via sqlx; SQLite planned for v0.5 |
| Publish target | Local disk with atomic renames; S3 + CDN invalidation planned |
| Styling / behaviour | TagBrush.css 2.0.1 and TagAction 0.2.0 bundled, served from the site's origin; htmx 2.0.11 bundled with `admin` |
| Checks | `engine check` and `engine dev` run TagCheck 0.1.3 as an external Node.js tool |

Later milestones (from the spec): v0.4 request mode with the fragment
cache, forms, jobs and mail; v0.5 media, search, API and SQLite; then the
visual editor and the full component library.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test                      # the default (static) engine; no database code
scripts/test-postgres.sh        # every crate and feature, with PostgreSQL
```

Tests that need PostgreSQL read `ENGINE_TEST_DATABASE_URL` and are skipped
without it, so `cargo test --workspace --all-features` also passes on a
machine with no database. `scripts/test-postgres.sh` starts the
`wrought-postgres` container (`deploy/compose.postgres.yml`, port 55432) and
runs everything against it. Builds need no database: sqlx's compile-time
query checks use the committed `.sqlx/` data; regenerate it with
`scripts/sqlx-prepare.sh` after changing a query or the engine's schema.

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
