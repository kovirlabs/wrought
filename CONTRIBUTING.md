# Contributing to Wrought

Thank you for helping. This file says how work flows, what a change needs
before it is merged, and the few project rules that keep the engine lean.
How decisions are made is in [GOVERNANCE.md](GOVERNANCE.md); security
problems go privately, as described in [SECURITY.md](SECURITY.md), never in
a public issue.

## Where things happen

- **Repository:** [github.com/kovirlabs/wrought](https://github.com/kovirlabs/wrought)
  (issues, pull requests, releases).
- Start with the [architecture guide](docs/content/docs/architecture.md),
  the [specification](wrought/) and [DECISIONS.md](DECISIONS.md).

## Finding something to work on

Issues are labelled by area, one label per crate, plus a difficulty:

| Label | Area |
| --- | --- |
| `crate: core` | `engine-core`: config, routes, templates, components, build mode, server |
| `crate: db` | `engine-db`: pools, statements, migrations |
| `crate: content` | `engine-content` |
| `crate: publish` | `engine-publish` (and the S3 target, `engine-s3`) |
| `crate: auth` | `engine-auth` (and `engine-oidc`) |
| `crate: admin` | `engine-admin` |
| `crate: forms`, `crate: jobs`, `crate: mail`, `crate: media`, `crate: search`, `crate: api`, `crate: payments` | the crate of that name |
| `crate: cli` | `engine-cli`: commands and feature wiring |
| `area: components` | built-in components and starter themes |
| `area: docs`, `area: ci` | the docs site; CI and release tooling |
| `good first issue` | small, well described, with a pointer to the code; combined with a crate label |
| `help wanted` | the maintainers would welcome a pull request |

`good first issue` issues name the files to change and how to test the
result. If you want to take one, say so in the issue so nobody duplicates
the work.

## The workflow

1. For anything bigger than a bug fix or a docs edit, open an issue first
   and describe the change; a short agreement on the approach saves a
   rewrite.
2. Fork, create a branch, make the change with tests and docs.
3. Run the checks below locally.
4. Open a pull request against `main` that says what changed and why, and
   links the issue. CI runs the same checks.
5. A maintainer reviews. Expect questions; small follow-up commits are fine,
   the branch is squashed or rebased when merged.

Commit messages: a short summary line (imperative, up to about 70
characters), a blank line, then what and why. Prefix the area when it helps
(`publish: retry failed renders on reload`).

## Checks

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test                      # the default (static) engine; no database
scripts/test-sqlite.sh          # every crate and feature on SQLite
scripts/test-postgres.sh        # the same on PostgreSQL, with Mailpit (Docker)
scripts/check-queries.sh        # every statement prepares on both backends
scripts/check-features.sh       # every allowed feature combination compiles
scripts/check-a11y.sh           # TagCheck and axe-core over the built pages (Node.js)
cargo deny check && cargo audit # licences, advisories, sources
```

`scripts/test-postgres.sh` starts its own containers (`wrought-test-*`, on
localhost ports) with Docker Compose. Tests that need a database read
`ENGINE_TEST_DATABASE_URL` and are skipped without it, so `cargo test`
always works on a machine with nothing installed. Use `-p <crate>` with the
test scripts while you work, the full runs before you push.

### What CI runs

CI runs on Forgejo Actions and GitHub Actions from three workflows in
`.github/workflows/`:

- `ci.yml` runs for every push to `main` and every pull request, including
  ones from forks: formatting, clippy, `cargo test`, the SQLite suite, the
  SQLite half of the statement catalogue, the feature matrix, `cargo audit`,
  `cargo deny` and the accessibility checks. All its jobs use the
  `ubuntu-latest` label.
- `host.yml` runs the PostgreSQL suite (with Mailpit and the other test
  containers) and the catalogue on both backends. These need Docker on the
  runner's host, so `host.yml` has **no pull-request trigger**: it runs only
  after a change is merged to `main`, or when a maintainer starts it by
  hand. Run `scripts/test-postgres.sh` locally before asking for a merge.
- `release.yml` runs only for pushed `v*` tags (see
  [releases](docs/content/docs/releases.md)).

### CI runners (for the repository owner)

A workflow file in a pull request can be edited by its author, so the
workflows themselves cannot stop a fork from asking for a powerful runner.
The protection is in the forge and runner settings, which the owner must
make:

- **Sandboxed runners for `ubuntu-latest`.** On GitHub, the hosted
  machines. On Forgejo or Codeberg, a runner that starts a fresh container
  for every job (preferably an ephemeral VM), with no Docker socket
  mounted, no host mode, and nothing secret on the machine.
- **The host runner (`WROUGHT_DOCKER_RUNNER`) never takes jobs from forks.**
  On Forgejo, register it for this repository only, with a label that only
  `host.yml` uses, and do not give it the `ubuntu-latest` label. On GitHub,
  put any self-hosted runner in a runner group limited to the selected
  workflows (`host.yml`), with "allow public repositories" off. Prefer an
  ephemeral VM per job there too.
- **Maintainer approval for workflow runs from fork pull requests:** on
  GitHub "Require approval for all external contributors"; on Forgejo, keep
  the default that a maintainer approves the first run of a new
  contributor's pull request, and check what a pull request changes in
  `.github/` before approving it.
- **The host runner is never the release runner.** Release builds
  (`WROUGHT_RELEASE_RUNNER`) run on a clean, ephemeral machine that runs
  nothing else.

For a faster local build on a slow disk, set
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`.

## Project rules

### Features stay switched off until asked for

Every capability is a Cargo feature of `engine-cli` that switches on its
crate plus an empty marker feature of `engine-core`. Code that is switched
off must not be compiled: the default `static` build has no database,
image or account code (`cargo tree -p engine-cli -e normal` shows no sqlx),
and features that are not compiled in have no routes. A new feature:

- gets its own crate (`crates/engine-<name>/`) depending on `engine-core`,
  not the other way round;
- is added to the feature list in `crates/engine-cli/Cargo.toml` with its
  dependency chain, and its combinations to `scripts/check-features.sh`;
- plugs in through the extension hooks (`SiteExtension`, `PublishTarget`,
  ...; see the architecture guide) behind `#[cfg(feature = ...)]` in the
  CLI.

### SQL goes through the statement catalogue

Never `sqlx::query!`. Declare statements in the crate's `sql` module with
`engine_db::statements!` (one text for both backends, `{ postgres, sqlite }`
where dialects differ, `{ postgres }` for Postgres-only features), run them
through `engine_db::Query`, and add migrations for both backends under
`migrations/postgres/` and `migrations/sqlite/`. The catalogue test
(`crates/engine-cli/tests/queries.rs`) must see every new statement. Content
tables use run-time queries with validated identifiers and bound values
only; never put a value into SQL text.

### Security baseline

Auto-escaping everywhere, no `| safe` in templates (engine-made HTML is
passed as a safe value), CSRF on every form, `__Host-` cookies, secrets
only from environment variables and redacted in `Debug` and errors. A
change that touches accounts, sessions, uploads or the API needs tests that
try to break it.

### Components

Built-in components are templates named `components/<name>.html`, render
complete semantic HTML without JavaScript, follow the site's design tokens,
target WCAG 2.2 AA (checked by `scripts/check-a11y.sh`) and are versioned:
a breaking markup change waits for a major release and gets an upgrade
note.

### Decisions are written down

A significant decision (a new dependency, a schema shape, a security
trade-off, a deviation from the spec) gets an entry in
[DECISIONS.md](DECISIONS.md) in the same pull request: a `###` title, then
**Decision**, **Why** and **Rejected** (the alternatives). User-visible
changes get a line in [CHANGELOG.md](CHANGELOG.md) under "Unreleased".

### Dependencies

Few, maintained and permissively licensed: `cargo deny check` enforces the
licence list, crates.io as the only source and the RustSec advisories. Prefer
what is already in the tree. Say in the pull request why a new crate is
worth it.

### Style

`rustfmt` defaults, clippy clean. Docs, comments and DECISIONS entries in
plain, concise British English, like the existing pages; no emojis.

## Licensing of contributions

Wrought is dual-licensed under MIT or Apache-2.0. Unless you say otherwise,
any contribution you submit for inclusion is licensed under the same terms,
MIT OR Apache-2.0, without additional terms or conditions (inbound =
outbound, as in section 5 of the Apache licence). There is no contributor
licence agreement and no sign-off (DCO) requirement; by opening a pull
request you confirm that you have the right to submit the work under these
terms.

## Conduct

Be kind and precise. Criticise code, not people. Maintainers may hide
comments, lock threads or block accounts that harass others; see
[GOVERNANCE.md](GOVERNANCE.md).
