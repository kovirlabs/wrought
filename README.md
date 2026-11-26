# Wrought

One open source Rust engine for static sites and web apps. Each route chooses
when it renders (the *render dial*: `build`, `publish` or `request`), and each
capability is a Cargo feature switched on at build time. With nothing switched
on, Wrought is a static site generator.

The full design is in [`wrought/`](wrought/) (the spec). This is
**version 0.1, the "static" milestone**: `core`, build mode, minijinja
templates, Markdown content, and `engine new / dev / build / serve / check`.
Its acceptance test, *the engine builds the project's own docs site*, is
`docs/`.

## Quick start

```sh
cargo build --release
./target/release/engine new my-site --preset static
cd my-site
../target/release/engine dev          # http://127.0.0.1:4400/
```

Read the docs by running them:

```sh
cargo run -p engine-cli -- dev --site docs
```

## Repository layout

```text
crates/
  engine-core/   config, render modes, routes, cache tags, templates,
                 Markdown content, build mode, static server
    builtin/     built-in layouts and the `core` component set
    vendor/      bundled TagBrush.css and TagAction (see VENDOR.toml)
  engine-cli/    the `engine` binary
examples/
  static-blog/   the `static` preset copied by `engine new`
docs/            the documentation site, built with the engine
deploy/          Caddy front door (Caddyfile + Docker Compose)
wrought/         the specification
```

## Status

| Area | v0.1 |
| --- | --- |
| Render modes | `build` works. `publish` and `request` parse and are rejected with "not in this build" |
| Cache tags | Recorded on every render (`i:`, `c:`, `s:`), written to `.engine-manifest.json`, shown by `engine build -v` |
| Features | `static` preset (core only) is the default and only preset |
| Styling / behaviour | TagBrush.css 2.0.1 and TagAction 0.2.0 bundled, served from the site's origin |
| Checks | `engine check` and `engine dev` run TagCheck 0.1.3 as an external Node.js tool |

Later milestones (from the spec): v0.2 Postgres, content types and publish
mode; v0.3 auth, roles and admin; v0.4 request mode, forms, jobs and mail;
v0.5 media, search, API and SQLite; then the visual editor and the full
component library.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Serve the built docs through Caddy, as in production:

```sh
cargo run -p engine-cli -- build --site docs
docker compose -f deploy/compose.yml up -d    # http://127.0.0.1:8790/
docker compose -f deploy/compose.yml down
```

## Licence

Wrought is dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. The bundled TagBrush.css and
TagAction are Apache-2.0; their licence texts are in
`crates/engine-core/vendor/` and are written to every built site under
`/_engine/licenses/`.
