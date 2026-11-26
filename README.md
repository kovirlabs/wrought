# Wrought

One open source Rust engine for static sites and web apps. Each route chooses
when it renders (the *render dial*: `build`, `publish` or `request`), and each
capability is a Cargo feature switched on at build time. With nothing switched
on, Wrought is a static site generator.

The full design is in [`wrought/`](wrought/) (the spec). This is
**version 0.2, the "publish" milestone**. The default build is still the
static site generator of 0.1 (`core`, build mode, minijinja templates,
Markdown content, `engine new / dev / build / serve / check`), with no
database code compiled in. Built with the `cms` preset it adds PostgreSQL,
content types declared in TOML, and the `publish` render mode: a content
change rewrites only the pages that show it (acceptance test:
`crates/engine-publish/tests/publish.rs`). Significant decisions are logged
in [DECISIONS.md](DECISIONS.md).

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
$E serve --site $S     # then, elsewhere: $E content set --site $S article 1 --set title=Hi
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
  engine-cli/      the `engine` binary
examples/
  static-blog/     the `static` preset copied by `engine new`
  cms-site/        the `cms` preset: PostgreSQL content, publish mode
docs/              the documentation site, built with the engine
deploy/            Caddy front doors and Postgres (Docker Compose)
scripts/           test-postgres.sh, sqlx-prepare.sh
.sqlx/             offline data for sqlx's compile-time checked queries
wrought/           the specification
```

## Status

| Area | v0.2 |
| --- | --- |
| Render modes | `build` everywhere; `publish` in `cms` builds; `request` parses and is rejected with "not in this build" |
| Cache tags | Recorded on every render; `publish` pages' tags kept in a tag index in the database, which decides what an edit rewrites |
| Features | `static` (default, core only) and `cms` (`db-postgres`, `content`, `publish`) presets |
| Data | PostgreSQL via sqlx; SQLite planned for v0.5 |
| Publish target | Local disk with atomic renames; S3 + CDN invalidation planned |
| Styling / behaviour | TagBrush.css 2.0.1 and TagAction 0.2.0 bundled, served from the site's origin |
| Checks | `engine check` and `engine dev` run TagCheck 0.1.3 as an external Node.js tool |

Later milestones (from the spec): v0.3 auth, roles and admin; v0.4 request
mode, forms, jobs and mail; v0.5 media, search, API and SQLite; then the
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
`/_engine/licenses/`.
