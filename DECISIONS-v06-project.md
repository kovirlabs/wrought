## v0.6 "project" workstream (2026-11-26)

### One CI workflow in `.github/workflows/`, read by Forgejo and GitHub
- **Decision:** `ci.yml` and `release.yml` live in `.github/workflows/`,
  with a shared local composite action `.github/actions/rust` (installs
  Rust with rustup when the image has none, restores the cargo cache).
  There is deliberately no `.forgejo/workflows/` folder: Forgejo reads
  `.github/workflows/` only when that folder is absent, and GitHub reads only
  `.github/workflows/`. Actions used are the short names `actions/checkout@v4`,
  `actions/cache@v4`, `actions/setup-node@v4`,
  `actions/upload-artifact@v4`/`download-artifact@v4`, which GitHub has and
  Forgejo resolves against `DEFAULT_ACTIONS_URL` (data.forgejo.org mirrors
  all of them; checked 2026-11-26). No marketplace actions
  (dtolnay/rust-toolchain, Swatinem/rust-cache are not mirrored).
  `forgejo-release` is only used in a Forgejo-only job (GitHub downloads
  every action of a job before running it, so it must not appear in a job
  GitHub runs).
- **Runner labels are variables:** `runs-on: ${{ vars.WROUGHT_RUNNER ||
  'ubuntu-latest' }}` (Forgejo allows `vars` in `runs-on`, as GitHub does).
  Container jobs use `WROUGHT_DOCKER_RUNNER`.
- **Why:** one file to keep in step; the forge-specific part is reduced to
  two variables.
- **Rejected:** two copies (`.forgejo/` and `.github/`), which drift;
  `.forgejo/` only (the GitHub mirror would run nothing).

### Test services: the jobs call the scripts, which start the containers
- **Decision:** the Postgres and catalogue jobs run `scripts/test-postgres.sh`
  and `scripts/check-queries.sh` unchanged; the scripts start
  `wrought-test-postgres`, `wrought-test-mailpit` and whatever containers the
  s3/oidc work adds, with `docker compose`, on 127.0.0.1 ports. So the job
  needs a runner with Docker and the compose plugin whose job sees the
  host's loopback: GitHub's `ubuntu-latest`, or on Forgejo a runner with a
  `:host` label (capacity 1, since container names and ports are fixed).
  Containers are left running after the job (the scripts reuse them;
  GitHub machines are discarded), because a cleanup step on a host runner
  that is also a development machine would delete the developer's test
  volume.
- **Why:** CI and developers run the same commands; new test services need
  no workflow change.
- **Rejected:** `services:` containers in the workflow (a second
  definition of every service, and the scripts' `docker exec` readiness
  check and cleanup of stale databases would need a CI-only path);
  Docker-in-Docker on Forgejo (privileged runners).
- **Codeberg hosted runners do not fit:** `codeberg-tiny/small/medium` cap
  a job at 2/5/10 minutes and offer no Docker daemon
  (codeberg.org/actions/meta, read 2026-11-26). A workspace build with all
  features takes longer. On Codeberg the project registers its own runner
  (or uses the internal Forgejo's runner); the variables above select it.

### Checker versions pinned and cached; audit and deny both run
- `cargo-audit` 0.23.0 and `cargo-deny` 0.22.1 are installed with
  `cargo install --locked --version` into `~/.cargo-tools`, cached by
  version, so a new checker release cannot fail CI by itself. `cargo audit
  --deny warnings` reads `.cargo/audit.toml`; `cargo deny --all-features
  check` reads `deny.toml`. Both run because the spec asks for both and
  they differ slightly (audit reads the whole lock file, deny the resolved
  graph for the listed targets).

### `deny.toml`
- **Licences:** MIT, Apache-2.0 (also WITH LLVM-exception), BSD-2/3-Clause,
  ISC, Zlib, 0BSD, Unlicense, CC0-1.0, BSL-1.0, Unicode-3.0,
  CDLA-Permissive-2.0 (webpki-roots' certificate data) and MPL-2.0. MPL-2.0
  is the only copyleft one: file-level, used by `cssparser`/`dtoa-short`
  (through ammonia's sanitiser), shipped unmodified with source on
  crates.io, which is compatible with distributing binaries under MIT OR
  Apache-2.0. GPL, LGPL-only and AGPL are not allowed.
- **Bans:** duplicate versions warn (all current duplicates are upstream:
  base64, getrandom, hashbrown, miniz_oxide, syn, winnow, and the RustCrypto
  0.10/0.11 split that sqlx 0.9 brings: sha2, hmac, digest); wildcards are
  errors; `openssl`, `openssl-sys` and `native-tls` are banned (rustls is the
  TLS stack).
- **Sources:** crates.io only; git dependencies are refused.
- **Advisories:** unmaintained crates reported for the whole tree, yanked
  versions are errors.

### Audit findings and what was done (2026-11-26)
- **RUSTSEC-2026-0459, libsqlite3-sys 0.30.1** (bundled SQLite before 3.54:
  uninitialised memory returned when a database is opened with a crafted
  `-wal` file). Fixed: sqlx 0.8.6 pins libsqlite3-sys `^0.30`, so sqlx went
  to **0.9.1** (allows `<0.41`) and libsqlite3-sys to **0.39.1** (SQLite
  3.54.1). Code changes were small and in one file
  (`crates/engine-db/src/sql.rs`): `SqliteArguments` lost its lifetime;
  query text must be a `SqlSafeStr`, so the catalogue's text is passed
  through one `sql_text()` helper using `AssertSqlSafe` (statement texts
  are constants, content-table SQL is built from validated identifiers and
  bound values, as before); `Executor::describe` is now behind sqlx's
  `offline` feature, so the catalogue check prepares statements with
  `Executor::prepare` instead (the same errors: unknown tables and columns,
  syntax, untyped parameters). Full suite green on SQLite and PostgreSQL.
- **Watch the jobs tests after the sqlx bump.** `engine-jobs/tests/queue.rs`
  is timing-sensitive. On this host (load average 20 to 30 from parallel
  work, two balanced CPUs) it failed twice in eleven runs with sqlx 0.9
  (`PoolTimedOut` in `two_workers_never_run_the_same_job`, a timeout in
  `a_crashed_job_is_retried`; once more in `TestDb::create` connecting) and
  passed 5 of 5 with sqlx 0.8. The sqlx 0.9 changelog shows no pool or
  `PgListener` change that would explain it, and the last five 0.9 runs all
  passed, so it looks like load, but the lead should run the Postgres
  suite on a quiet machine before merging. Fallback if it is real: go back
  to sqlx 0.8.6 and ignore RUSTSEC-2026-0459 with the reason "local attack
  only; the engine opens only its own site database and WAL, which an
  attacker able to write them could replace outright".
- **Minimum Rust raised from 1.85 to 1.94** (`rust-version`): required by
  sqlx 0.9.1 (libsqlite3-sys 0.39 alone needs 1.88). CI uses current
  stable.
- **RUSTSEC-2023-0071, rsa 0.9 (Marvin attack)** was in the lock file
  through sqlx-mysql, which Wrought never compiles; it disappeared with
  sqlx 0.9.
- **RUSTSEC-2026-0385, ctutils 0.4.2** (non-constant-time comparisons;
  reached through sqlx-postgres 0.9's hkdf/hmac): updated to 0.4.8.
- **RUSTSEC-2026-0543, chrono unmaintained** (informational, no
  vulnerability): **ignored**, in `.cargo/audit.toml` and `deny.toml`, with
  the reason. chrono is engine-db's `Timestamp` and sqlx's `chrono` feature,
  used across crates; moving to `jiff` or `time` (both supported by sqlx)
  is a change of its own for a later milestone, not a dependency bump.
  Review at every release.
- **For the lead (merge):** other v0.6 branches were written against sqlx
  0.8. Code that goes through `engine_db::Query` and `#[derive(FromRow)]` is
  unaffected; direct `sqlx::query(...)` calls with a non-`'static` string
  need `sqlx::AssertSqlSafe(...)`, and `SqliteArguments<'q>` is now
  `SqliteArguments`. Run `cargo audit` and `cargo deny check` again after
  merging (new crates for s3/oidc/payments bring their own trees).

### Release signing: SSH signatures, key from a CI secret
- **Decision:** `scripts/release.sh` signs each archive and `SHA256SUMS`
  with `ssh-keygen -Y sign` (namespace and principal `wrought-release`),
  verifies its own output against `release/allowed_signers`, and refuses a
  real run when that file lists no key. The private key comes from the
  environment (`WROUGHT_SIGNING_KEY`, CI secret `RELEASE_SIGNING_KEY`) and is
  written to a 0600 file in a temporary folder removed on exit. `--dry-run`
  signs with an ed25519 key made in a temporary folder for the run and
  marks the output `DRY-RUN`. No key was generated or committed;
  `release/allowed_signers` holds only instructions for the owner.
- **Why:** verification needs only OpenSSH 8.1+, already on every Linux
  and macOS machine; no extra tool in CI or for users; ed25519 keys are
  small and easy to publish next to the code.
- **Rejected:** minisign (good, but one more tool for every user and for CI
  images); sigstore keyless signing (needs an OIDC identity from the CI
  provider, which Forgejo Actions does not offer; it would work only on the
  GitHub mirror); GPG (heavy tooling, key-server trust model, the usual
  source of verification mistakes).
- **Builds:** presets static, cms, app for x86_64 and aarch64 Linux (gnu),
  aarch64 cross-compiled with `gcc-aarch64-linux-gnu` (needed for the
  bundled SQLite and ring's C code). Built on ubuntu-22.04 for an older
  glibc (2.35). Archives are reproducible in form (sorted, fixed owners,
  `SOURCE_DATE_EPOCH` from the last commit, `gzip -n`). musl static
  binaries were not done (would need a musl cross toolchain for the C
  parts); a later option.
- **Publishing is opt-in:** the release workflow keeps the files as a
  workflow artifact; only with the variable `WROUGHT_PUBLISH_RELEASE=true`
  does it create a *draft* GitHub release or upload to the Forgejo release
  (as a pre-release). A tag must match the workspace version.

### Project files
- **SECURITY.md:** latest 0.x supported only (no backports before 1.0);
  private report by email to a placeholder the owner fills in; 5 working
  days to acknowledge, 14 to assess, 90-day publication target. Forgejo and
  Codeberg have no private security advisories or confidential issues
  (nothing in the Forgejo or Codeberg docs as of 2026-11-26; Forgejo itself
  takes reports by encrypted email), so they are not mentioned; GitHub's
  private vulnerability reporting on the mirror is not mentioned either, so
  reports stay in one channel.
- **CONTRIBUTING.md:** Codeberg is where merges happen (GitHub mirror is
  read-only); labels `crate: <name>` per crate plus `good first issue` and
  `help wanted`; the project rules (feature discipline, statement
  catalogue, security baseline, components, DECISIONS log, dependencies).
- **No DCO, no CLA:** contributions are inbound = outbound under MIT OR
  Apache-2.0 (the Rust project's convention and Apache-2.0 section 5).
  **Rejected:** DCO sign-off (friction for small fixes, little gain for a
  permissive project with one owner); a CLA (would allow relicensing and
  discourages contributors).
- **GOVERNANCE.md:** BDFL now; a maintainer team once at least two other
  people each have ten or more non-trivial merged pull requests over six
  months; the steps and the team's rules (one approval for everyday
  changes, a week of lazy consensus for significant ones, the signing key
  held by at least two maintainers).
- **Architecture doc in the docs site** (`docs/content/docs/architecture.md`,
  weight 50) with a short root `ARCHITECTURE.md` pointer: the docs site is
  built and link-checked with the engine, and readers of the site get it
  too. The planned v0.6 crate names (`engine-s3`, `engine-oidc`,
  `engine-payments`) are listed with a note for the lead to check. Docs
  pages `releases.md` (51) and `project.md` (52) point to the project files.
- **CHANGELOG.md** (Keep a Changelog) backfilled for 0.1–0.5 from the commit
  messages and DECISIONS.md; the 0.6 section lists the milestone's items in
  a block marked for the lead to finalise.

### Name check (read-only, 2026-11-26; nothing registered or claimed)
- **crates.io `wrought`: taken** (created 2021-01-12, "A Rust implementation
  of the Wrought compiler", github.com/wrought-lang/wrought-rs, last
  version 0.1.2 from 2021-01-15).
- **crates.io `bloomery`: taken** (created 2026-09-30, "A lightweight,
  minimalist build system for Java written in Rust", 20 versions, latest
  2.2.0 on 2026-10-22).
- `wrought-engine`, `wrought-cli` and `kovir-wrought` were free on
  crates.io.
- **Codeberg:** no user or organisation named `wrought` or `bloomery`
  (`/api/v1/users/<name>` and `/api/v1/orgs/<name>` return 404), and
  `kovirlabs` does not exist yet either, so the spec's planned home
  `codeberg.org/kovirlabs/wrought` is not set up (404). The repository
  search (`/api/v1/repos/search`) answered 429 (Codeberg throttling search)
  on two attempts, so repositories named `wrought`/`bloomery` under other
  owners were not checked.
- **Consequence for the open question:** the crate names `wrought` and
  `bloomery` are not available; the engine's crates keep their neutral
  `engine-*` names (v0.1 decision), and a published crate would need
  another name (for example `wrought-engine`, free today). The Codeberg
  organisation `kovirlabs` is free to create. Owner to decide.
