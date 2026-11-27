# Decisions

A dated log of significant decisions: what was decided, why, and what was
rejected. Newest milestone last. Entries for v0.1 were backfilled from the code
and README at the start of v0.2.

## v0.1 "static" (2026-11-26, backfilled)

### Front matter is TOML between `+++` lines
- **Why:** TOML is already the config language (`site.toml`, type files), so
  one syntax and one parser (`toml`) cover everything; `+++` is the Zola/Hugo
  convention for TOML front matter. Unknown keys are errors so typos surface.
- **Rejected:** YAML `---` (second parser, implicit typing surprises; the
  engine rejects it with a clear message); JSON front matter.

### Build mode reads Markdown files only
- **Why:** answers the spec's open question for v0.1: `build` needs no
  database, so the static preset stays a single binary with no services.
  Database content arrives with the `content` feature.
- **Rejected:** reading the database in build mode from day one (would force a
  DB dependency into the static preset).

### Default listen address 127.0.0.1:4400
- **Why:** localhost-only is the safe default; exposing the server needs an
  explicit `--host` (and prints a warning). 4400 avoids common dev ports
  (3000, 4000, 5173, 8000, 8080).
- **Rejected:** 0.0.0.0 by default; port 3000/8080.

### Atomic output swap for `engine build`
- **Why:** render into `.<output>.staging`, then replace the output folder, so
  a failed build leaves the previous site intact. Only replaces a folder an
  earlier build created (holds `.engine-manifest.json`) or an empty one.
- **Rejected:** writing in place (half-built sites on failure); deleting
  arbitrary existing folders.

### Tag stack: TagBrush 2.0.1 and TagAction 0.2.0 bundled, TagCheck 0.1.3 external
- **Why:** the spec makes the Tag stack the default and says assets ship in the
  binary and are served from the site's origin. TagCheck is a Node.js program,
  so it runs as an external tool (`WROUGHT_TAGCHECK`, `tagcheck` on PATH, or
  `npx @kovirlabs/tagcheck@0.1.3`) and is optional: `engine dev` skips it with
  a note when absent.
- **Rejected:** bundling Node/TagCheck; loading assets from a CDN.

### minijinja is the default template engine
- **Why:** answers the spec's open question: templates are editable without
  recompiling, which suits sites and themes edited by non-Rust developers.
- **Rejected:** askama as the default (compiled templates need a rebuild for
  every template edit); it remains a possible opt-in later.

### Crate and binary naming follows the spec's `engine-*` layout
- **Why:** `engine-core`, `engine-cli` and a binary called `engine` match the
  spec's project layout and CLI, so later crates (`engine-db`,
  `engine-content`, ...) slot in by name. The product name (Wrought) is not
  final (spec open question), and crate names stay neutral until it is.
- **Rejected:** `wrought-*` crate names before the name is confirmed.

### Cache tags recorded through a scoped thread-local collector
- **Why:** loaders and template functions call `tags::record` without
  threading a collector through every signature; renders are synchronous
  (minijinja), so a per-thread stack is sound. Tags were written to
  `.engine-manifest.json` but not yet consumed.

### Raw HTML in Markdown passes through in build mode
- **Why:** in build mode the content author is the site developer and is
  trusted (same as other static site generators). Flagged for v0.2: CMS content
  must be sanitised.

## v0.2 "publish" (2026-11-26)

### Feature crates behind marker features; the default build stays DB-free
- **Decision:** `engine-db`, `engine-content`, `engine-publish` are optional
  dependencies of `engine-cli`, switched on by its features `db-postgres`,
  `content`, `publish` (chain: publish → content → a db backend). Each also
  switches on an empty *marker* feature of `engine-core`, so core knows which
  render modes and route kinds exist without depending on the feature crates.
  The workspace's `default-members` are core + cli, so a plain `cargo build`
  / `cargo test` compiles no database code (`cargo tree -p engine-cli` shows
  no sqlx).
- **Why:** the spec's "code that's switched off isn't compiled"; core stays
  the dependency root.
- **Rejected:** core depending on the feature crates behind features (cycles
  and a fatter core); Cargo can't express "content needs *one of* the db
  features", so `engine-db` has a `compile_error!` naming the fix instead.

### `cms` preset = static + db-postgres + content + publish
- **Why:** the spec's cms preset is content, publish, admin, workflow, media;
  this is "as far as its features exist". Postgres is the backend because
  SQLite is v0.5 and publish relies on NOTIFY.

### Core hook: `Renderer` + `SiteExtension`
- **Decision:** builder logic was split into a reusable `Renderer` (renders
  any single route, resolves one URL path) and a `SiteExtension` trait that
  the content crate implements (template functions, expanding pattern routes
  like `/articles/{slug}/`, resolving one path). `RouteSource::Dynamic`
  carries the page value and its tags.
- **Why:** publish and render-on-miss need to render one page at a time and
  resolve a path without expanding every route.

### `Db` is an enum over backends, not a trait object
- **Decision:** `engine_db::Db` has one variant per compiled backend
  (`Postgres(PgPool)` now). Backend-specific SQL lives in `pg` modules
  reached by `match db`. Not `#[non_exhaustive]`, so adding `Sqlite` makes
  every match fail to compile until handled; callers never change.
- **Rejected:** `sqlx::Any` (loses compile-time checking, weak typing);
  `dyn` async traits (boxing everywhere for one or two backends).

### Own migration runner instead of `sqlx::migrate!`
- **Decision:** `engine_migrations(id, checksum)`; engine migrations are
  embedded per feature (`content/0001_base`, `publish/0001_tag_index`), then
  the site's `migrations/postgres/NNNN_name.sql`. One transaction per
  migration, advisory lock per run, checksum mismatch is an error.
- **Why:** sqlx's migrator wants one source; we combine embedded per-feature
  migrations with a site folder chosen at run time in one ordered history.
- **Site migrations live in `migrations/<backend>/`** so SQLite (v0.5) can
  get its own dialect without renaming files.

### Compile-time checked queries where the schema is fixed; dynamic where it is per site
- **Decision:** every query on engine tables (migrations, revisions, terms,
  item terms, tag index, change log, cursor) uses `sqlx::query!` with
  offline data committed in `.sqlx/` (26 queries), so `cargo build` needs no
  database. Queries on `content_<type>` tables are built at run time.
- **Why dynamic:** those tables and columns come from each site's
  `types/*.toml`; they don't exist when the engine is compiled. Safety:
  identifiers are validated (`[a-z][a-z0-9_]{0,39}`, reserved names refused)
  and quoted; every value is a bound parameter.
- **Regenerate:** `scripts/sqlx-prepare.sh` (scratch DB `wrought_sqlx`).
- **sqlx 0.8**, not 0.9 (just out; 0.8's API is the one the code targets).
  TLS via rustls (`tls-rustls-ring-webpki`) for managed Postgres.

### Content model: one table per type, generated migrations, snapshot diff
- **Decision:** `types/<name>.toml` with `[[fields]]` (ordered). `engine types
  sync` diffs the types against `migrations/types-snapshot.json` (committed)
  and writes one migration. Field columns are nullable (`required` is
  validated on save, so adding a required field to a table with rows works).
  Field type changes are refused (hand-written migration + `--snapshot-only`);
  drops need `--allow-drop`.
- **Rejected:** a generic JSONB `items` table (spec says the engine generates
  migrations from type files; typed columns are queryable and ordered);
  diffing against the live database (sync should work offline and be
  reviewable in a PR).
- Taxonomy is shared tables (`engine_terms`, `engine_item_terms`); terms are
  created on first use. Vocabulary names share the tag namespace with types,
  so clashes are refused. Blocks are JSONB validated against
  `types/blocks/*.toml`. Singletons (`singleton = true`) are one row (unique
  index on `(true)`) tagged `s:<type>`.

### Optimistic locking and revisions in one transaction
- **Decision:** every content row has `version`; `UPDATE ... WHERE id = $1 AND
  version = $2`, zero rows → `SaveError::Conflict { expected, current }`.
  Create/update/delete write an `engine_revisions` row (full item JSON,
  author, op) in the same transaction; a test forces the revision insert to
  fail and checks the item is unchanged. `engine_terms` also has `version`.
- **CLI default:** `engine content set` uses the version it read just before
  saving unless `--expect-version` is given (CLI is single-shot; the v0.3
  admin must carry the version from the form).

### Slugs are stable and constrained in the database too
- Slugs come from the title on create (accents folded, `-2` suffixes), never
  change on title edits, and have a CHECK constraint
  (`^[a-z0-9]+(-[a-z0-9]+)*$`, ≤ 80), so SQL edits can't put `..`, `/` or
  markup into a published path. A slug change moves the page (old file
  deleted, new one written).

### Sanitise at render time with ammonia
- **Decision:** `markdown` fields are rendered with the core Markdown renderer
  then cleaned with ammonia (allow-list; heading ids kept with a `c-` prefix
  against DOM clobbering; `rel="noopener noreferrer"`). `url` fields accept
  only http(s), mailto, `/path`, `#frag` (checked on save and again on
  render). Plain fields rely on template auto-escaping. Done when rendering,
  so rows edited directly in SQL are still safe.
- Markdown *files* still pass raw HTML (developer-authored, as in v0.1).
- **Rejected:** sanitising only on save (SQL edits bypass it); escaping all
  HTML in Markdown (editors lose links/emphasis from inline HTML).

### Only published items render
- Drafts have no pages and are invisible to `list_items`/`get_item`, including
  related items (an author must be published to appear on an article).
  `get_singleton` ignores status (settings have no draft state). Preview of
  drafts is the v0.3/workflow job.

### Template function names: `list_items`, `get_item`, `get_singleton`, `list_terms`, `render_block`
- **Why:** `items`/`item` collided with component arguments (the built-in
  breadcrumbs used `items or breadcrumbs`, and a global function is truthy);
  the chosen names match core's `get_page` / `section_pages`.

### Change log + NOTIFY as a wake-up (outbox), cursor in the database
- **Decision:** triggers on every content table (and terms, item-terms) insert
  into `engine_changes` and `pg_notify('engine_changes', id)`. The listener
  treats NOTIFY only as a wake-up and reads the log from a cursor
  (`engine_publish_state`), then prunes it. Catch-up at start, after
  reconnects, and every 60 s.
- **Why:** NOTIFY alone loses events while the listener is down; this way
  edits made with `serve` stopped, or in psql, are published later. Triggers
  (not app code) so SQL edits count.
- **Rejected:** app-emitted NOTIFY only; full rebuild on reconnect.
- After a batch the cursor advances even if some pages failed to render
  (errors reported, old files kept); otherwise a broken template would be
  retried forever. `engine rebuild` retries.

### Tag index in the database
- **Decision:** `engine_pages(path, output, sha256)` + `engine_page_tags(tag,
  path)`; written by `engine build` (with the cursor, one transaction) and on
  every publish render. Only `publish` routes are indexed: content changes
  never touch `build` routes.
- **Invalidation:** an item change → `i:<type>:<id>` + `c:<type>` (+ its new
  path if published). So pages listing the type are re-rendered even when the
  edited item isn't on them; finer collection tags are a later optimisation.

### Publish targets: trait now, disk only; S3 deferred
- **Decision:** `PublishTarget { put, delete, exists, finish_batch }`;
  `DiskTarget` writes a hidden temp file and renames it over the old one.
  `finish_batch` gets the changed URL paths (where CDN invalidation goes).
  `[publish] target = "s3"` (and bucket/prefix/endpoint/region) parse and are
  refused with "not in this release".
- **S3 deferred:** doing it properly means an S3 client (aws-sdk-s3 or
  rusty-s3 + HTTP), credentials from machine roles, Content-Type /
  Cache-Control / ETag headers, CDN invalidation APIs, and a MinIO test
  container — not cheap. Planned with media (v0.5) or deployment work.
- **No fsync per file:** the rename gives readers atomicity; an fsync per page
  made an 11-page publish take ~3 s on this disk. After a power loss
  `engine rebuild` regenerates every page from the database.

### Render on miss
- `engine serve` installs a `MissHandler` on the core static server: a GET for
  a missing file whose path resolves to a `publish` route (and a published
  item) is rendered, written and served; anything else gets the 404 page.
  Build routes never render on miss.

### Publisher is synchronous and blocks on the runtime handle
- minijinja renders synchronously and cache tags are collected per thread, so
  template functions call `Handle::block_on` for database queries; the
  publisher runs in `spawn_blocking`. One mutex serialises publish batches
  and miss renders.

### Database URL from `ENGINE_DATABASE_URL`, then `[database] url`
- Keeps credentials out of site folders; the example site has no URL in
  `site.toml`. Passwords are redacted in errors and Debug output.

### Local dev and test infrastructure
- `deploy/compose.postgres.yml`: container `wrought-postgres`, Postgres 17,
  127.0.0.1:55432 (5432 is taken by a system Postgres), volume
  `wrought-pgdata`, public dev-only credentials.
- Tests needing Postgres use `ENGINE_TEST_DATABASE_URL`; each creates and
  drops its own `wrought_test_*` database and returns early (skips) when the
  variable is unset. `scripts/test-postgres.sh` is the one command.
- `deploy/compose.publish.yml` + `Caddyfile.publish`: Caddy on the host
  network bound to 127.0.0.1:8791, `try_files` then `reverse_proxy` to
  `engine serve`. Mounts the *site* folder, because `engine build` replaces the
  output folder and a bind mount of it would go stale. The hidden-file rule
  is its own `handle` (Caddy orders `handle` before `respond`).

### Not in this milestone
- S3 target (above); SQLite (v0.5); pagination of database listings;
  `engine dev` doesn't run the publish listener (it rebuilds everything on
  file changes); templates/Markdown are read when `engine serve` starts
  (restart after editing them); one publishing process per site.

## v0.3 "admin" (2026-11-26)

### Security finding on v0.2: rendered HTML is a safe *value*, not a `| safe` filter
- **Finding:** `author.html` printed `{{ page.bio | safe }}` and put
  `page.website` in an `href`. Established by test
  (`crates/engine-publish/tests/xss.rs`, real example site, hostile values
  through the store and straight into SQL): both were already safe in v0.2
  (`markdown` fields are rendered and cleaned by ammonia at render time;
  `url` fields are scheme-checked on save and again on render). The audit
  of every `| safe` found a real hole next door: the built-in `page_list`
  component printed `p.summary | safe`, so a database item's plain-text
  `summary` passed to it was inserted raw (stored XSS).
- **Decision:** fix it at the engine level so templates cannot get it wrong
  by default. Engine-made HTML (Markdown `page.content`/`summary`, sanitised
  `markdown` fields and blocks, component and block output) reaches
  templates as minijinja *safe values*; `DynamicPage.page` became a
  `minijinja::Value` so the mark survives. Every `| safe` was removed from
  built-in, example and docs templates, and `engine build` / `serve` warn
  about any `| safe` in a site's templates (comments ignored).
- **Rejected:** keeping `| safe` and documenting it (the review shows how
  easily it lands on the wrong value); escaping in the component only (the
  same mistake would reappear elsewhere); a tainted-string type for all
  database text (large change, minijinja has no native support).

### Feature layout: `auth`, `roles`, `admin`; `cms` gains all three
- `engine-auth` (accounts, sessions, roles behind its `roles` feature) and
  `engine-admin` are optional crates of engine-cli. Features follow the
  spec's table: `auth` needs a db backend (same `compile_error!` as
  `content`), `roles` needs `auth`, `admin` needs `auth` + `content`. `auth`
  also switches on core's `request` marker. Each combination compiles
  (checked: auth, db-postgres+auth+roles, admin without roles, content,
  publish, cms); the default build has no sqlx, argon2 or htmx.
- **`cms` includes `roles`:** the spec lists roles under `app`, but a CMS
  with several editors and the required per-field rights needs them.
  Without `roles` every active account is an administrator (`Access::full`).
- **Rejected:** roles always on with auth (no way to build the simplest
  single-editor site lean).

### Accounts and sessions
- **Argon2id** with the `argon2` crate defaults (19 MiB, t=2, p=1, OWASP);
  hashing runs in `spawn_blocking`. Unknown accounts are checked against a
  dummy hash (same timing). Policy: 12+ characters, no composition rules
  (NIST 800-63B), not the email address.
- **Server-side sessions:** the cookie is a random 256-bit token; the
  database stores only its SHA-256. A copy of `engine_sessions` cannot sign
  anyone in. Absolute lifetime 12 h, idle timeout 120 min (`[auth]`).
  Sign-in always makes a new session (no fixation); password change/reset
  ends other sessions; blocking an account ends all of them.
- **Cookies:** `__Host-wrought_session`, `Secure; HttpOnly; SameSite=Lax;
  Path=/`. Lax, not Strict, so following a link into the admin from email
  works; CSRF tokens cover the rest. `[auth] cookie_secure = false` exists
  only for plain-http development off localhost (browsers and curl treat
  `http://localhost` as secure).
- **CSRF:** synchronizer token stored in the session row for signed-in
  forms (`_csrf`, or `X-CSRF-Token`); double-submit cookie
  (`__Host-wrought_csrf`, SameSite=Strict) for signed-out forms (sign-in,
  reset). Every POST is also refused when `Origin` names another host.
- **Rate limits in the database** (`engine_auth_events`): 5 failures per
  account and 30 per client address per 15 minutes; then even the right
  password is refused (429). Reset requests: 3 per account, 10 per address
  per hour, silently dropped beyond. In the database so limits survive
  restarts and hold across processes. Client address: the peer, or with
  `[auth] behind_proxy` the last `X-Forwarded-For` hop.
- **Password reset:** single-use token (SHA-256 stored), one hour, claimed
  atomically; the request page answers the same for unknown addresses.
  Delivery goes through the `AccountNotifier` trait; v0.3's `LogNotifier`
  prints the link to stderr. The `mail` feature (v0.4) implements the trait.
  Links use `[auth] public_url`, else `ENGINE_PUBLIC_URL`, else `[site]
  base_url` (never the request's Host header: reset-link poisoning).
- **TOTP:** RFC 6238 (SHA-1, 6 digits, 30 s, ±1 step) implemented with
  `hmac`/`sha1` (already in the tree via sqlx) rather than a TOTP crate;
  each step is accepted once (`totp_last_step`, claimed with a conditional
  UPDATE). Optional for everyone with admin access;
  `require_totp_for_admins` makes it compulsory for administrators. Secrets
  are stored unencrypted (as most apps do); encrypting them with a site key
  is a later hardening step. No QR code: the key and the `otpauth://` link
  are shown (a QR encoder would be a new dependency for a convenience).
- **No default credentials:** `engine user create` makes the first account
  (an administrator when `roles` is on). Without `--password-stdin` the
  account gets an unusable hash and a reset link is printed, so no password
  ever passes through shell history or logs.

### Roles in the database, rights as one JSON document per role
- `engine_roles(name, label, is_admin, grants jsonb, version)`; grants =
  per type (and `*` for every other type) view/create/edit/delete/publish
  plus per-field `write`/`read`/`none`, and site-wide `taxonomy` and
  `users`. A user's rights are the union of their roles (`Access`). A
  type's own entry replaces `*` within a role. In a union, a role that
  cannot write a type contributes at most `read` to its fields, so a
  view-only role never re-opens a field another role restricted.
- Seeded `admin` (is_admin, immutable) and `editor`. The roles migration
  gives every existing account `admin`, so switching `roles` on later does
  not lock anyone out.
- Enforcement is server-side in `forms::parse`: any submitted field the
  user may not write (or any key that is not a field) refuses the whole
  POST with 403, and only submitted fields change. Hidden fields are left
  out of forms, conflict screens and revision views. Only administrators
  grant admin roles or edit administrators' accounts (an account manager
  could otherwise take one over through its email), and the last active
  administrator cannot be removed or blocked (checked in the transaction).
- **Rejected:** roles in `site.toml` (developer-owned file; roles are run by
  the site's owners) and per-field rules in type files (same reason).

### Admin UI
- Server-rendered with minijinja, templates embedded in `engine-admin`
  (layered over `engine-auth`'s layout). Semantic HTML only, TagBrush and
  TagAction served from `/_engine/ui/` so the admin looks right even when a
  site sets `[ui] css = "none"`. Strict CSP (scripts from self, no inline
  script), `X-Frame-Options: DENY`, `Cache-Control: no-store`.
- **htmx 2.0.11, bundled in engine-admin** (`vendor/VENDOR.toml`), not
  4.0 (released 2026-08 with API changes; 2.x is maintained). `hx-boost` on
  the body, search-as-you-type on lists; `htmx-config` turns off eval,
  inline indicator styles and the history cache, and swaps 4xx bodies so
  the conflict page shows. Only admin builds ship it.
- Forms generated from type files; blocks are edited as nested fieldsets
  with add/remove/move buttons that re-render the form server-side (no JS
  needed). Read-only fields have no `name`.
- **Conflict screen (acceptance criterion):** a save with a stale `_version`
  gets 409 with who saved and when (from the revision log), a table of the
  fields where the editor's value and the current value differ (marking
  fields both changed, using the base revision), and the form again with
  the editor's values and the *current* version, so "Save my version" is an
  explicit, still version-checked overwrite. Delete, restore, term, account
  and role saves are version-checked too.
- Revisions: list, view, restore (writes a new version; only fields the
  user may write; slug and status unchanged). Revision `author` is the
  account's email.
- **Not done:** draft preview (spec `workflow`), recent-activity dashboard,
  bulk actions.

### `request` mode, to the extent v0.3 needs it
- `request` routes (and wildcard `/x/*` request routes) are rendered by
  `engine serve` per request, never written by build or publish. The spec
  rule is implemented: a signed-in visitor or a query string renders any
  route per request unless it has `always_static = true`; signed-in
  responses are `private, no-store` with `Vary: Cookie`. `requires =
  "auth"` needs `mode = "request"` (else config error: a published file is
  world-readable) and redirects signed-out visitors to sign-in.
- Anonymous requests without a query string skip resolution unless the path
  could be a request route (cheap check), so files stay fast.
- **No fragment cache** (v0.4). The hook is `request::render`: the renderer
  already returns each render's cache tags for a cache keyed on them.
- Caddy: `deploy/Caddyfile.publish` sends admin/account paths, POSTs,
  session-cookie and query-string requests to the engine.

### Engine paths are reserved
- `/_engine/`, and with the features `/account/{login,logout,reset,totp}/`
  and `/admin/`; site routes using them are config errors. `/account/`
  itself stays free for site pages (the spec's `/account/*` example), and
  matches Bloomery's CDN rule that sends `/admin/*` and `/account/*` to the
  server.

### v0.2 gaps closed
- **Pagination of database lists:** `[routes.list] type/per_page/order/term`
  makes `/articles/`, `/articles/page/2/`...; `listing` matches Markdown
  sections' so the `pagination` component works. Pages record `c:<type>`;
  `current_paths` adds every current list page for a changed type, so a new
  page is written, and stale ones no longer resolve and are deleted.
- **`engine serve` picks up template and Markdown edits:** a watcher reloads
  the renderer (the publisher's is now swappable) and re-renders every page;
  a broken template keeps the old files and logs the error. `site.toml`,
  `types/` and `migrations/` changes are logged as needing a restart (routes,
  tables and the database schema are fixed for a running server).

### Tests
- `crates/engine-cli/tests/admin.rs` spawns the real binary (`migrate`,
  `seed`, `build`, `user create`, `serve`) against a throwaway database and
  drives it over HTTP with separate cookie jars: the two-editor conflict,
  cookies, CSRF, cross-origin POST, per-field rights with crafted POSTs,
  revisions, pagination, rate limits, password reset, TOTP, request mode.
  It reaches `engine-db`/`engine-auth` as the package's optional
  dependencies, so the default build gets no database dev-dependency.
- A static engine has no `/admin/` or `/account/` routes (tested).

## v0.4 "app" (2026-11-26)

### Flaky database tests: readiness race and an unbounded connection budget
- **Cause found:** on a fresh volume the `postgres` image runs initdb with a
  temporary server that listens only on the Unix socket, then shuts it down
  and starts the real one. The health check (`pg_isready` over the socket)
  reported healthy during that window, so `docker compose up --wait`
  returned while the server was about to restart; anything connecting then
  failed ("the database system is shutting down", seen in the logs of a
  reproduction run), and pools retrying through the restart time out
  together. The volume had been recreated several times on the failing day
  (container create events at 02:53, 03:46, 03:57). The script also ignored
  a failing cleanup `psql` (command substitution in a `for`). Second
  contributor: nothing bounded the suite's connections (24 test threads ×
  a 5-connection pool per test database, plus `engine serve` children),
  against Postgres' default `max_connections = 100` and Docker's 64 MB
  `/dev/shm`; v0.4 adds job workers (LISTEN connections) and more
  multi-process tests, which would have hit that wall.
- **Fix:** the health check runs a real query over TCP (the temporary
  server has no TCP listener); `scripts/lib.sh` additionally waits for
  three consecutive successful queries over TCP before tests start, and
  fails loudly otherwise. Test databases are capped per process
  (`MAX_CONCURRENT = 6`, a semaphore held until cleanup or drop; override
  `ENGINE_TEST_DB_CONCURRENCY`), test pools have 3 connections, spawned
  engines get `ENGINE_DATABASE_MAX_CONNECTIONS` (new, also useful in
  production). The container runs with `max_connections=300` and
  `shm_size: 256mb`. Leftover `wrought_test_<pid>_*` databases are dropped
  only when that pid is gone, so two overlapping runs no longer kill each
  other's databases (a third way to get "everything fails at once").
- **Rejected:** retries around connects (hide the race instead of removing
  it); running tests single-threaded (slow, and hides the budget problem).

### `request` mode: whole pages per request, fragments cached
- **Decision:** a `request` page is rendered on every request (the spec's
  "render on every request"); what is cached are *fragments* a template
  marks with `fragment(template, key=None, per_user=false, **args)`, kept in
  a `moka` cache with their cache tags. Pages are not cached as a whole: a
  request page carries per-visitor data (form tokens, the user), so a page
  cache would need the same scoping rules for every byte; fragments make
  the scope explicit where the developer knows it.
- **No personal data across users, by construction:** a shared fragment is
  rendered *without* `user`, `request` or any CSRF token in its context
  (even a template bug cannot print them), and one copy serves everyone; a
  `per_user=true` fragment gets `user` (minus the token) and is keyed by the
  account id. Fragments are never cached outside request renders (build,
  publish) or in draft previews (`preview` in the context), so a cached
  part never ends up in a file or shows a draft to the public.
- **Eviction:** the publisher now broadcasts every batch's changed tags,
  even when no published page was affected (it used to skip empty batches),
  and `engine serve` evicts fragments carrying any of them: the same change
  log, NOTIFY and tags that drive `publish`. A form submission evicts
  `i:submissions:<user>` (recorded by `my_submissions()`). Template reloads
  start a fresh cache; entries also expire (`[cache] fragment_ttl_seconds`,
  600) and the cache is bounded (`max_mb`, 64). Without `publish` (no change
  feed) fragments only expire.
- Responses say `X-Wrought-Fragments: hits=N misses=M`, which makes the
  cache observable from curl and tests. Anonymous request pages are `no-cache,
  private` (they may carry a per-visitor form token).
- **Rejected:** a full-page cache keyed on path + query (wrong for pages
  with forms or anything personal); evicting by polling the database.

### `request` is a feature of its own, switched on by `auth` and `forms`
- engine-core's `request` marker now brings `moka` and the `web` module:
  cookies, origin check, submitted forms, client address, random tokens and
  the signed-out CSRF token (double-submit cookie), moved out of
  engine-auth (which re-exports them) so `forms` works without accounts.
- The App UI components and their helpers (`table_view`, `chart_svg`) are
  compiled with `request` (the spec lists them under "request routes"); the
  Accounts and Forms components come from engine-auth and engine-forms
  through a new `SiteExtension::template_dirs` hook, so a build only
  contains the components of its features. Site templates still override
  any of them. `SiteExtension` gained default methods and an `Extensions`
  combinator; content, accounts and forms extensions are rendered together
  by every command (`build`, `dev`, `serve`, `rebuild`).

### Accounts components and registration
- Login, register, password reset, profile and account menu are components
  that post to the engine's `/account/` routes, so a site shows them in its
  own layout. They need a request route (each visitor's form token); on a
  build/publish page they degrade to links. The account menu is a
  `<details>` disclosure (no JavaScript).
- **Registration** (`/account/register/`) is off unless `[auth]
  registration = "open"`, needs the `roles` feature (without roles every
  account is an administrator, so the config is refused), gives
  `default_roles` (never `admin`), has a honeypot, is limited per address
  (`max_registrations_per_address`, 5/hour) and sends a `welcome` mail. It
  says when an address is taken (account enumeration is inherent to open
  registration). No email verification yet (noted for later).
- `/account/profile/` lets any signed-in user change name and password.
  Members without admin rights land on the site after sign-in, not on a 403
  in the admin.

### TOTP secrets encrypted at rest, QR codes drawn on the server
- **Decision:** ChaCha20-Poly1305 (`chacha20poly1305`), key derived with
  HMAC-SHA256 from `ENGINE_SECRET_KEY` (32+ characters) or the file named by
  `[auth] secret_key_file`; stored as `v1:` + base64(nonce ‖ ciphertext),
  with the account id as associated data (a secret copied to another row
  does not decrypt). Without a key: turning TOTP on is refused with a
  message saying what to set, `require_totp_for_admins` is a config error,
  and `engine serve` refuses to start while any account has TOTP on. Secrets
  stored by v0.3 are encrypted when the server starts with a key.
- QR codes: the `qrcode` crate renders an inline SVG of the `otpauth://` URI
  in the profile page; the secret never leaves the server.
- **Rejected:** a key in `site.toml` (ends up in the repository); keeping the
  key in the database (a dump would hold both); AES-GCM (equally fine;
  ChaCha20 needs no hardware support for constant time).

### `jobs`: Postgres queue with leases
- `engine_jobs` + `engine_job_schedules`. Claim = one `UPDATE ... WHERE id =
  (SELECT ... FOR UPDATE SKIP LOCKED LIMIT 1)`, only for kinds the worker
  has handlers for. A claimed job has a lease (`locked_until`) renewed by a
  heartbeat; an expired lease means a dead worker and the job is queued
  again (counts as an attempt). Failures back off exponentially with
  jitter until `max_attempts`, then `dead` (kept; `engine jobs retry`).
  Shutdown stops claiming, waits `shutdown_grace_seconds`, then releases
  unfinished jobs without counting the attempt. NOTIFY on insert wakes
  workers; they also poll. Schedules (`[[jobs.schedule]] every = "1h"`) are
  claimed with `SKIP LOCKED` too, so several processes enqueue once.
  `engine.cleanup` (hourly) prunes finished jobs and form events.
- Delivery is at least once (a worker cut off from the database can have its
  job re-run after the lease); documented, handlers should be idempotent.
- Workers run inside `engine serve` (`--no-workers` to turn off) or alone
  (`engine jobs work`). Tests prove two workers never run the same job (300
  jobs, two pools) and that a job whose worker's runtime is torn down
  mid-job is retried and finishes.
- **Rejected:** an external queue (Redis, RabbitMQ: a second service, against
  "Postgres or SQLite only"); a crate such as `apalis` (we need the lease
  and claim semantics spelled out and tested, and few dependencies).

### `mail`: render at enqueue time, send from a job, scrub after delivery
- `Mailer::send` renders `templates/mail/<name>.txt` (blocks `subject` and
  `body`) and an optional `.html` at once (template errors surface in the
  request that caused them) and queues `mail.send`. SMTP via `lettre`
  (rustls, connection pool built lazily inside the runtime). The subject is
  forced onto one line. After delivery the job's payload is replaced with
  `{}`, so reset links do not stay in the database.
- `ENGINE_SMTP_URL` (or `[mail] smtp_url`) and `[mail] from`; without them
  the engine says so at start, account messages go to the log, and queued
  mail waits (jobs fail and retry).
- `AccountNotifier` became asynchronous and generic: a `Message` is a
  template name plus data; `MailNotifier` (engine-mail, `auth` feature of
  the crate) queues them; `LogNotifier` logs them. Password reset, welcome
  and workflow notices all go through it.
- **Rejected:** rendering in the worker (a broken template would only show
  in the dead-job list); sending inline in the request (slow SMTP would slow
  the page; failures would be lost).

### `forms`
- Forms are TOML files (`forms/<name>.toml`, the same shape as admin-made
  forms stored as JSON in `engine_forms`, version-checked); a file wins over
  an admin form of the same name. Built-in `contact` and `newsletter` forms
  back the components when a site has none. Field types: text, email,
  textarea, number, select, radio, checkbox, date, url, tel; server-side
  validation (length, range, choices, shapes); errors come back on the
  same page with values kept and `aria-invalid`.
- **Posting:** `POST /_engine/forms/<name>/`; a refused post re-renders the
  form's page (from `_page`, through request mode, with `form_state`) with
  422/429; a good one redirects to `redirect` or `?sent=<name>`. Forms need
  request pages: `form_view()` refuses to render elsewhere because each
  visitor needs their own token (a published file cannot carry one).
- **Spam guard, no third parties:** honeypot (answered like success, nothing
  stored), time trap (an HMAC-signed stamp of when the form was shown, key
  made once and kept in `engine_keys`; too fast, forged or older than
  `max_age_hours` is refused), rate limit per form and address
  (`engine_form_events`). Every refusal is logged with its reason.
- **Multi-step:** each step posts; earlier answers travel as hidden inputs;
  the last step validates everything again (hidden inputs can be edited).
  No server-side draft storage.
- Receipts and notifications are a hook the CLI implements with `mail`, so
  `forms` does not depend on `mail` (spec: forms needs only `db`).
- Admin (features `admin` + `forms`, the `forms` right): forms list,
  submissions with paging, view, delete, CSV export (formula injection
  defused), and a form builder (fields added, removed and moved with
  server-side buttons, like blocks). File upload waits for `media` (v0.5).

### `workflow`: review state beside the item, not in its table
- An item under review is a draft plus a row in `engine_workflow`; history
  in `engine_workflow_log`. That needs no change to the generated content
  tables (whose `status` CHECK is `draft`/`published`) and no migration of
  existing sites. Editors who may edit but not publish send drafts for
  review; reviewers (publish right) preview, approve (a version-checked
  save of the status) or send back with a note; both sides are notified
  through `AccountNotifier`. `/admin/review/` lists what waits.
- **Preview** renders the draft through the site's own template for its
  type (`ContentSite::preview_page` + `Renderer::render_dynamic`), as a
  request render with `preview = true` (no fragment caching), `noindex`,
  `no-store`, and a banner.
- `cms` now includes `workflow` (the spec's cms preset lists it).

### Presets and features
- `app` = `cms` + `forms`, `jobs`, `mail` (`api` joins in v0.5). `engine new
  --preset app` copies `examples/small-app`. `scripts/check-features.sh`
  compiles every allowed combination with `-D warnings` and checks the
  default build has no sqlx; `sqlx-prepare.sh` runs it so the offline query
  data covers every combination (v0.3's data missed the queries compiled
  only without `roles`).

### Not in this milestone (for v0.5)
- File upload fields in forms (need `media`); email verification for
  registrations; TOTP for non-admin members (the site profile page has
  none); a jobs screen in the admin (`engine jobs list/retry` cover it);
  per-fragment TTLs; cron-style schedules (`every` intervals only).
- SQLite: jobs need `SKIP LOCKED` and NOTIFY, so `jobs`/`mail` stay
  Postgres-only (the spec says so); forms, accounts and workflow use plain
  SQL that SQLite can run but every query is `query!` against Postgres
  today, behind `match Db`.
- The fragment cache is per process; several `engine serve` processes each
  evict from the shared change feed, which is fine, but there is no shared
  cache.

## v0.5 "polish" (2026-11-26)

### Queries: one statement catalogue checked against both backends, instead of `query!`
- **Problem:** v0.2–v0.4 wrote every engine query as `sqlx::query!` against
  Postgres, behind `match Db`. The macro checks one database type per build,
  so SQLite would have meant a second copy of every query, a second offline
  data set and a prepare step running the whole feature matrix twice (the
  Postgres one alone took an hour).
- **Decision:** each crate declares its statements in a `sql` module with
  `engine_db::statements!`: one text that both backends accept (`$1`-style
  parameters work in SQLite too), or `{ postgres, sqlite }` where the
  dialects differ (array parameters: `= ANY($1)` vs `IN (SELECT value FROM
  json_each($1))`; `GREATEST` vs `max`), or `{ postgres }` alone for the
  Postgres-only features (jobs). Queries run through `engine_db::Query`
  with typed `Arg`s and `#[derive(sqlx::FromRow)]` rows; the backend is
  picked at run time by the `Db` or `Tx`. The **catalogue test**
  (`crates/engine-cli/tests/queries.rs`, `scripts/check-queries.sh`)
  prepares every statement (139 now; 121 exist on SQLite) against a fully
  migrated database of each backend and fails on unknown tables or columns,
  syntax and dialect slips, untyped parameters, duplicate names; a second
  test fails if a statement outside jobs has no SQLite text. Column types are
  covered by the whole suite, which runs on both backends.
- **Trade-off against the spec's "compile-time checked queries":** errors
  that `query!` caught at `cargo build` are now caught by a test that runs
  in seconds (~0.3 s on SQLite, ~2 s on Postgres after the build), and
  result-column types are checked when rows decode in tests rather than at
  compile time. In return: one SQL text per statement for two backends, no
  `DATABASE_URL` and no `.sqlx/` for building, and the hour-long prepare
  step is gone (`.sqlx/` and `scripts/sqlx-prepare.sh` removed; sqlx's
  `derive` feature replaces `macros`). The catalogue found three real
  problems on its first run (parameters Postgres could not type in
  `$2 IS NULL`; fixed with `CAST($2 AS TEXT)`, which both accept).
- **Rejected:** `query!` twice (duplicated SQL, two offline data sets, two
  prepare runs); `sqlx::Any` (no timestamps or JSON, weak typing); an ORM or
  query builder (Diesel, SeaORM, sea-query: a large dependency and a second
  language for SQL that is mostly portable already); keeping `query!` for
  Postgres plus runtime queries for SQLite (two code paths to keep in step).

### `Db` is a struct over a pool enum, with a change signal; times are bound
- `engine_db::Db` holds the pool (`Postgres` or `Sqlite`, both may be
  compiled; the URL picks), the database file for SQLite, and a
  `tokio::sync::watch` change signal. `Tx` (`BEGIN IMMEDIATE` on SQLite, so
  a transaction that reads then writes never fails with `SQLITE_BUSY` on
  the lock upgrade) and `Conn` are the other executors.
- `Timestamp` wraps `chrono::DateTime<Utc>`: `timestamptz` on Postgres,
  ISO-8601 text with exactly three fraction digits on SQLite (the same
  format the SQLite migrations' `DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ',
  'now'))` writes), so text order is time order. Statements bind the time
  (`at > $3` with `now - 15 min`) instead of calling `now()`, so one text
  works on both. Public structs keep their `String` times
  (`YYYY-MM-DDTHH:MM:SSZ`), so templates and the admin did not change.
- SQLite connections: WAL, `synchronous = NORMAL`, foreign keys on, 15 s
  busy timeout; relative paths in `sqlite://data/site.db` resolve against
  the site folder; in-memory databases are refused (the engine runs several
  processes on one site).

### SQLite parity
- Every feature the spec gives "needs `db`" runs on SQLite: content,
  publish, auth, roles, admin, workflow, forms, media, search, api. Each has
  a `migrations/sqlite/` twin of its Postgres migration: `INTEGER PRIMARY
  KEY AUTOINCREMENT` (change-log ids must never be reused: the publisher's
  cursor), `GLOB` checks for slugs and names (no regular expressions),
  `json_valid` checks, triggers per event instead of plpgsql functions, a
  `BEFORE INSERT` trigger for singletons.
- `engine types sync` writes one migration per backend from the same
  snapshot; a backend folder with no migrations gets the whole schema
  (sites made before 0.5 gain SQLite migrations on their next sync).
- **Change notification without NOTIFY:** the store signals `Db` after every
  committed write (instant for edits through this server), a `notify` watch
  on the database file and its `-wal` catches other processes (CLI, sqlite3
  shell), and a 2 s poll of `max(id)` vs the cursor covers file systems
  where watching fails. Each is only a wake-up; the change log and cursor
  stay the source of truth, as on Postgres.
- **Jobs and mail stay Postgres-only** (spec). An `app` binary on SQLite runs
  without them (account messages are logged); a site whose configuration
  asks for them (`[jobs]`, `[mail]`, or a form with `[receipt]`/`[notify]`)
  refuses to start with a message naming what asked and why.
- The whole test suite runs on both (`scripts/test-sqlite.sh`,
  `scripts/test-postgres.sh`); `TestDb` takes `ENGINE_TEST_DATABASE_URL=sqlite:`
  (a temporary file per test) and `TestDb::create_postgres` skips jobs/mail
  tests on SQLite.
- **Presets:** `cms` now compiles both backends plus `media` and `search`
  (the spec's cms preset has media); `app` adds `api` (spec). The bundled
  SQLite costs ~1.5 MB of binary.

### `media`
- **Type by content:** magic bytes decide; JPEG, PNG, GIF, WebP and PDF are
  accepted. Markup (SVG, HTML, XML, also UTF-16 and after a BOM or comment)
  is refused with a reason; so is everything else. **SVG is refused, not
  sanitised:** an SVG sanitiser is a large attack surface of its own, and
  raster formats cover the CMS case; a site that needs SVG icons ships them
  in `static/`.
- **Limits:** `max_mb` while reading the multipart body (and the route's
  body limit); `max_megapixels` checked from the header before decoding,
  plus the `image` crate's allocation limit.
- **Names are made by the engine:** `<public|private>/<id>/<slug>.<ext>`,
  slug from the uploaded name, extension from the sniffed type; the public
  route only serves names of that shape (`[a-z0-9-]+\.(jpg|png|gif|webp|pdf)`
  under a numeric id), so traversal has nothing to work with.
- **Metadata:** JPEG/PNG/WebP are decoded and re-encoded (EXIF orientation
  applied first), which drops EXIF/XMP/ICC/comments. GIF keeps its bytes
  (animation; GIF has no EXIF block); PDFs are kept and served as
  attachments. JPEG quality 82; the `image` crate writes WebP lossless only,
  so WebP variants are lossless (bigger; documented).
- **Variants** at upload time (eager), widths `[320, 640, 960, 1280, 1920]`
  below the original; GIF variants are PNG. Lanczos3.
- **Serving:** files live outside the output folder (`[media] dir`, default
  `media/`; the example uses `data/media`), so `engine build`'s atomic swap
  never touches them. `engine serve` answers `/media/` with `nosniff`, a
  `sandbox` CSP, a year of immutable caching and `attachment` for PDFs; the
  docs give the equivalent Caddy block. Private files (form attachments)
  sit in `private/` and are only downloaded through the admin by people who
  may read submissions.
- **Who:** anyone who may create or edit some content type
  (`Access::can_manage_media`), not a new grant: an editor who writes pages
  needs pictures, and existing roles keep working.
- **Content field `media`** (a library number, a select in the admin) and
  `media(id)` in templates; pages record `i:media:<id>`, and with `content`
  a trigger puts media changes in the change log, so a new description or a
  deletion rewrites the pages showing the file. `image` and `gallery`
  components are in core (the spec lists them under `core`) and take either
  `media()` objects or plain `src`/`srcset`.
- **Not done:** S3 storage for media (with the S3 publish target), image
  editing (crop/focus), AVIF output, deduplication.

### Form file uploads
- `type = "file"` fields (`accept = ["image", "pdf"]`, `max_mb`), needing
  `media`. The form posts `multipart/form-data`; files are sniffed first and
  their kind and size go through the normal validation as a marker value
  that only real uploads can create (posted text under a file field's name
  is dropped). Files are stored, privately, only when the whole submission
  is valid; a failing later file deletes the ones already stored. At most
  five files per form; not in multi-step forms (files cannot travel in
  hidden inputs).

### `search`
- **Postgres:** `engine_search` with a generated `tsvector` (title weight A,
  body B) over a `lang regconfig` column, GIN index, `websearch_to_tsquery`,
  `ts_rank`, `ts_headline`. **SQLite:** FTS5 external-content table kept in
  step by triggers, `porter unicode61 remove_diacritics 2`, BM25 with title
  weight 10; visitors' words are quoted (and the last one prefixed) so FTS5
  syntax cannot error or widen a query. Highlights come back between
  `\x01`/`\x02` (control characters are stripped from indexed text), then
  the text is escaped and the markers become `<mark>`, so stored text can
  never inject markup.
- One document per *published* item; drafts never. Fed from the publisher's
  batch reports (the same change feed, so SQL and CLI edits count);
  `engine build` and `engine search reindex` rebuild it; `engine serve`
  builds it when empty. Without `publish` the index is only rebuilt by
  those commands (documented).
- **Pagefind is an external tool, run by `engine build`** when `[search]
  pagefind = true` (like TagCheck: `WROUGHT_PAGEFIND`, `pagefind` on PATH,
  or `npx pagefind@1.5.2`). Embedding it would put Pagefind's indexer (and
  its dependency tree) into every engine for one optional step of static
  sites; it ships as a Rust binary through npm, so "install it" is one
  command. The runner lives in the CLI's default build because static sites
  are exactly the ones that need it; it adds no dependency. Pagefind search
  needs JavaScript in the browser (stated in the component).
- **Listings components:** `content_list`, `filtered_list`, `tag_cloud`,
  `related_items` come with `content` (template dir of the content
  extension, new functions `term_counts()` and `related_items()`);
  `search_box`, `search_results` with `search`; `pagefind_search` in core.
  All but the last work without JavaScript; `filtered_list` filters by
  `?term=` on request routes and links to term pages on published ones.

### `api`
- `/api/v1/` JSON for content types. Tokens: `wr_` + 32 random bytes
  (base64url), only the SHA-256 stored, a 10-character prefix shown to tell
  them apart, scopes `read`/`write`/`read:<type>`/`write:<type>`, optional
  expiry, revocation; blocked accounts' tokens stop working. A token acts
  as its account: roles and per-field rights apply, narrowed by scopes.
- **Version checks are mandatory on writes:** `If-Match` (the `ETag`) or
  `"version"`; missing → 428, stale → 409 with `current_version`. Writing a
  field without write access → 403 `field_forbidden` naming the fields,
  nothing saved; hidden fields are left out of reads.
- **One error shape** `{"error": {status, code, message, ...}}` for every
  failure, including unknown API paths (`/api/{*rest}` route, so the site's
  404 page never answers an API client) and bad JSON/queries.
- **Rate limits** in memory per process (fixed one-minute windows): per
  token, and per client address for missing/bad tokens. Simple and enough
  for one process; several processes multiply the budget (documented).
- Cookies are ignored (no CSRF surface) and no CORS headers are sent.
- **Deviation:** the spec says `api` needs `auth`; it needs `auth` and
  `content` here, because it is the JSON API *for content types* and has
  nothing to serve without them.
- Routes are spelled out (`/api/v1/...`) rather than `nest`ed: axum 0.8's
  nest does not match the trailing-slash index.

### Carry-overs from v0.4
- **Email verification:** `[auth] verify_email` (default on) makes
  registered accounts follow an emailed link (single use, 48 h, SHA-256
  stored, `engine_email_verifications`) before they can sign in; signing in
  unverified sends a fresh link (at most three an hour) and says so. The
  migration marks existing accounts verified; administrators and the CLI
  make verified accounts; a password reset also proves the address.
- **TOTP for members** on `/account/profile/` (same code path as the admin
  profile: QR drawn on the server, secret sealed with the site key).
- **Jobs screen** at `/admin/jobs/` (Postgres sites, administrators only):
  counts per status, list with paging (dead first), job details, retry
  (dead jobs) and delete (anything not running). Payloads are not shown
  (queued mail holds whole messages).

### Benchmarks (`bench/`)
- **One command** (`bench/run.sh`): builds the engine (static and app
  presets), generates the same content for everyone (`data/generate.py`,
  seeded: 200 articles of ~400 words with topics, 1000 for build times),
  starts each system with Docker Compose from pinned images, loads the
  content with each system's own tools (engine seed, `wp eval-file`, `drush
  php:script`, Markdown files for Astro), measures, writes
  `bench/results/results.json`, and removes containers and volumes.
- **Equal footing:** every system runs in a container capped at 4 CPUs and
  2 GB (databases 2 CPUs, 1 GB), on its **default configuration**: no
  WordPress page-cache plugin, Drupal with its default internal page cache,
  OPcache as the official images ship it. Wrought and Astro's static output
  are served by the *same* Caddy, so the static row compares outputs, not
  servers; Wrought's own server is a separate row. Dynamic pages are search
  results with a different word per request (a wrk Lua script) so no page
  cache answers for anyone. The Wrought image is Ubuntu (same release as the
  build host, for glibc), not a scratch image: its size is reported but is
  mostly the base.
- **One core per role, pinned** (application, database, tools, load
  generator on cores 6, 7, 8, 9; overridable). Found during the first run:
  the host boots with `isolcpus` on 22 of 24 threads, so the scheduler
  never spreads a container's processes over cores; `cpus: 4` silently
  meant "one core, wherever the process started", sometimes shared with
  its database. Pinning makes the equal treatment explicit; the docs say
  it is a one-core benchmark.
- **Short load runs** (3 s warm-up, 10 s measured, 1 thread, 32
  connections, 30 s timeout so slow responses are measured rather than
  dropped) so other services on the machine are not starved; the
  numbers are indicative, not a capacity test. The docs page states the
  limits of one machine over loopback (no network latency, load generator
  on the same CPUs, small data set, one run).
- **What the first run showed and what changed:** a 1000-article build took
  10–27 s, three times Astro's; 60% of it was the `related_items` query
  (a self-join of `engine_item_terms` with the content table, 2.6 ms per
  page on SQLite). It is now two indexed lookups (terms of the item, then
  items sharing them) with drafts filtered while reading the candidates:
  ~0.5 ms, a 1000-article build 7.2 s → 4.2 s on the host. Rendering is
  still single-threaded; a parallel build is the next lever.
- **Container artefacts fixed for everyone, not tuning:** WordPress's
  WP-Cron calls the site's own URL, which is unreachable from inside its
  container, so due events were retried (with a database write) on every
  view and serialised all requests; the harness runs due events once, as a
  reachable site would. WordPress is marked a local site so application
  passwords (for the REST edit) work over plain HTTP. Drupal's page cache
  is left on (its default) and verified (`X-Drupal-Cache: HIT`).
- **Results (published in docs/content/docs/benchmarks.md):** static
  article page 12,964 req/s from `engine serve`, 3,924 via Caddy (Astro's
  output via the same Caddy 7,144; WordPress 11, Drupal 61); search 289/s
  on SQLite, 161/s on Postgres (WordPress 9, Drupal 10); 1000-article
  build 7.8 s SQLite, 13.7 s Postgres, Astro 6.4 s (Wrought loses);
  edit to published 25 ms SQLite, 140 ms Postgres, WordPress 305 ms,
  Drupal 834 ms, Astro 3.1 s; memory 19 MB (SQLite) vs 255 MB (WordPress
  with MariaDB) and 499 MB (Drupal with Postgres).
- **Not equalised, and said so:** HTML weight differs (each system's
  default markup); WordPress and Drupal have no "cold publish" (they render
  on request) and Astro has no server (no dynamic row); edit-to-published
  is an API call plus a poll of the public page for Wrought, WordPress
  (REST, application password) and Drupal (JSON:API), and a file edit plus
  `astro build` for Astro.

## v0.6 "completion" (2026-11-26)

The milestone was done in workstreams (merged into one commit); each part
below is one of them.

### Lead: carry-overs, PostgreSQL work, block JSON, askama

#### Test containers renamed to `wrought-test-*`
- **Decision:** every compose file in `deploy/` has a `wrought-test-` project
  and container name (`wrought-test-postgres`, volume `wrought-test-pgdata`,
  `wrought-test-mailpit`, `wrought-test-caddy`, ...). The benchmark keeps
  `wrought-bench-*`.
- **Why:** the owner's Bloomery stack runs as compose project `wrought`
  (`wrought-postgres-1`, `wrought-caddy-1`, volumes `wrought_pgdata`, ...).
  `wrought-postgres` and `wrought-pgdata` were one character away from it, a
  prune or `down` by name could hit the wrong one.
- **Rejected:** keeping the names and relying on care.

#### Flaky Postgres test: the `LISTEN` connections starved the pool
- **Cause:** `sqlx::postgres::PgListener::connect_with(&pool)` takes a
  connection *out of the shared pool* and keeps it for as long as it
  listens. `engine serve` has two listeners for its whole life (the
  publisher and the job workers), so two of the pool's connections were
  never available. The tests run the server with
  `ENGINE_DATABASE_MAX_CONNECTIONS=4`, leaving two for request handlers,
  publish batches (every template function that reads content takes one),
  the search indexer and four job workers (claims, heartbeats, retries of
  mail that has nowhere to go). After a save, the publish batch that
  re-renders the page queues behind them for every one of its queries; on a
  loaded machine (the suite runs many servers at once, and on this host all
  of them share one CPU) a batch could take longer than the test's 15 s
  wait, which is the failure that was seen: the restored revision's page
  was rewritten, but late. Production had the same starvation with the
  default pool of 5 (three left for everything).
- **Fix:** `Db::pg_listener()` gives each listener a one-connection pool of
  its own (same connect options; sqlx reconnects it), so the shared pool is
  whole again. Also: the test helper now prints the tail of the engine's
  log when a test fails (straight to stderr, past the harness's capture),
  so a slow batch and a lost one can be told apart from the output.
- **Rejected:** a longer wait in the test (hides it); a bigger default pool
  (still two connections short, and the cause stays).

#### Parallel `engine build`
- **Decision:** routes render on N threads (`--jobs`, `ENGINE_BUILD_JOBS`,
  default: the CPUs this process may use). Workers take the next route from
  a shared counter; results are put back in route order (manifest and
  sitemap stay identical); the first error stops the others. Plain scoped
  threads, no rayon: the work is one loop.
- **Pinning on `isolcpus` hosts:** the kernel never moves threads between
  isolated CPUs, so on the benchmark host every worker shared the one CPU
  the process started on (verified: four busy processes in a 4-CPU cpuset
  all ran on CPU 6 at 25 % each). `ENGINE_BUILD_PIN_CPUS=1` pins worker *n*
  to the *n*-th allowed CPU (Linux, `sched_setaffinity` through `libc`).
  Off by default: ordinary hosts spread threads by themselves, and pinning
  a short build is not worth surprising anyone.
- **Rejected:** rayon (a dependency for one loop); pinning by default.

#### Whole-site renders read the content once (bulk preload)
- **Decision:** `SiteExtension::begin_bulk`/`end_bulk` bracket `engine build`
  and full rebuilds. The content extension then loads every item of every
  type in one query per type (with their terms) and answers `get_item`,
  `get_singleton` and `related_items` from memory, with the same ranking as
  the SQL (shared terms, ties by newest id, `limit × 4` candidates of any
  status, then published only). Item values (rendered, sanitised Markdown
  and blocks) are made once per item, not once per page that shows it.
  `media()` memoises per file. Pages record exactly the same cache tags.
  Publish batches and request pages still read the database (they need
  current data and render few pages).
- **Why:** profiling the 1000-article build (`perf`, and wall time against
  CPU time) showed it was round trips, not rendering: the same build took
  4.3 s on SQLite and 18.6 s on Postgres on one pinned core, with the CPU
  mostly idle on Postgres. An article page made about 13 queries (author:
  row + terms; related: term ids, candidates, then each candidate's row +
  terms; cover; settings).
- **Also batched:** the tag index after a build is written with three
  statements in total (`unnest` arrays on Postgres, `json_each` on SQLite)
  instead of three per page; `attach_terms` groups term rows by item once
  (it scanned every row for every item).
- **Rejected:** a general query cache (invalidation is the hard part, and
  request pages must see current data); joining author/related into the
  page query (the template decides what a page reads; the extension cannot
  know it in advance).

#### Indexes from `engine types sync`, checked with EXPLAIN
- **Decision:** every non-singleton type gets
  `<table>_published_newest ON (COALESCE(published_at, created_at) DESC, id
  DESC) WHERE status = 'published'` (both backends support partial and
  expression indexes). The snapshot format went to 2; a format-1 snapshot
  still counts as in sync, and the next `types sync` adds the index to
  existing tables. The list filter writes the status as a literal (one of
  two enum words), so Postgres can use the partial index in cached generic
  plans too. Engine migration `content/0002_term_index` replaces the index
  on `engine_item_terms (term_id)` with `(term_id, content_type, item_id)`,
  which answers related-item and term-listing lookups from the index alone.
- **Measured (EXPLAIN (ANALYZE, BUFFERS), 1000 articles, Postgres 17):**
  page 1 of a list went from a sequential scan of the whole table plus a
  sort (1.35 ms, 209 buffers) to an index scan of 10 rows (0.07 ms, 12
  buffers). A deep page (OFFSET 500) still walks the index to the offset
  (0.55 ms, 512 buffers; the sequential plan read 209): OFFSET is cheap at
  this size, and whole-site builds no longer use it (bulk preload).
  Related items: an index-only scan on the new term index, no heap
  fetches, 0.7 ms.
- **Not added:** GIN on JSON fields (no query filters inside JSON); covering
  `INCLUDE` indexes for lists (list pages select whole rows, which templates
  use freely); keyset pagination for list routes (pages are addressed by
  number, `/articles/page/7/`, and the bulk preload removes OFFSET from
  builds); summary tables for tag counts (one query per tag-cloud render,
  not a measured cost).

#### Postgres connection settings
- Each connection keeps up to 512 prepared statements (sqlx's default 100 is
  fewer than the engine's distinct statements), one connection is opened at
  start, and Unix sockets work through the URL (`?host=/run/postgresql`),
  documented as the recommended setup when the engine and Postgres share a
  host. The search page runs its count and its page of hits concurrently.
- `synchronous_commit = off` per transaction (`Db::begin_relaxed`) only for
  job bookkeeping: claim, heartbeat, finish, fail, reap. Never for content,
  revisions, accounts or orders.
- A recommended server tuning profile is in the databases docs, for
  Bloomery's `postgres` role to apply by RAM (Bloomery itself is not
  changed here).

#### Block trees as JSON in the admin (for the visual editor)
- **Decision:** `GET /admin/api/items/{type}/{id}/` returns an item's stored
  fields (block trees as saved), version, status and which block fields the
  user may write; `PUT /admin/api/items/{type}/{id}/blocks/{field}/` with
  `{"version", "blocks"}` saves one block field with the session, the CSRF
  token in `X-CSRF-Token`, same-origin, edit and field rights, block
  validation and the version check (409 with `current_version`). Errors use
  the JSON API's shape.
- **Why:** the editor runs inside the admin with the editor's session; the
  token-based `/api/v1/` deliberately ignores cookies (no CSRF surface) and
  should stay that way.

#### `askama`: deferred, with reasons
- **Decision:** no `askama` feature in 0.6. The spec's "askama optional for
  compiled speed" stays open.
- **Why, precisely:**
  1. askama compiles templates into the binary when *the engine* is
     compiled. A site's templates are files in its own folder, read at run
     time by one generic `engine` binary; using askama for them means every
     site compiling its own binary (a site crate depending on the engine
     crates), which contradicts "a single binary" and "editable without
     recompiling" for the default path, and needs a stable library API for
     the engine that does not exist yet.
  2. askama templates take typed Rust structs. Pages here are dynamic:
     fields come from each site's `types/*.toml`, Markdown front matter and
     `[site.extra]`; templates reach them as `minijinja::Value`s. An askama
     path would need generated Rust types per site (a code generator) or a
     dynamic value type, which loses most of askama's speed and checking.
  3. The engine's template contract lives in minijinja: `component()` with
     site overrides, cache-tag recording in template functions, *safe*
     values instead of `| safe`, the fragment cache, `render_block`. An
     askama path would have to re-implement each, or silently not support
     them.
  4. The speed is not where the time goes: the 1000-article Markdown build
     renders 1111 pages in 0.3 s (0.27 ms a page, templates included); the
     database builds were bound by round trips, not rendering (see "bulk
     preload"). A compiled-template path cannot buy much.
- **When to revisit:** once there is a public `engine` library API (the
  plugin-model open question), an `askama` feature can let a site crate
  register compiled templates for named routes behind the same
  `Renderer` interface.
- **Rejected:** compiling the built-in admin/components with askama (it
  would break the "override any component from `templates/components/`"
  rule, and they are not a bottleneck).

#### One admin gate for HTML and JSON (security review finding)
- **Finding (medium, from review):** the first version of the block-tree
  JSON endpoints checked only for a session, skipping what `Admin::require`
  enforces: admin access (`can_use_admin`) and, with
  `require_totp_for_admins`, TOTP enrolment. An administrator who had not
  enrolled could use them. (Per-type and per-field checks were there, so a
  member without rights still got 403.)
- **Fix:** `Admin::gate` is the one gate (session, admin access, TOTP
  enrolment with only `/admin/profile/` open before it), returning the
  session or why not; `require` turns the reason into the HTML answer
  (sign-in redirect, 403 page, enrolment redirect) and the JSON endpoints
  into `401 unauthorized` / `403 forbidden` / `403 totp_required`.
  Per-type, per-field and CSRF checks stay on top in each handler.
- **Audit:** every other admin handler (content, taxonomy, users, roles,
  review/preview, forms builder and submissions, media, jobs, API tokens)
  already went through `require`/`require_post`; no other route in the
  engine reads the session for admin purposes.
- **Test:** `crates/engine-cli/tests/admin_gate.rs` reads every `/admin/`
  route from the admin's router source (so new routes are covered without
  editing the test), fills in parameters, and asserts each refuses no
  session, a member without admin access, a state-changing request without
  the CSRF token, and an administrator without TOTP when it is required.

### CI, supply chain, project files, release tooling

#### One set of CI workflows in `.github/workflows/`
- **Decision:** `ci.yml` and `release.yml` live in `.github/workflows/`,
  with a shared local composite action `.github/actions/rust` (installs
  Rust with rustup when the image has none, restores the cargo cache).
  Actions used are the short names `actions/checkout@v4`,
  `actions/cache@v4`, `actions/setup-node@v4`,
  `actions/upload-artifact@v4`/`download-artifact@v4`. No marketplace
  actions (dtolnay/rust-toolchain, Swatinem/rust-cache), so the workflows
  depend on as little outside the repository as possible.
- **Runner labels:** pull-request jobs use the fixed label `ubuntu-latest`;
  only the host jobs (`host.yml`, no pull-request trigger) and the release
  job take their label from a variable (`WROUGHT_DOCKER_RUNNER`,
  `WROUGHT_RELEASE_RUNNER`). See "Host-runner jobs in their own workflow" below; this replaced
  a first version where every label was a variable.
- **Why:** one file to keep in step; the runner-specific part is reduced
  to two variables.

#### Test services: the jobs call the scripts, which start the containers
- **Decision:** the Postgres and catalogue jobs run `scripts/test-postgres.sh`
  and `scripts/check-queries.sh` unchanged; the scripts start
  `wrought-test-postgres`, `wrought-test-mailpit` and whatever containers the
  s3/oidc work adds, with `docker compose`, on 127.0.0.1 ports. So the job
  needs a runner with Docker and the compose plugin whose job sees the
  host's loopback: GitHub's `ubuntu-latest`, or a self-hosted runner in
  host mode (capacity 1, since container names and ports are fixed).
  Containers are left running after the job (the scripts reuse them;
  GitHub machines are discarded), because a cleanup step on a host runner
  that is also a development machine would delete the developer's test
  volume.
- **Why:** CI and developers run the same commands; new test services need
  no workflow change.
- **Rejected:** `services:` containers in the workflow (a second
  definition of every service, and the scripts' `docker exec` readiness
  check and cleanup of stale databases would need a CI-only path);
  Docker-in-Docker (privileged runners).

#### Checker versions pinned and cached; audit and deny both run
- `cargo-audit` 0.23.0 and `cargo-deny` 0.22.1 are installed with
  `cargo install --locked --version` into `~/.cargo-tools`, cached by
  version, so a new checker release cannot fail CI by itself. `cargo audit
  --deny warnings` reads `.cargo/audit.toml`; `cargo deny --all-features
  check` reads `deny.toml`. Both run because the spec asks for both and
  they differ slightly (audit reads the whole lock file, deny the resolved
  graph for the listed targets).

#### `deny.toml`
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

#### Audit findings and what was done (2026-11-26)
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

#### Release signing: SSH signatures, key from a CI secret
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
  provider, which ties verification to one CI service); GPG (heavy tooling, key-server trust model, the usual
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
  does it create a *draft* release. A tag must match the workspace version.

#### Project files
- **SECURITY.md:** latest 0.x supported only (no backports before 1.0);
  private report by email to a placeholder the owner fills in; 5 working
  days to acknowledge, 14 to assess, 90-day publication target. GitHub's
  private vulnerability reporting is not mentioned, so reports stay in one
  channel.
- **CONTRIBUTING.md:** labels `crate: <name>` per crate plus `good first issue` and
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

#### Name check (read-only, 2026-11-26; nothing registered or claimed)
- **crates.io `wrought`: taken** (created 2021-01-12, "A Rust implementation
  of the Wrought compiler", github.com/wrought-lang/wrought-rs, last
  version 0.1.2 from 2021-01-15).
- **crates.io `bloomery`: taken** (created 2026-09-30, "A lightweight,
  minimalist build system for Java written in Rust", 20 versions, latest
  2.2.0 on 2026-10-22).
- `wrought-engine`, `wrought-cli` and `kovir-wrought` were free on
  crates.io.
- **Consequence for the open question:** the crate names `wrought` and
  `bloomery` are not available; the engine's crates keep their neutral
  `engine-*` names (v0.1 decision), and a published crate would need
  another name (for example `wrought-engine`, free today). Owner to decide.

#### Security review fixes to CI and release (2026-11-26)
- **Finding (high): the signing key could reach a run that is not a
  release.** `release.yml` also had `workflow_dispatch`, so a manual run on
  any branch received `RELEASE_SIGNING_KEY` and signed whatever that branch
  built. **Fix:** tag pushes only (`workflow_dispatch` removed), the build
  job runs only when `github.ref_type == 'tag'`, the secret is passed only
  on tags and lives in a protected `release` environment (required
  reviewers, `v*` tags only), and `scripts/release.sh` refuses
  a real signing run without `WROUGHT_RELEASE_TAG` (`--dry-run` still works
  without key or tag). **Why:** a signature must mean "a maintainer tagged
  this commit", nothing else.
- **Finding (medium): signed artefacts built from a shared cache.** The
  release job restored the `target` cache with loose `restore-keys`, and
  caches can be written by other refs, so a poisoned build cache could end
  up in signed binaries. **Fix:** the setup action got a `cache` input; the
  release job passes `"false"` (no registry or target cache at all),
  removes `target` and `dist` before building, checks out without
  persisted credentials, and builds `--locked`. **Why:** a release build is
  rare enough that a cold build costs little; reproducibility from the lock
  file and the tag is worth more. **Rejected:** a registry-only cache with an
  exact key (registry sources are checksummed by Cargo, so it would be
  safe, but it saves only download time).
- **Finding (medium): fork pull requests on the host runner.** The
  Postgres and catalogue jobs run on `WROUGHT_DOCKER_RUNNER`, a host-mode
  runner with the Docker socket, and ran for every `pull_request`, so a fork
  could run arbitrary code on that host. **Fix:** both jobs run only for
  pushes, manual runs, and pull requests whose head is a branch of this
  repository (`github.event.pull_request.head.repo.full_name ==
  github.repository`); fork pull requests get the sandboxed jobs only, and
  a maintainer pushes the branch to the repository to run the rest.
  CONTRIBUTING.md and the releases page say so, and that the host runner
  must never run release builds. **Rejected:** running the database
  services as `services:` containers for forks (see the CI decision above:
  a second definition of every service).

#### Host-runner jobs in their own workflow, without a pull-request trigger
- **Finding (high, on the previous fix):** the `if:` that kept fork pull
  requests away from the Postgres and catalogue jobs lived in `ci.yml`, a
  file the pull request itself can change; and the runner labels were
  variables, so an edited workflow could still land on the self-hosted
  host-mode runner with the Docker socket.
- **Fix:** the Docker/host jobs (PostgreSQL suite with Mailpit and the other
  test services, the catalogue on both backends) moved to
  `.github/workflows/host.yml`, triggered only by pushes to `main` and manual
  runs; it has no `pull_request` or `pull_request_target` trigger. `ci.yml`
  keeps the pull-request trigger and only jobs on the fixed label
  `ubuntu-latest` (the `WROUGHT_RUNNER` variable is gone); it gained a
  `queries-sqlite` job, so pull requests still get catalogue coverage on
  SQLite and the full SQLite suite. The a11y job stays in `ci.yml` on the
  assumption that `scripts/check-a11y.sh` needs Node.js but no Docker (lead:
  move it to `host.yml` if that changes). Host jobs check out without
  persisted credentials and use their own cache prefix.
- **What stays the owner's job (outside the repository, documented in
  CONTRIBUTING.md "CI runners" and the releases page):** sandboxed runners
  for `ubuntu-latest` (hosted, or a fresh container or ephemeral VM per job
  with no Docker socket); the host runner registered for this repository
  only, with a label only `host.yml` uses (GitHub: a runner group limited to
  that workflow, public repositories off); maintainer approval for workflow
  runs from fork pull requests; the host runner never used for releases;
  an ephemeral VM per job preferred. A fork can still edit `ci.yml` to ask
  for any label, so only these settings truly enforce the split.
- **Rejected:** PostgreSQL coverage for fork pull requests through
  `services:` containers on hosted runners (the scripts start containers
  with compose and check them with `docker exec`, so it would be a second,
  CI-only path; the SQLite suite and catalogue cover pull requests, and
  `host.yml` runs the PostgreSQL side after merge).

### S3 publish target and CDN invalidation

#### `s3` is a feature of its own, crate `engine-s3`, in the `cms` and `app` presets
- **Decision:** a new crate `engine-s3` (S3 client, SigV4, credentials,
  CDN providers, build sync, the bucket `PublishTarget`) behind the
  `engine-cli` feature `s3 = ["dep:engine-s3", "engine-core/s3"]`. It needs
  no database: a static build with `--features s3` uploads from
  `engine build` (Bloomery's "static site with no server"). `s3` joins
  `cms` (so `app`), because Bloomery's images build `--features app` and its
  bucket option must work there. The default `static` build has no HTTP
  client (`check-features.sh` now also fails on `reqwest`/`engine-s3` in
  the default tree).
- **Cost:** reqwest + hyper-util + rustls (ring, webpki roots); hyper and
  tokio were already in. About 1.5 MB on the cms/app binary.
- **Rejected:** S3 only in `app` (the cms preset is the publishing one);
  S3 inside engine-publish (static sites could not use it without the
  database crates).

#### The `PublishTarget` trait moved to engine-core
- **Decision:** `engine_core::target` (behind a non-marker feature
  `targets`, switched on by `publish` and `s3`) holds `PublishTarget`,
  `DiskTarget`, `check_rel` and `TargetKind`/`target_kind()`;
  engine-publish re-exports them, so its API is unchanged. The trait gained
  three defaulted methods: `skip_unchanged()`, `is_current(rel, body)` and
  `render_on_miss()`. `PublishConfig` now only reads `target` and refuses
  bucket settings with `target = "disk"`; engine-s3 parses the rest.
- **Why:** engine-s3 implements the trait without depending on
  engine-publish (sqlx); `targets` is not a marker, so engine-cli's
  dev-dependency on engine-s3 (for the end-to-end test) does not switch on
  the `s3` marker in a default test build.

#### SigV4 by hand (hmac + sha2), not the AWS SDK
- **Decision:** `engine_s3::sigv4` is ~100 lines, sans-IO, checked against
  the AWS test suite's `get-vanilla` vector and the S3 documentation's GET
  Object example, and for real by MinIO in the tests. HTTP is the workspace
  `reqwest` (rustls/ring). XML answers (ListObjectsV2, errors) are read by a
  30-line extractor; we never parse arbitrary XML.
- **Rejected:** `aws-sdk-s3` (hundreds of crates, aws-lc by default, a
  compile-time cost bigger than the rest of the engine, and no use for GCS
  bearer tokens or Cloud CDN); `rusty-s3` (fine, but SigV4 is small and we
  also sign CloudFront requests); `quick-xml` (one more dependency for two
  fixed document shapes).

#### Credentials: environment, then machine roles; never site.toml
- **Decision:** `[publish] credentials = "auto" | "env" | "aws-instance" |
  "gcp-metadata"`. `env`: `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/
  `AWS_SESSION_TOKEN` (or `GOOGLE_OAUTH_ACCESS_TOKEN`). `aws-instance`: ECS
  container credentials if their variables are set, else EC2 IMDSv2
  (token first). `gcp-metadata`: the GCE metadata server's service account
  token, sent as `Authorization: Bearer` to GCS's XML API (its S3 API takes
  OAuth tokens, so GCP needs no HMAC keys at all) and to Cloud CDN. `auto`:
  env if set, else GCP when the endpoint is storage.googleapis.com, else the
  AWS role. Temporary credentials are cached until five minutes before
  expiry. The metadata addresses are fixed (see the security fix below:
  no environment overrides).
- **Safety:** `Secret` prints `<redacted>` in Debug/Display; settings whose
  names look like keys (`secret`, `access_key`, `token`, `password`,
  `hmac`, `key`...) are refused with a message naming the environment
  variables, without echoing the value; `credentials` with an unknown value
  is refused without echoing it (it may be a pasted key); endpoints with a
  user, password, path or query are refused likewise.
- **Rejected:** EKS web identity (`AssumeRoleWithWebIdentity` needs STS
  calls and token files; IMDS/ECS cover Bloomery's VMs); AWS profiles and
  `~/.aws/credentials` (stored keys on disk, which the Bloomery spec rules
  out).

#### `engine build`: compare with the bucket's ETags, not a manifest
- **Decision:** render as before into the output folder, then one
  ListObjectsV2 of the prefix; a file whose MD5 equals the object's ETag is
  skipped, others are uploaded (8 at a time, `Content-MD5` sent, the
  returned ETag checked), objects with no file are deleted except under
  `media/`, then the URL paths of everything uploaded or deleted go to the
  CDN in one batch (new media excluded: new ids are never cached; new pages
  included: a CDN may hold a cached 404).
- **Why:** the bucket is the truth; a build repairs objects changed or
  removed by hand, and nothing has to be kept in step with it.
- **Rejected:** a manifest object in the bucket (drifts when the bucket is
  edited; a second source of truth); per-object HEAD (one request per file).
- **Known cost:** objects with non-MD5 ETags (multipart, SSE-KMS) are
  uploaded on every build. Header changes alone (a new `html_max_age`) are
  not detected; documented.

#### Publish mode: skip pages whose bytes did not change
- **Decision:** with a target whose `skip_unchanged()` is true (the bucket),
  the publisher compares the new SHA-256 with `engine_pages.sha256` (new
  statement `PAGE_SHA256`) for publish routes, and asks the target
  (`is_current`, the mirror file) for `build` routes; equal pages are not
  written, not invalidated, and reported in a new `PublishReport.unchanged`
  list. Their tags are still recorded. The disk target keeps writing every
  re-rendered page (unchanged behaviour, and the disk tests rely on it).
- **Effect:** editing an article's body re-renders the listings (tag
  `c:article`) but uploads and invalidates only the article page (checked
  end to end).
- **The output folder stays a mirror:** the bucket target writes the bucket
  first, then the local file, so `engine serve` can still serve the origin,
  `is_current` has something to compare with, and `index_build` hashes
  the same bytes that were uploaded.
- **No render-on-miss** with a bucket (`render_on_miss() = false`; serve
  does not install the miss handler and prints so at start).

#### CDN invalidation behind a trait; CloudFront and Cloud CDN both implemented
- **Decision:** `Cdn` trait with `none`, `log`, `file` (JSON line per batch:
  for scripts, and the recording double of the end-to-end test),
  `cloudfront` (CreateInvalidation, SigV4 `cloudfront`/`us-east-1`) and
  `google` (`urlMaps.invalidateCache`, one call per path, bearer token).
  An in-memory `RecordingCdn` serves the crate tests. `Invalidator`
  expands paths per `index_paths` (`both` default, `directory`, `file`)
  and sends one `/*` above `max_paths` (default 1000).
- **Why both:** each is ~60 lines once SigV4 and the token sources exist;
  Bloomery's AWS row uses CloudFront and its GCP row (the one written)
  Cloud CDN.
- **Tests:** against a local axum mock server (endpoint override): request
  shape, recomputed signature, IMDSv2 flow with session token, metadata
  token flow. Never a real cloud endpoint. (The mock is reached through
  test-only constructors, see the security fix below.)
- **`index_paths` default `both`:** whether a CDN's cache key is the
  directory URL or the rewritten `index.html` key depends on how the
  rewrite is done; invalidating both is always correct and the doubled
  path count is documented as a cost.

#### Media on S3: disk stays the working copy, public files are copied
- **Decision:** engine-media had no storage abstraction; it gains a minimal
  one, `PublicStorage` (`put`, `delete`), default none. The disk folder
  keeps every file (admin downloads of private files, `/media/` from
  `engine serve`, variants); public files and their copies are also put to
  `<prefix>media/<id>/...` on upload (before the database commit; a failure
  removes what was copied and fails the upload) and deleted with the file
  (and invalidated). Private files never reach the bucket, which a CDN
  serves publicly. `engine build` also uploads public media missing from the
  bucket (for sites switching to S3), and never deletes under `media/`.
- **Rejected:** moving all media storage to the bucket (private files would
  need a second, non-public bucket, and the admin and `/media/` route would
  need an S3 read path; a larger change than v0.6 needs).

#### Test infrastructure: MinIO from the Pigsty community image
- **Decision:** `deploy/compose.minio.yml` (`name: wrought-test-minio`,
  container `wrought-test-minio`, 127.0.0.1:59000, console 59001, data on
  tmpfs, public test credentials), image `pgsty/minio:RELEASE.2026-08-04T00-00-00Z`.
  MinIO Inc. no longer publishes images (`minio/minio` is gone from Docker
  Hub, quay.io needs a login); the Pigsty build is the maintained community
  one. Tests read `ENGINE_TEST_S3_ENDPOINT` (and `_ACCESS_KEY_ID`,
  `_SECRET_ACCESS_KEY`) and skip without it; each creates and destroys its
  own bucket. `scripts/test-postgres.sh` starts MinIO and passes the
  variables; `scripts/test-sqlite.sh` passes them when MinIO answers (it
  stays container-free otherwise).
- **Rejected:** a separate `scripts/test-s3.sh` (one more command to
  remember; the existing scripts already know about containers).

#### Security fix: where credentials may be sent
- **Finding (review, two MEDIUM):** the Google bearer token (a machine
  credential) was sent to whatever URL was configured: the bucket
  `endpoint` (`Auth::Bearer` in `client.rs`) and `[publish.cdn] endpoint`
  (`invalidateCache` in `cdn.rs`). A mistyped or hostile endpoint, or plain
  `http://`, would leak the token. SigV4 requests could likewise go over
  plain HTTP to any host, the CDN endpoint override was settable from
  `site.toml`, the metadata addresses from environment variables, and the
  HTTP clients followed redirects. `is_google()` matched by suffix.
- **Fix:** a new module `engine_s3::endpoint` checks every credential-
  carrying request before it is sent:
  - a bearer token only over HTTPS, default port, to a host *exactly* on an
    allow-list (`storage.googleapis.com` for the bucket,
    `compute.googleapis.com` for Cloud CDN); URLs are parsed (`url`), never
    matched by substring or suffix; URLs with user info are refused;
    anything else must use SigV4;
  - SigV4 only over HTTPS, or plain HTTP to loopback (127.0.0.0/8, `::1`,
    `localhost`) for a local MinIO; checked when `site.toml` is read and
    again per request;
  - the CloudFront API only at `https://cloudfront.amazonaws.com`;
  - `[publish.cdn] endpoint` is gone; the provider tests use
    `CloudFront::with_endpoint_for_tests` / `GoogleCdn::with_endpoint_for_tests`
    and `Credentials::with_metadata_for_tests`, which accept loopback URLs
    only and are not reachable from configuration. The metadata addresses
    (169.254.169.254, `metadata.google.internal`) are fixed; the
    `AWS_EC2_METADATA_SERVICE_ENDPOINT` / `GCE_METADATA_HOST` overrides were
    removed (`AWS_CONTAINER_CREDENTIALS_FULL_URI` stays, with the SDKs'
    HTTPS-or-local check, because ECS sets it);
  - every client that carries credentials (bucket, CDN, metadata) uses
    `redirect::Policy::none()`: a 3xx is an error, never followed;
  - `is_google()` compares the whole endpoint with
    `https://storage.googleapis.com`.
- **Tests:** a bearer token over plain HTTP, look-alike hosts
  (`storage.googleapis.com.evil.test`, another port), user info, non-loopback
  plain-HTTP SigV4 endpoints, test overrides to non-loopback hosts, and a
  307 redirect from the bucket and from the CloudFront API (the redirect
  target receives nothing) are all refused.
- **Why this shape:** the allow-lists are tiny and fixed, so exact matching
  costs nothing; refusing at the last moment before `send` covers every
  path to the network, including ones added later.
- **Rejected:** a site.toml escape hatch (`allow_insecure_endpoint`) for
  private S3 gateways over HTTP (a setting a deployment could flip by
  mistake; put TLS in front of such a gateway instead).

### Single sign-on (oidc)

#### `oidc` is a module of engine-auth behind a feature, not a crate
- **Decision:** `engine-auth/src/oidc/` with an `oidc` feature of engine-auth
  (optional `reqwest` and `ring`), switched on by engine-cli's `oidc` feature
  (`auth` + `engine-auth/oidc` + core's `oidc` marker). The settings
  (`ProviderConfig`, `[[auth.oidc]]` in `AuthConfig`) and `Identity` /
  `Auth::identities` are always compiled, without dependencies: a build
  without the feature refuses `[[auth.oidc]]` with a message naming the
  feature, and the admin lists identities without a feature of its own
  (empty list without `oidc`). Routes exist only with the feature *and* at
  least one provider. Not in any preset: it adds an HTTP client stack and
  is only useful with a provider.
- **Why:** sign-in by provider has to reach into the login page, the profile
  page, sessions, rate limits, registration rules and the Accounts
  components, all inside engine-auth; a separate crate would need hooks into
  each.
- **Rejected:** `engine-oidc` crate (hooks for every page and private
  account functions); a top-level `[oidc]` section (would need a new field
  in core's `SiteConfig`; providers belong with the other `[auth]` rules).

#### Settings in `[[auth.oidc]]`, secret only from `ENGINE_OIDC_<NAME>_CLIENT_SECRET`
- `name` (`[a-z][a-z0-9_]*`, max 32: URLs and the variable), `label`,
  `issuer`, `client_id`, `scopes` (default `openid email profile`; `openid`
  always sent), `public` (PKCE only), `create_accounts`. Two providers may
  not share a name or an issuer (identities are keyed by issuer).
- A confidential provider without its secret, or a public one with a secret
  set, makes `engine serve` refuse to start (not `build`/`check`, which
  never talk to providers). The secret has a redacting `Debug`; provider
  errors never carry codes, tokens or secrets.

#### Authorization code + PKCE (S256), state and nonce in a database row keyed by a cookie
- **Decision:** start is `POST /account/oidc/<name>/start/` with the
  signed-out double-submit CSRF token (no login CSRF via GET). It stores
  `state`, `nonce`, PKCE verifier, purpose (`login`/`link` with the account
  id) and `next` in `engine_oidc_states` under the SHA-256 of a random token
  set as `__Host-wrought_oidc` (HttpOnly, Secure, **SameSite=Lax** so it
  comes back on the provider's cross-site redirect, 10 minutes). The
  callback deletes the row as it reads it (`DELETE ... RETURNING`, single
  use even under races, expired rows ignored), compares `state` in constant
  time, checks the provider in the path (per-provider redirect URIs against
  mix-up) and an RFC 9207 `iss` parameter when present. Failures count
  towards the per-address sign-in limit.
- **Rejected:** state in a signed or encrypted cookie only (cannot be made
  single use); one shared callback URL (mix-up between providers);
  `form_post` response mode (needs a cross-site POST, which the Lax cookie
  would not accompany).

#### Redirect to the provider, and `form-action`
- The start POST answers 303 to the provider. Browsers check redirects after
  a form submission against the submitting page's CSP `form-action`, and
  engine pages say `form-action 'self'`; so the login and profile pages add
  the configured issuers' origins to `form-action`. If a provider's
  authorization endpoint is on another origin than its issuer, the start
  answers with a page and a *Continue* link instead (rare; no CSP change
  needed). Site pages using the `login_form` component get no CSP from the
  engine; the docs tell sites with their own CSP to add the issuer origin.
- **Rejected:** always an interstitial page (an extra click/flash for every
  provider); `form-action *` (gives up the protection for every form).

#### Discovery and keys: lazy, cached, refresh on unknown `kid` at most once a minute
- Discovery is fetched on first use (so the site starts when the provider is
  down) and cached an hour; its `issuer` must equal the configured one
  byte for byte; every endpoint must be https, plain http only for
  127.0.0.1, ::1 and localhost (for the local test provider). A provider
  that lists PKCE methods without S256 is refused. The JWKS is cached an
  hour; a token whose key is not in the set triggers one refetch, at most
  once per minute (counted even when the fetch fails). Responses are capped
  at 512 KiB and 10 s, redirects never followed (`reqwest`, workspace
  dependency, rustls with ring).
- Token endpoint authentication: `client_secret_basic` (default when the
  provider does not say), else `client_secret_post`; public clients send
  only `client_id`. `private_key_jwt` not supported.

#### ID token checks with `ring`, no JOSE crate
- **Decision:** a small verifier (`oidc/jwt.rs`) on `ring` 0.17 (already in
  the tree through rustls): RS256/384/512, PS256/384/512, ES256, ES384.
  `none`, `HS*` and anything else refused; `crit` refused; the key must fit
  the algorithm (`kty`, `crv`, the key's `alg` if stated, `use` sig); key
  chosen by `kid`, or without `kid` only when exactly one key fits. Claims:
  `iss` exact, `aud` contains the client id, `azp` required and equal when
  several audiences (and equal whenever present), `exp`/`iat`/`nbf` with 60
  s leeway, `nonce` constant-time equal, `sub` 1-255 characters. Email,
  `email_verified` (bool or the string `"true"`) and name are read from the
  ID token only.
- **Why:** the needed subset is ~200 lines and fully unit tested; `ring`
  avoids a second crypto stack. Tests: RFC 7515 A.2's published RSA key
  (signature vector, plus signing test tokens with it) and ECDSA P-256 keys
  generated by ring, covering bad signatures, alg confusion, `none`/HMAC,
  wrong iss/aud/azp, expiry, nbf, iat, nonce, kid selection.
- **Rejected:** `jsonwebtoken` (pulls its own crypto choices and accepts
  HMAC unless configured carefully), `openidconnect` (large dependency tree,
  its own HTTP client abstraction); fetching UserInfo (an extra request for
  data most providers put in the ID token; documented as a limit).

#### Account rules: identities by (issuer, subject); never sign into an account by email
- **Decision:** `engine_oidc_identities` keyed `UNIQUE (issuer, subject)`,
  plus `UNIQUE (user_id, issuer)` (one identity per provider per account).
  An identity that nobody linked never signs into an existing account, even
  with a verified matching email address: the owner signs in with their
  password and links the provider on `/account/profile/` (`POST
  /account/oidc/<name>/link/` with the session's CSRF token; the callback
  checks the same account is still signed in). Linking refuses an identity
  linked to someone else and a second identity of the same issuer.
- **New accounts** only with `create_accounts` (default: `[auth]
  registration = "open"`; needs `roles`, same reason as registration), only
  for a provider-verified address that no account uses, with
  `default_roles` (never `admin`, already enforced), no password
  (`unusable_hash`), address marked verified, counted against
  `max_registrations_per_address`, `welcome` message sent. A race making the
  same identity twice deletes the loser's account and signs in to the
  winner's.
- **Why:** providers differ in how well they verify addresses (some let
  users type any address), addresses get reassigned, and a provider anyone
  can register at would otherwise be a key to every account. Linking while
  signed in proves control of both sides.
- **Rejected:** auto-linking on verified email with a per-provider "trusted"
  flag (one misconfigured or compromised provider takes over any account,
  including administrators); linking by emailed confirmation (needs mail,
  which SQLite sites do not have).
- Tests: `crates/engine-auth/tests/oidc.rs` (both backends: takeover by
  email in several spellings, admin's address, unverified/missing email,
  creation with default roles, link conflicts, unlink rules, blocked
  accounts, rate limit, single-use and expiring attempts) and the
  end-to-end test below.

#### Unlinking keeps a way in; blocked accounts; sessions; TOTP
- Unlinking is refused when the account has no usable password and no other
  identity of a *configured* provider (identities of removed providers do
  not count). The account row is locked first (`UPDATE ... WHERE id`), so
  two concurrent unlinks cannot both pass the check on Postgres; SQLite's
  `BEGIN IMMEDIATE` serialises anyway.
- Blocked accounts are refused (and their sessions were already deleted);
  the per-account failure limit applies as for passwords.
- Sign-in through a provider ends the browser's existing session and starts
  a new one exactly as password sign-in does (new token, `last_login_at`,
  members without admin rights do not land in `/admin/`).
- **TOTP:** an account with two-factor sign-in on still gets the TOTP step
  after a provider sign-in. The provider replaces the password, not the
  second factor the account owner chose; `require_totp_for_admins` keeps
  working unchanged. **Rejected:** trusting the provider's MFA (`amr`/`acr`
  claims are not standardised enough to rely on).

#### Test provider: Dex v2.44.0 in `deploy/compose.oidc.yml`, started by both test scripts
- `wrought-test-oidc` on 127.0.0.1:59110 (issuer
  `http://127.0.0.1:59110/dex`), in-memory storage, three static users
  (password `password`, Dex's own example hash), a confidential client
  `wrought-test` and a public one `wrought-public`, redirect URIs for
  engines on localhost:59111-59119. The e2e test
  (`crates/engine-cli/tests/oidc.rs`) runs the engine on 59111 (Postgres)
  or 59112 (SQLite), so both suites may run at once, and drives Dex's real
  login form over HTTP. Skipped without `ENGINE_TEST_OIDC_ISSUER`.
- `scripts/lib.sh` gained `start_oidc`; `test-postgres.sh` and
  `test-sqlite.sh` call it (so the SQLite suite now needs Docker for this
  one test; `WROUGHT_NO_OIDC=1` skips it).
- **Rejected:** a mock provider written in Rust (would test our reading of
  the spec against itself); Keycloak (500+ MB image, slow start).

#### Credentials only go to checked URLs (security review, after the first merge)
- **Rule (milestone-wide):** a credential is never attached to a
  configurable URL without checks. For OIDC:
  - **https only**, plain http only to this machine (`localhost`,
    127.0.0.0/8, `::1`; widened from 127.0.0.1 alone). Enforced when
    `[[auth.oidc]]` is loaded (the issuer) and again on every request
    (discovery, keys, token endpoint, authorization URL).
  - **Endpoints on the issuer's host:** the discovery document's
    `authorization_endpoint`, `token_endpoint` and `jwks_uri` must be on
    exactly the issuer's host (case-insensitive, any port), unless the
    provider lists other hosts in its new `allowed_hosts` setting (host
    names only). A different name for the same machine (`localhost` for
    `127.0.0.1`) counts as another host.
  - **No user name or password in any URL** (issuer, endpoints), refused
    at load and per request.
  - Each URL is checked as text (`oidc::endpoint_host`/`check_endpoint`,
    always compiled, so config load can use it) and again after `reqwest`
    parsed it (`client::checked_url`), and the request is sent to that
    parsed URL, so the two parsers cannot disagree about the destination
    (backslashes, `%2f@` tricks). The token endpoint is checked
    immediately before the client secret is attached.
  - **Redirects:** the client is built with `redirect::Policy::none()`
    (as before), and a 3xx answer from the token endpoint or any GET is
    now an explicit error, so no credential follows a redirect anywhere.
- Tests (`oidc::tests`, `oidc::client::tests`): an http token endpoint on a
  non-loopback host, a token endpoint on another host, user info in
  issuer/endpoint URLs, URLs that two parsers might read differently, and
  a fake provider (a local TCP server) whose token endpoint answers 307 to
  another host: the exchange fails and the other host receives nothing.
- **Rejected:** comparing whole origins including the port (some providers
  serve the token endpoint on the same host but a different port);
  following same-host redirects (no provider needs it for these
  endpoints, and it would re-send the secret).

#### Not done
- UserInfo endpoint, logout propagation (front/back-channel), `private_key_jwt`,
  `form_post`, per-provider allowed email domains, mapping provider groups
  to roles, setting a first password from the profile page (a reset link
  does it).

### Payments and Commerce

#### `payments` is a feature of its own crate, PostgreSQL only, in no preset
- **Decision:** new crate `engine-payments`, engine-cli feature
  `payments = ["auth", "jobs", ...]` (the spec's dependencies) plus core's
  marker. `jobs` needs Postgres, so payments does too: `Payments::new`
  refuses SQLite with `engine_db::postgres_only("payments")`, and a SQLite
  site with `[payments]` is refused at start (build, dev and serve). Its
  statements are `{ postgres }` only; the catalogue test's second check
  accepts `engine_payments::` next to `engine_jobs::`. Not in `app`: a
  shop is a deliberate choice, and the fake provider and webhook routes
  should not appear in every app build. `check-features.sh` gains
  `payments`, `payments,content`, `payments,admin,db-sqlite` and
  `app,payments`.
- **Rejected:** putting it in `app` (every app binary would carry a
  checkout and a webhook endpoint); a SQLite path without jobs (applying
  events inline in the webhook request would lose the retry and lease
  guarantees the spec asks jobs for).

#### Provider trait, Stripe Checkout Sessions, and a fake provider
- **Decision:** `Provider { name, create_checkout, parse_webhook, as_any }`;
  events are normalised to `paid | failed | expired | cancelled |
  refunded` with amount, currency and order/session/payment ids. The
  shared code (webhook endpoint, replay guard, job, state machine) never
  sees provider formats. **Stripe** (the Bloomery spec names no provider;
  Stripe is the common default) via Checkout Sessions, form-encoded with
  `price_data` lines from the engine's prices, `client_reference_id` +
  metadata (also on the PaymentIntent, for refunds), `Idempotency-Key:
  checkout-<order>`; keys only from `ENGINE_STRIPE_SECRET_KEY` /
  `ENGINE_STRIPE_WEBHOOK_SECRET`, `Debug` redacted, reqwest errors
  stripped of URLs. Tested against an in-process axum mock on an ephemeral
  127.0.0.1 port and recorded event fixtures; the API address cannot be
  configured at all (see "The secret key only goes to api.stripe.com").
- **Signatures:** one implementation of the Stripe scheme (`t=`, any of
  several `v1=` HMAC-SHA256 over `t.body`, constant-time, tolerance both
  ways, default 300 s); the fake provider uses the same function with its
  own header (`Wrought-Signature`).
- **Rejected:** a provider-agnostic crate such as `async-stripe` (a very
  large generated dependency for one endpoint and one webhook parser);
  PayPal or Mollie first (no spec preference; Stripe's scheme is the one
  most others copy).

#### The secret key only goes to `api.stripe.com` (security review rule)
- **Decision:** a credential is never attached to a configurable URL
  without checks. `StripeConfig` has no address field; `Stripe::new` always
  uses `https://api.stripe.com`, and the URL is parsed and checked when the
  provider is built and again before each request: scheme `https`, host
  exactly `api.stripe.com` (so `api.stripe.com.evil.test` and
  `api.stripe.com@evil.test` fail), default port, no user name or
  password, no path, query or fragment. The mock-server override is a
  `#[cfg(test)]` constructor (`Stripe::with_mock`) that additionally
  accepts plain `http` to a loopback address only; neither `site.toml` nor
  any environment variable can reach it, so the adapter's mock tests moved
  from `tests/` into a unit-test module. The client carrying the key uses
  `redirect::Policy::none()`, and a 3xx answer is an error (the key is
  never re-sent elsewhere). The checkout URL Stripe returns, which the
  buyer is sent to, must be `https` (loopback `http` in tests).
- **Outbound callbacks:** payments makes none that carry a signature or
  secret. The fake provider signs its events and hands them to the webhook
  handling in process (no HTTP); success and cancel URLs carry no secret
  and are built from `ENGINE_PUBLIC_URL` / `[site] base_url`.
- **Tests:** refusals for a non-Stripe host, look-alike hosts, `http` to
  Stripe and to non-loopback hosts (also with the test opt-in), user info,
  other ports and paths; a 307 from the mock is not followed (the target
  records no request); an `http` checkout URL is refused.
- **Rejected:** an `ENGINE_STRIPE_API_BASE` variable for staging mocks (a
  deployment variable that redirects the live key is exactly what the rule
  forbids); following redirects only to the same host (no legitimate
  Stripe API call redirects).

#### The fake provider is guarded by an environment variable
- **Decision:** `provider = "fake"` only starts with `ENGINE_PAYMENTS_FAKE=1`
  in the environment; `engine serve` then warns at start and its pages say
  "Test payments". Its routes (`/_engine/payments/fake/checkout/<id>/` and
  `/deliveries/<id>/`) exist only while it is the configured provider. Its
  events are signed and go through `Payments::receive`, the same function
  the HTTP webhook calls (in process, so no loopback HTTP or public URL is
  needed). Deliveries (raw body + header) are kept in memory and listed as
  JSON so a replay can be done by hand. Signing secret random per process
  (`ENGINE_PAYMENTS_FAKE_SECRET` to fix it).
- **Why:** a `site.toml` is committed and deployed; a setting in it alone
  could ship a shop that "accepts" every payment. Production environments
  do not set the variable, so a forgotten `fake` fails loudly at start.
- **Rejected:** guarding on `base_url` being https or non-localhost (staging
  sites are https too, and the check would be guesswork); debug builds
  only (tests and the example run release builds too); delivering the
  fake webhook over loopback HTTP (needs a reachable own address behind
  proxies; the in-process call exercises the same verification).

#### Carts belong to accounts; checkout needs sign-in
- **Decision:** the cart is `engine_cart_lines(user_id, product, quantity)`
  (ids and quantities only, ≤ 50 lines, ≤ 99 each); shop posts from
  visitors who are not signed in redirect to sign-in. Orders belong to the
  account, which is what order history and the spec's `auth` dependency
  need.
- **Why:** no second anonymous cart cookie, no cart merging on sign-in, and
  CSRF is always the session's token. Product pages can stay published:
  anonymous visitors see "Sign in to buy"; signed-in visitors get every
  page per request (existing rule), so the add-to-cart form with their
  token appears without making shop pages `request` routes.
- **Rejected for now:** guest checkout (a cart cookie, an email field and
  order lookup by link): a later step if shops need it.

#### Products: a content type when `content` is compiled in, else a file
- **Decision:** `[payments] product_type = "<type>"` makes a content type
  the catalogue (published items only; integer `price` in minor units,
  title field as name, optional `summary`, `image`, `image_alt`,
  `available`), which gives admin editing, revisions and published product
  pages for free. Without it, `products.toml` (`products_file`) is read at
  start. The catalogue is a trait in `engine-payments`; the content adapter
  lives in engine-cli, so the crate does not depend on `engine-content` and
  `payments` still needs only `auth` + `jobs`. Content listings record
  `c:<type>` / `i:<type>:<id>`, so published grids follow product edits.
  One currency per shop (`[payments] currency`).
- **Rejected:** a payments-owned products table with its own admin
  screens (duplicates content editing); requiring `content` (against the
  spec's dependency list); per-product currencies (multi-currency pricing
  is a pricing policy, not a field).

#### Money is integer minor units end to end
- `i64` minor units in the catalogue, cart, orders (`bigint`), provider
  requests and events; checked multiplication and sums; formatting by ISO
  4217 exponent (JPY 0, GBP 2, KWD 3, a table of the exceptions) with
  symbols for GBP/USD/EUR/JPY and the code otherwise. Order lines copy
  name and price at checkout (`CHECK (amount = unit_amount * quantity)`).

#### Webhooks: verify, store with a unique event id, queue in one transaction
- **Decision:** the endpoint verifies the signature on the raw body first
  (400, nothing stored, reason logged), then inserts the event with
  `ON CONFLICT (provider, event_id) DO NOTHING` and enqueues
  `payments.apply` with `Jobs::enqueue_in` in the same transaction. A
  duplicate is a 200 no-op. Genuine events of other types are acknowledged
  and not stored.
- **Why:** answering fast keeps providers from retrying; storing and
  queuing together means an accepted event is never lost or applied
  without a record; the unique id plus the timestamp tolerance is the
  replay guard.

#### Order state machine applied by the job, with locks and version checks
- **Decision:** `pending → paid | failed | expired | cancelled | flagged`,
  `paid → refunded`; the same state again is a no-op; anything else is
  refused (event kept as `refused` with the reason, logged, order
  untouched). `paid` needs the event's amount and currency to equal the
  order's, otherwise the order becomes `flagged` (needs a person; never
  paid by mistake). Only full refunds change an order. The job locks the
  event row (`FOR UPDATE`), skips events no longer `received` (at-least-once
  delivery), and updates the order with `WHERE version = $read`; a
  conflict rolls back and the job retries. Paying clears the buyer's cart.
- **Rejected:** applying events inside the webhook request (a slow or
  failing database would make the provider retry into a half-applied
  state); `flagged` as a side flag on `pending` (a later correct event
  could then pay an order whose money was already wrong).

#### Checkout idempotency
- The checkout form carries a fresh random key (`shop_cart().checkout_key`);
  `UNIQUE (user_id, idempotency_key)` makes a second post of the same form
  find the same order (and its checkout URL); the provider gets
  `checkout-<order id>` as its idempotency key. A provider error marks the
  pending order `failed` with the reason and sends the buyer back with a
  notice; nothing was charged.

#### Commerce components
- `product_card`, `product_grid`, `cart`, `checkout_summary`,
  `order_history` in `engine-payments/site-templates/components/`, provided
  through `SiteExtension::template_dirs`, overridable by the site. One
  template each, arguments documented at the top of the file (ready for a
  manifest/versioning convention). Semantic HTML only (no classes beyond
  the existing `outline` button style, no colours): tables with captions
  and scoped headers, labelled quantity inputs, `aria-describedby` tying
  buttons to their product, `role="status"`/`role="alert"` regions for
  the post-redirect notices (`?shop=added` etc.), `<data value>` for
  amounts. Functions: `shop_products`, `shop_product`, `shop_cart`,
  `shop_orders`, `shop_order`, `money` (also a filter), global `shop`.
  Personal functions read `user` from the context, so shared fragments
  (which never get `user`) cannot show anyone's cart.

#### Admin orders screen
- Read-only `/admin/orders/` (administrators): counts per state (flagged
  first when any), paging, and an order's lines, buyer, provider ids and
  events with their results. Behind engine-admin's new `payments` feature
  (`engine-admin?/payments`). Refunds are made at the provider; its webhook
  updates the order.

#### Not done
- Guest checkout; partial refunds as a state; shipping, tax and discounts;
  stock levels; emails on payment (the `mail` hook is there: the job could
  send a receipt); other providers; S3-hosted product images (products use
  `image` paths/URLs).

#### Room for donations and bank debits (owner's "Payments and donations")
- **Decision:** v0.6 keeps its scope (provider trait, Stripe, fake
  provider), but the model no longer precludes what the spec's new
  "Payments and donations" section describes: events carry an optional
  order (they already did), and an event that matches no order is stored
  as `unmatched` instead of `refused`, so a later record type (donations,
  which arrive by webhook with no order made first) can claim it; orders
  and events gain `processing` (a bank debit submitted, waiting to settle;
  Stripe's `checkout.session.completed` with `payment_status = unpaid`)
  and `returned` (a bank debit came back after it was submitted or paid).
  Migration `payments/0002_settlement` widens the CHECK constraints.
- **Not built:** donations as a record type, other gateways (PayPal,
  Givebutter, Anedot), subscriptions, tickets (v0.9 in the roadmap).
- **Rejected:** a generic "payment record" table now (its shape depends on
  the donation fields, which are a later milestone's decision).

### Component library

#### The Tag stack covers tabs and the mobile menu: no custom element for them
- **Decision:** checked TagAction 0.2.0's source before writing scripts. It
  already implements the ARIA tabs pattern (`role="tablist"`/`tab`/
  `tabpanel`, roving `tabindex`, arrow keys, Home/End, `hidden="until-found"`
  so find-in-page and `#panel` links select a tab), disclosure dropdowns in
  the header nav (`<li><details>`: Escape, click outside, arrow keys),
  popover menus (arrow keys, close on link), and file inputs (file list,
  thumbnails, drag and drop). TagBrush 2.0.1 styles the phone menu (a
  native `popover` directly in the header's nav: links in the bar on wide
  screens, behind the button on phones) and the tab row. So `tabs`,
  `menu`, `mobile_menu` and `file_upload` emit exactly that markup and ship
  no script.
- **Mobile menu = native popover**, not a `button[aria-expanded]`
  disclosure: it needs no JavaScript at all (open, Escape, light dismiss
  and focus order are native; browsers expose the invoker's expanded
  state), browsers without popover show the links, and it is the pattern
  TagBrush lays out. A TagAction `aria-expanded` dropdown would hide the
  links on wide screens too.
- **Known limit, logged:** without JavaScript the tab buttons do nothing;
  all panels show one after another (TagAction's documented fallback), so
  no content is lost.
- **Rejected:** `<wrought-tabs>`/`<wrought-menu>` custom elements
  (duplicating TagAction, against "custom elements only for what the Tag
  stack doesn't cover"); a CSS-only `:target` tab hack (breaks history and
  the ARIA pattern).

#### One component script: `video_embed`'s click-to-load player
- **Decision:** the only behaviour the Tag stack lacks is loading a
  third-party player on request. `video_embed` renders a plain `<iframe>`
  (lazy, titled, `youtube-nocookie.com` / Vimeo `dnt=1`) by default, so it
  needs no script; with `consent=true` it renders a link to the video inside
  `<wrought-video>`, whose module adds a Play button that swaps in the
  player and focuses it. No request goes to the provider before Play.
- **Rejected:** a facade by default (a link instead of a video without
  JavaScript); embedding any URL unchecked (only `https://` players and
  site-relative video files; IDs are validated).

#### Component assets: content-hashed files, emitted by the template, deduplicated per page
- **Decision:** a manifest names its assets (`script = "x"`, `style = "y"`;
  files `components/x.js`, `y.css` next to the templates, embedded).
  `engine build` writes every compiled component's assets to
  `/_engine/components/<name>-<fnv32>.<ext>` with the other engine assets.
  The template prints them with `{{ component_assets("name") }}` exactly
  when the page needs them (so `video_embed` only with `consent`); after
  rendering a page the engine drops repeated identical tags
  (`components::dedup_assets`, in every render path of `Renderer`). Module
  scripts and stylesheet links, no inline code: strict CSP works.
- **Why emitted inline, not collected into `<head>`:** fragments of request
  pages are cached; a collector would have to store assets with each
  fragment and could lose a script when a cached fragment is reused on a
  page that did not otherwise need it. Inline tags travel with the markup;
  module scripts are deferred anyway and `<link rel=stylesheet>` is valid
  in the body. Dedup is cosmetic (browsers run a module once per URL).
- **Content hash, not the component version, in the name:** several
  components share one stylesheet (`layout`), and a hash changes exactly
  when the bytes change. FNV-1a, so names are stable across Rust versions.
- **Rejected:** one global component stylesheet in `base.html` (every page
  pays for it, and sites with their own `base.html` would lose it); inline
  `<style>`/`style=""` (blocked by a strict CSP; TagBrush's own docs use
  `style="--grid-min-column"`, the `grid` component uses `data-min`).

#### Layout CSS only where TagBrush has nothing
- `grid` and `container` are TagBrush's `.grid` and `.container`; `divider`
  is `<hr>`; `section` uses TagBrush's bands (`data-theme`). A small
  `layout.css` adds `stack`, `columns` ratios, `spacer` sizes, the grid's
  `data-min` and the reading-width container: only TagBrush length
  properties (`--spacing-*`, `--grid-gap`, `--reading-width`), no colours,
  in `@layer wrought-components` so a site's CSS wins.

#### Manifests: one TOML per component, embedded, the future block type
- **Decision:** every component (all 52, including the existing ones and
  the feature crates' Listings, Forms, Accounts and App UI) has
  `components/<name>.toml`: `label`, `version`, `group`, `features`,
  `description`, `script`/`style`, `reads` (page context it uses), typed
  `[[args]]` (types aligned with content field types plus `html`, `list`,
  `object`, `page`, `any`; `items`/`fields` for lists), `[editor]` (`icon`,
  `thumbnail`, `layout` rows, `slots`, `hidden`) and an `[example]`. A test
  checks every template has a valid manifest and every example renders.
  Sites may add `templates/components/<name>.toml` for their own
  components. `components::Catalogue` loads them for a build (the feature
  crates' folders it is given, core's last, then the site's).
- **Exposed as data:** `engine components [--json]` (no database needed:
  the CLI lists the compiled feature crates' component folders) and, with
  `admin`, `GET /admin/api/components` / `GET /admin/api/tokens` (signed-in
  users with admin rights; JSON 401/403, never a redirect; computed at
  start). Tokens come with their custom property and kind (`color`, `font`,
  `length`, `number`, `background`) for the editor's guardrails.
- `component(name, args, **kwargs)` now takes a map of arguments too (a
  block, an example); names starting with `_` are partials (`_nav.html`,
  the menu list shared by `menu`, `mobile_menu` and `header`).
- **Rejected:** manifests in Rust code (not editable by theme authors, and
  not the shape a site's own components can use); JSON Schema (heavier,
  and TOML is the project's config language).

#### A block type may name a component
- **Decision:** `types/blocks/<name>.toml` takes `component = "quote"`;
  `render_block` uses `templates/blocks/<name>.html` when the site has one
  and the component otherwise, with the block's fields as arguments.
  `Registry::load` checks the component exists and, for a built-in one the
  site does not override, that every field is one of its arguments. It
  fits cleanly because blocks and component arguments are both flat field
  maps; the admin's block forms are unchanged.
- **Rejected:** deriving the block's fields from the manifest (type files
  stay the single source of content schema and migrations; the visual
  editor milestone can revisit it).

#### Versioning policy
- Each component has its own SemVer version (all 1.0.0 in 0.6): patch for
  fixes keeping structure and arguments, minor for additions that keep
  existing pages the same, major for any change to elements, nesting,
  attributes, ids, arguments or defaults a site may rely on. Majors only in
  a release marked breaking (0.x: a new minor; from 1.0: a new major), each
  with an upgrade note in `docs/content/docs/components.md`, which records
  the 0.5 → 0.6 changes to `header` (now built on `mobile_menu`, visible
  "Menu" text, `aria-label="Main"`).

#### Docs gallery: live core groups, generated static examples for gated groups
- **Decision:** the docs are built by the default static engine. The
  gallery renders every core component live from its manifest example
  (`component_catalogue()`); the Listings, Forms, Accounts, App UI (and
  later Commerce) groups are included from `docs/templates/gallery/<group>.html`,
  generated by `crates/engine-cli/tests/gallery.rs` with the real
  templates and extensions of an all-features build (sample data in place
  of the database, the forms' time-trap stamp scrubbed). The test fails
  when a file is stale (`WROUGHT_UPDATE_GALLERY=1` regenerates). Sample
  forms for the examples live in `docs/forms/` (unused by the static
  build).
- **Rejected:** building the docs with an app engine in CI (request-only
  components still could not render in build mode, and the docs would need
  a database); hand-written static examples (they drift from the
  templates).

#### Accessibility tooling: `scripts/check-a11y.sh`
- Builds an `app` engine, copies each site into `target/a11y-work`, builds
  it (SQLite for database sites), runs TagCheck (`engine check`) on the
  output, serves it and runs axe-core 4.14 through Playwright 1.61.1
  (headless Chromium shell, installed into `target/a11y-tools`) on every
  page in light and dark schemes at 1280 and 390 px wide, against WCAG
  2.0/2.1/2.2 A and AA. The cms example's admin is crawled signed in (a
  throwaway account) and its server HTML is also given to TagCheck. Sites
  are lines in `SITES` (`name folder static|db|db+admin`), so starter
  themes are one line each.
- **small-app on SQLite** cannot run `engine serve` (it asks for mail and
  jobs, Postgres-only), so its built pages are checked and its request
  pages are covered through the same components in the gallery.

#### Found by the accessibility run: browsers could not sign in (engine-auth)
- **Finding:** driving the admin with a real browser (Playwright Chromium)
  for axe, every sign-in failed with "Form expired". Engine pages sent
  `Referrer-Policy: no-referrer`; under that policy browsers send
  `Origin: null` with a form POST, and the CSRF origin check
  (`web::same_origin`) refuses a `null` origin. The end-to-end tests use an
  HTTP client that sends no `Origin`, so they never saw it.
- **Decision:** `Referrer-Policy: same-origin` on engine pages: same-origin
  posts carry their real origin, and no address (with a reset token) is
  sent to other sites. The origin check stays strict (`null` refused).
- **Rejected:** accepting `Origin: null` (that is also what sandboxed and
  some cross-site contexts send).

### Benchmark results for 0.6, and why some runs stall
- **Builds (0.5 → 0.6, same day, same machine, one core):** 1000 articles
  on PostgreSQL 14.9 s → 2.3 s typical, SQLite 9.3 s → 1.8 s, Markdown
  files 0.7 s (Astro 6.4–7.4 s). Four cores: 2.1 s, 1.6 s, 0.5 s. Stock
  and tuned PostgreSQL and the Unix socket are within the noise for this
  test (`bench/results/pg-before.json`, `pg-after.json`).
- **Why one run in three took about twice as long:** not the engine. During
  slow runs the build used under 5 % of its core and waited on the disk;
  the host's file system was being resized (`ext4lazyinit`, I/O pressure
  25–50 %, `pg_test_fsync` 61 ms per `fdatasync`). 0.5's build committed
  each search document separately (over a thousand `fsync`s), so it
  stalled more and longer; 0.6 commits a handful of times. The same disk
  made PostgreSQL's edit-to-published about a second in the 0.6 full run
  (140 ms in 0.5's run on a quiet disk); reported as measured.
- **Rejected:** dropping slow runs or re-running until quiet (all runs are
  published, with the reason).

### Ruby on Rails in the benchmark (owner request)
- **Decision:** a fifth target, `wrought-bench-rails`: Ruby 4.0.10 and
  Rails 8.1.4.1 (the current stable releases, 2026-11-26), an application
  made by `rails new --database=postgresql` with the generator's defaults
  except the parts the benchmark does not use (JavaScript/Hotwire, Action
  Mailer/Mailbox/Text/Cable, Active Storage, Kamal/Thruster/CI/RuboCop/
  Brakeman, tests), PostgreSQL 17.12 in its own container, production
  mode (`RAILS_ENV=production`, eager loading, `assets:precompile`), Puma
  with Rails' generated `config/puma.rb` (3 threads, no forked workers:
  the default for one core), Rails' default log level (info). Same CPU and
  memory caps and core pinning as every other application, the same 200
  generated articles (`bench:seed` reads the same JSON file), the same page
  shapes (home with the latest three, a list paginated by ten, article page
  with author, topics and three related articles, topic pages, search).
- **Two configurations, both measured:** (a) plain: every view rendered on
  each request; (b) Rails caching: fragment caching with the standard
  `cache` helper and Russian-doll keys (`updated_at`, `touch: true` on the
  associations) in Rails 8's default cache store, Solid Cache (in
  PostgreSQL), plus `fresh_when` conditional GETs. `actionpack-page_caching`
  (full-page caching) is an extracted gem, no longer part of Rails, and
  Rack::Cache is not in Rails' defaults, so neither is used.
- **Only two changes to the generated production config,** in
  `config/initializers/bench.rb`: `force_ssl` off and the host allow-list
  cleared, because every system runs over plain HTTP on loopback.
- **Search:** PostgreSQL full text, written the way PostgreSQL's docs
  describe (a GIN index on the same `to_tsvector` expression the query
  uses, `websearch_to_tsquery`), not a gem.
- **Not applicable:** the 1000-article cold build (Rails has no static
  build). Edit to published is measured like WordPress and Drupal (a
  token-protected JSON update, then poll the public page): in both
  configurations the change is visible on the next request (the cache
  keys change with `updated_at`), so the number is "time until an edit is
  visible", labelled as such.
- **Also recorded for every server:** cold start (restart the container,
  time until a page answers) and memory idle and under load (sampled while
  the load runs) for Rails; memory after the load runs for all.
- **Rejected:** hand-tuning (more Puma threads or workers, a Redis or
  memory cache store, YJIT flags beyond the Ruby default): the benchmark
  compares systems as a competent developer ships them by default, as
  for WordPress and Drupal.

### For the visual editor (v0.7): what is in place
- **Block trees as JSON with the version check:** `GET
  /admin/api/items/{type}/{id}/` (stored fields, `version`, which block
  fields the user may write, the preview URL) and `PUT
  /admin/api/items/{type}/{id}/blocks/{field}/` with `{"version",
  "blocks"}` (409 with `current_version` on a stale version), behind the
  shared admin gate, CSRF header and per-field rights.
- **Block types = type file + template + editor hints:** every built-in
  component has a manifest (`components/<name>.toml`: version, group,
  required features, typed arguments, `[editor]` icon/slots, an example);
  a site block type can say `component = "<name>"` to render through one;
  the catalogue is `GET /admin/api/components` and `engine components
  --json`.
- **Design tokens as data:** `GET /admin/api/tokens` (the site's
  `[ui.tokens]`, their TagBrush properties and defaults).
- **Draft preview rendered by the engine:** `/admin/content/{type}/{id}/preview/`
  (workflow feature) renders the draft with the site's own templates,
  never cached, `noindex`.
- **Not there yet:** an endpoint that renders one block or a whole draft
  from posted JSON without saving (the editor's live preview will want
  it), per-block ids for click-to-edit mapping, and media picking over
  JSON.

## v0.7 "visual editor" (2026-11-26)

### Svelte 5 for the shell, Tiptap 3 for rich text
- **Decision:** the editor is TypeScript with Svelte 5 (runes) and Tiptap 3
  (ProseMirror) with `@tiptap/markdown`, built by Vite into one ES module
  and one stylesheet.
- **Why Svelte over Lit:** Svelte compiles to plain DOM in the light DOM,
  so the admin's TagBrush styles, focus rings and colour schemes apply
  without restyling inside shadow roots; its compiler checks accessibility
  (`svelte-check --fail-on-warnings` is part of the build and caught a
  focusable iframe); the runtime is small and there is no `eval`, so the
  admin's strict CSP is unchanged. Lit's web components would fit the
  project's custom-element habit, but shadow DOM fights a classless
  stylesheet, and the editor is one page, not a reusable widget.
- **Rich text stores Markdown:** `markdown` fields stay Markdown (the stored
  form every other path renders and sanitises), so Tiptap reads and writes
  Markdown and offers only what Markdown and the sanitiser keep (headings
  2-4, emphasis, code, links, lists, quotes). Markdown it could not keep
  (raw HTML, tables, images, footnotes) opens as Markdown text so nothing
  is lost; every field can switch to text.
- **Packages pinned to releases at least two weeks old** (svelte 5.57.3,
  vite 8.3.8, @tiptap/* 3.33.1, typescript 5.9.3 for svelte-check), exact
  versions with a lock file, installed with `--ignore-scripts`. The bundle
  is about 530 kB (170 kB gzipped), served from the admin's origin as an
  immutable file named by its content hash; only the editor page loads it.
- **Rejected:** Lit (above); a home-made contenteditable (Tiptap is the
  spec's choice and does lists, marks and undo properly); storing HTML from
  Tiptap (a second stored format beside Markdown, and the sanitiser would
  then decide what editors meant).

### The bundle is committed; CI and releases rebuild and compare it
- **Decision:** `scripts/build-editor.sh` builds `editor/` into
  `crates/engine-admin/editor-dist/` (module, stylesheet, the licences of
  every bundled package), which is committed and embedded with
  `include_bytes!`. `--check` rebuilds into a temporary folder and fails if
  it differs (the build is reproducible: verified byte for byte). CI runs it
  in its own job, a "no node" job builds the `cms` engine with Node removed
  from `PATH`, and `scripts/release.sh` runs the check before building (a
  dry run without npm only warns).
- **Why:** the spec wants the editor built at release time and a site never
  to need Node; a clean checkout must build without Node too. Building in
  `build.rs` would make Node a build dependency. Committing the output and
  proving it is the build of the committed sources gives both.
- `build.rs` in engine-admin names the files by an FNV hash of their
  content (`editor-<hash>.mjs`), since `/_engine/ui/` serves immutable
  files; `.mjs` assets are modules only their page loads (not added to
  every admin page like htmx).
- **Rejected:** building in `build.rs` (Node at compile time); downloading
  a prebuilt bundle (network at build time, and a supply-chain step);
  gating the feature on a Node build (a clean checkout of `cms` would fail).

### Live preview: validate like a save, render like a page, show in a frame
- **Decision:** `POST /admin/api/items/{type}/{id}/preview/` takes the
  unsaved block fields, runs them through `Store::validate_changes` (the
  exact validation `update` runs: block types, unknown keys, required
  values, URL schemes, token names, ids), overlays them on the stored item
  and renders it through the same `Preview` renderer as the workflow
  preview (the site's own item template, `request` mode, fragments never
  cached, nothing written). The HTML is kept in memory per user, item and
  editor tab (64 at most, 30 minutes) and shown by `GET
  /admin/editor/{type}/{id}/frame/` in a same-origin iframe; two iframes
  swap when the new page has loaded, keeping the scroll position.
- **Security:** both routes are behind `Admin::gate` (and the route
  enumeration test, which now also asserts they are found); the POST needs
  the CSRF header, the edit right and write access to each field. The
  sanitising is not a second implementation: values are stored-form JSON
  and pass through `item_value` like a published page (Markdown through
  ammonia, everything else escaped; `| safe` stays banned). The server
  fetches nothing for a preview, so no SSRF. The frame has its own CSP:
  `frame-ancestors 'self'`, `script-src 'self'`, `form-action 'none'`,
  `base-uri 'none'`, `object-src 'none'`; `no-store`, `noindex`. Tests post
  `<script>`, `javascript:` links, markup in text, colours that are not
  tokens and bad ids.
- **Why a frame URL, not `srcdoc` or `document.write`:** a navigated frame
  gets the response's own headers (CSP), scripts run as on the real page,
  and relative URLs resolve normally.
- **Block markers:** with `block_markers` in the render context (only the
  editor's frame), `render_block` wraps each block in
  `<!--wrought-block:ID-->` comments. Comments, not wrapper elements, so
  the markup and layout are exactly what ships; ids are checked
  (`[A-Za-z0-9_-]{1,40}`) so a comment cannot be closed early. The editor
  marks the elements between the comments for click-to-edit and drag.
- **Rejected:** rendering in the browser (the preview would not be the
  engine's output); replacing `<main>` with fetched HTML (scripts and
  custom elements would not re-run).

### Stable block ids, assigned by the store
- **Decision:** every block in a tree has `id`. `value::validate` keeps a
  valid unique id, and gives a missing, duplicate (a copy) or pre-0.7 one
  the first free `b<n>`: deterministic, so validating twice is a no-op. The
  editor makes random ids for new blocks; the admin form carries ids in a
  hidden input. `id` was already a reserved field name, so no block field
  can clash. A component drawing a block does not get it as its own `id`
  argument (an anchor). Search does not index it.
- **Rejected:** random ids in the store (validation would not be a pure
  function); ids by position (moving a block would change it).

### Design guardrails: `token` fields hold token names
- **Decision:** a new field type `token` with `token = "color" |
  "background" | "font" | "spacing"` (and optional `options` to narrow it).
  Values are names from `engine_core::ui::TOKEN_CHOICES`, checked on every
  save path. Templates print them as `data-color`, `data-background`,
  `data-font` or `data-padding`; `components/tokens.css` (a hashed
  component asset) maps those onto the token custom properties, and
  `render_block` adds it for blocks with token fields (`token_assets()`
  elsewhere). The editor shows swatches with the site's real values, read
  from the preview frame's computed style.
- **Why:** the spec says styling options come only from the design tokens.
  Names in data attributes need no inline style, so the strict CSP holds,
  and the site's `[ui.tokens]` stay the one place values live. Background
  choices are limited to surfaces text stays readable on; `primary` also
  switches text and links to `on_primary`.
- **Rejected:** colour pickers or free CSS values (break designs and need
  inline styles); generated classes per value (a second naming scheme).

### Block types from manifests; editor hints on block files
- **Decision:** a block type file may have `description`, `[editor]` (the
  manifest's hints: icon, thumbnail, layout) and field `default`s; one that
  names a component and has no `[[fields]]` takes them from the manifest
  (arguments a block field can hold; `html`, lists and reserved names left
  out, an error if such an argument is required). The item JSON lists each
  block field's allowed types with fields and merged hints, which is all
  the editor reads: a new block type or component appears without editor
  changes. This revisits the 0.6 decision "type files stay the single
  source": they still are for blocks that list fields.
- The `markdown` filter now passes engine-made safe HTML through: a
  block's Markdown field reaches a component as sanitised HTML, which
  `rich_text` printed as text.

### `visual-editor` in the `cms` preset
- Like `roles`, `search` and `s3` before it, it joins `cms` (and so `app`),
  because release binaries are built from the presets and the spec wants
  the editor in the binary. Off, nothing of it is compiled
  (`scripts/check-features.sh` greps an admin build for the bundle and the
  editor page; that check caught the page's template, which now lives in
  its own folder embedded only with the feature). Size: the release `cms`
  binary is 26,001,008 bytes with the editor and 25,176,688 without
  (+824,320 bytes, 3.3 %: the bundle, its licences and the Rust side). The
  draft renderer (`Preview`) moved to `engine_admin::preview`, compiled
  with `workflow` or `visual-editor`.

### Cold-start test failure: a fixed port in the ephemeral range
- **Found:** reproduced from a cold start (all `wrought-test-*` containers,
  volumes and networks removed, then the SQLite script, then the Postgres
  script, three rounds on a clean export of 0.6): one of six script runs
  failed, the OIDC end-to-end test: `cannot listen on 127.0.0.1:59112:
  Address already in use`. The test must use a port registered at Dex
  (59111 on Postgres, 59112 on SQLite), and those lie inside the kernel's
  ephemeral range (32768-60999 here), from which every outgoing connection
  and every `bind(0)` takes its local port. The suites open thousands of
  connections (to PostgreSQL, to test servers on `free_port()`, to Dex), so
  now and then one held the test's port at the moment it bound. Later runs
  pass for the same reason they usually do: it is a matter of chance.
- **Fix:** the redirect URIs and the test now use 31111-31114 (Postgres)
  and 31115-31119 (SQLite), below the ephemeral range, taking the first
  that is free. **Guard:** `common::fixed_port` refuses (panics on) any
  fixed test port inside `/proc/sys/net/ipv4/ip_local_port_range`, so a
  future fixed port in that range fails every run, not one in fifty.
- **Not proven:** the owner's failing run (exit 1 from the Postgres script)
  left no log. Exit 1, not cargo's 101, points at the script's own steps
  (container start); those did not fail in the reproduction. The port race
  is the one cold-start failure found and is fixed; the container ports
  (55432, 51025, 58025, 59000, 59110) are also in the ephemeral range and
  could in principle collide at `compose up`, which fails loudly.
- **Also:** the scripts start every test container through `compose_up`
  (scripts/lib.sh), which on failure prints the container state and logs
  and retries twice, so a slow health check on a cold, loaded machine no
  longer ends the run with a bare exit 1, and a real failure says why.

## v0.8 "small host", first steps (2026-11-26)

### The intermittent build stall: the disk's sync, and a build that waited for it
- **Symptom:** the 1000-article build on PostgreSQL took either about
  2.1–2.8 s or 9.2–11.4 s (two of three runs in the published 0.7 results),
  with nothing in between; SQLite had the same shape.
- **Method:** `engine build --timings` (new) prints the time of every
  phase. The build was run 50 times on a fresh database against the test
  PostgreSQL, with `pg_stat_activity` sampled every 50 ms and, independently
  of PostgreSQL and the engine, a probe writing 8 KiB and `fdatasync`ing it
  every 200 ms on the same file system (`bench/run.sh stall` does the same
  in the benchmark stack).
- **Finding:** 11 of 50 builds stalled, by 7–14 s, and the whole delay was
  in one phase, *search index* (9–10 s instead of 1.9 s); twice also *tag
  index* (7–8 s instead of 0.3 s). During every stall the build's backend
  waited on `LWLock:WALWrite` while PostgreSQL's WAL writer sat in
  `IO:WalSync`, i.e. in `fdatasync` of the WAL. Rewriting the search index
  (1000 documents with their `tsvector` and GIN entries in a logged table)
  produced more WAL than PostgreSQL's WAL buffers hold, so the build had to
  wait for the WAL writer's sync mid-transaction, then again at commit. The
  probe showed the cause of the length: on this machine (a VM; the disk is
  a "QEMU HARDDISK") an 8 KiB `fdatasync` has a median of 9–30 ms but takes
  2–8.6 s dozens of times an hour (56 syncs over 1 s in 20 minutes, max
  8.6 s), and the probe's slow syncs started and ended at the same moments
  as the build's waits (e.g. 22:45:16–22:45:24 in both). Not a timeout,
  not the pool, not the listener: any wait for a sync on this disk can take
  ~7–8 s, and the build waited for several.
- **Fix (engine):** (1) the PostgreSQL search index is an **unlogged
  table** (migration `search/0002_unlogged`): it is derived from the
  content, so it writes no WAL and never waits for a sync; a crash or
  failover empties unlogged tables, so `engine serve` refills it when it
  finds it empty, at start and once a minute (one `EXISTS` lookup), and
  `engine build` rebuilds it anyway. (2) A reindex is one statement with
  arrays and writes only documents that differ, so rebuilding a current
  index writes nothing. (3) The build's tag index commits with
  `synchronous_commit = off` for that one transaction: the pages it
  describes are written without fsync too (DECISIONS, v0.2), and the
  recovery after a crash is the same for both, `engine rebuild`. A build
  now waits for no disk sync at all. (4) The tag index no longer reads
  every page back from disk to hash it: the hash is taken at render time.
  SQLite was left as it was (it is to be removed).
- **Result (same machine, same harness, 30 builds each):** before min 2.76,
  median 2.98, max 16.9 s, 10 of 30 over 5 s (11 of 50 in all); after
  min 1.55, median 1.70, max 3.31 s, none over 5 s (one at 3.3 s, its search
  phase slow by 1.7 s while the disk probe showed 1–5 s syncs: likely the
  page cache throttling writes; not a sync wait). The disk was no quieter
  during the after runs (probe max 7.5 s).
- **Guard:** `postgres_reindex_writes_no_wal_and_skips_unchanged_documents`
  (engine-search) fails if any search relation is logged, if reindexing a
  current index rewrites a row, or if an emptied index is not refilled;
  `bench/run.sh stall` repeats the build and reports stalls; the cold
  builds in `bench/run.sh` now record each run's phases and the disk's sync
  latency beside it.
- **Rejected:** blaming the disk and stopping there (the engine was waiting
  for syncs it did not need); `synchronous_commit = off` for the search
  index alone (the WAL volume still filled the buffers and waited); making
  the tag index unlogged (an emptied tag index silently stops pages being
  re-rendered); dropping slow runs.

### Edit to published on PostgreSQL: ~1 s → ~0.1 s
- **Cause:** not the disk this time. In the example site every article
  page carries `c:article` (its "related articles"), so editing one
  article re-renders every article page: 233 pages for the 200-article
  benchmark site. The batch read the database once per lookup on every page
  (no snapshot outside `engine build`), wrote the edited page wherever it
  fell in path order, and recorded the index with three statements per page
  plus a second commit for the cursor: 1.9 s per batch, and the edited page
  appeared after 1.0–1.7 s (median 1.73 s over 15 edits). 0.5 measured
  140 ms because its per-page commits made each page appear progressively
  and its batch was ordered differently; the work was the same.
- **Fix:** the changed items' own pages are rendered first; a batch of more
  than 16 pages renders the rest from one content snapshot (as the build
  does; publish routes resolve from it too); the index entries and the
  cursor go in one transaction of three array statements. Batch 1.9 s →
  0.33 s; edit to published median 1.73 s → 106 ms (max 186 ms, 15 edits).
  Guard: the publish test checks the edited page is the first rendered.

### Pre-compressed publish output
- **What:** every text file the engine writes (html, css, js, svg, xml,
  feeds, json, txt, ...) of at least 512 bytes gets `.br` and `.gz` copies
  beside it, unless a copy would save under 5 %. Written by the build's
  render threads into the staging folder (so they arrive in the same swap),
  by every publish write, and removed with the file. Caddy serves them with
  `file_server { precompressed br gzip }` (shipped Caddyfiles; `encode` is
  kept only for responses proxied from the engine); `engine serve` serves
  them itself; the bucket target uploads them with `Content-Encoding`.
- **Levels by measurement** (the 1000-article build's 1,125 text files,
  6.3 MB, one core, rust-brotli 8 and flate2/zlib-rs): Brotli q1 38.0 % of
  the original in 86 ms, q4 34.2 % / 241 ms, **q5 32.2 % / 311 ms**, q6
  32.2 % / 374 ms, q7–q9 32.1 % / 0.8–4.3 s, q10 30.5 % / 5.6 s, q11 29.3 %
  / 13.3 s; gzip l1 39.8 % / 71 ms, **l6 35.4 % / 148 ms**, l9 35.4 % /
  151 ms. q5 and l6 are the knees: q11 would save 3 more points for 43
  times the time, on every edit.
- **Cost:** 1000 articles on PostgreSQL, one core, six builds each: total
  median 1.65 s without copies, 2.24 s with them and no previous output,
  1.65 s when the previous build's output is there (unchanged files'
  copies are hard-linked from it, not recompressed); four cores, cold:
  1.49 → 1.52 s (render and write 83 → 234 ms). Still well under Astro's
  6.4 s. Edit to published (200-article site, 15 edits): median 106 →
  109 ms. Sizes: all text 6.01 MB, Brotli copies 2.02 MB (33.7 %), gzip
  2.22 MB (37.0 %); an article page 4,588 → 1,753 bytes (br).
- **Never stale (the invariant):** a copy's bytes are written before its
  file's, and renamed into place before the file; a file that no longer
  gets a copy loses it before the new file goes live; delete removes the
  copies first. So after any complete write no copy is newer than its file
  and none belongs to an older version. `engine serve` additionally serves a
  copy only if it is a regular file (never a symlink) no newer than the
  file, so a write cut short falls back to the file; the next write of the
  file heals it. Copies are never served by their own names (404 in
  `engine serve` and the Caddyfiles). Caddy cannot check freshness; it
  relies on the write order.
- **Unchanged files are not rewritten:** a publish whose bytes equal the
  file on disk (and whose copies are in order) leaves it alone, so its
  modification time, and the ETag Caddy derives from it, stay stable; an
  edit that re-renders 233 pages rewrites only those that changed.
- **Headers:** `engine serve` sends a strong content-derived ETag (128 bits
  of the SHA-256 of the file's bytes; `-br` / `-gz` for a copy, so one per
  encoding), cached per (path, length, mtime, inode) in a map bounded by
  memory (1/1024 of it); `Vary: Accept-Encoding` on text files; `304` for a
  matching `If-None-Match`; `Cache-Control: no-cache` for HTML, feeds,
  sitemaps and other un-hashed documents. Hashed asset URLs, tag-derived
  ETags and surrogate keys are left for later, as asked.
- **Bucket:** `S3Target` and `engine build`'s sync upload both copies for
  every text file (made on the fly where the folder keeps none, as a CDN
  function cannot test whether a copy exists), with the file's type and
  caching and `Content-Encoding`; the buckets page gives a CloudFront
  Function that picks the copy by `Accept-Encoding`.
- **Workers:** the build compresses on its render threads (one per core);
  a publish batch writes the edited page at once and the rest in groups of
  64 on one thread per available CPU (`engine_core::resources`, which reads
  the affinity mask, a cgroup CPU quota and the cgroup memory limit).
- **Rejected:** compressing per request (`encode`), zstd copies (Caddy's
  `precompressed` supports it, browsers mostly do too, but two copies
  already cover every browser and a third costs disk and build time for
  little), a configuration switch (nothing to choose).

### Resource benchmarks (`bench/run.sh`, `bench/resources.py`)
- **Targets and groups:** `--group core` (default: Wrought, WordPress,
  Drupal, Astro serving; Wrought, Astro, Hugo 0.170.1, Zola 0.22.1
  building), `--group extended` (Rails, Wrought on SQLite), `--targets a,b`;
  wall time per target goes into results.json. The expensive measures
  (smallest memory limit, 20 sites on one PostgreSQL) run for Wrought,
  WordPress and Drupal only. The core run took 64 minutes up to its last
  step (cold starts, lost to a harness bug that emptied the site database
  after the shared-PostgreSQL step; fixed, not re-run).
- **Accounting:** each target's containers summed from cgroup v2:
  `memory.current` and `anon`, sampled every 100 ms during loads; CPU from
  `cpu.stat` `usage_usec` per 1,000 requests of the standard wrk test. The
  minimum memory limit steps `docker update --memory` down (1 GiB … 8 MiB)
  until the load test errors or the kernel OOM-kills.
- **Page caching for WordPress:** Apache `mod_cache` with a disk store
  (exact directives in `run.py::wp_page_cache`), the host-level page cache
  of a plain Apache host, chosen over a plugin because it needs no
  plugin-specific setup to be reproducible. Drupal's stock install already
  caches pages for anonymous visitors.
- **Hugo and Zola:** the same 1000 Markdown articles, a template of the
  same weight (nav, article, list paginated by 10, topic pages), RSS off,
  no syntax highlighting; cold builds (output removed) on one core, peak
  anonymous memory from the container's cgroup. Neither writes a search or
  tag index or compressed copies; both beat Wrought's Markdown build (0.75
  and 0.70 s against 1.27 s, which includes 0.6 s of compression). Neither
  has an incremental `build`: changing one article rebuilds everything.
- **Not done:** Phoenix and Loco targets (asked for; not built this round),
  Rails re-run (moved to `extended`), PostgreSQL edit-to-published under
  the 1 GB container stall (see the stall entry).
- **Also found:** `engine_forms` wrote its stamp key on every open, so each
  build and server start on a new database committed (and waited for) one
  sync: 3.6 s in a stalled bench build. The key is now read first and made
  by `engine migrate`.

### Engine memory budget
- **Measured** (`scripts/check-memory.sh`: release `cms` preset serving
  examples/cms-site on PostgreSQL, after 120 requests of pages, searches
  and the sign-in page): RSS 24,336 KiB, of which anonymous memory 4,120
  KiB; the rest is the binary's own code and data mapped from disk, shared
  with every other engine process on the host. In the benchmark the engine
  container served the load test at a 12 MiB limit; 20 engines on one
  PostgreSQL took 4.7 MiB of memory each.
- **Allocator:** kept glibc's. With 4 MiB of heap at idle there is no
  allocator win worth a dependency; not measured under load this round
  (mimalloc and jemalloc are the candidates if the load-test peak grows).
- **Sizing:** the new `engine_core::resources` (CPUs from the affinity
  mask and cgroup quota, memory from the cgroup limit) sizes the publish
  write pool and the ETag cache (1/1024 of memory); the database pool
  stays at `[database] max_connections` (5), lazily filled. Note from the
  shared-PostgreSQL run: 20 sites hold about 110 connections at idle (the
  pool's first connection plus the publish and job listeners'), so a
  host's PostgreSQL needs `max_connections` above 100 for 20 sites.
- **Guard:** `scripts/check-memory.sh` fails above 28,672 KiB (the
  measurement plus 18 %), run by the `memory-budget` job in host.yml.

### Roadmap after v1.0: enterprise gaps as v1.1 to v1.6 (owner, 2026-11-27)
- **Decided by the owner:** v1.0 ships first with no new features beyond
  v0.9; what enterprise platforms provide that the engine and its serving
  stack do not becomes milestones v1.1 (scale-out and high availability),
  v1.2 (editorial at scale), v1.3 (identity, governance, compliance), v1.4
  (personalisation, experiments, insight), v1.5 (hosting platform) and
  v1.6 (ecosystem and support). The spec's roadmap has the contents and an
  acceptance test for each.
- **Order** is the supervisor's judgement of how much each gap blocks a
  growing customer; it can change with demand.
- **Constraint:** the small-host rule applies. Each item is a feature
  toggle or a separate serving-stack component, so a single small install
  gets no heavier.
- **Unchecked:** the v1.5 list was written from comparison with commercial
  hosting platforms, not from an audit of the serving-stack repository.
- **Benchmark targets added by the owner the same day:** Hugo and Zola
  (builds, done), Phoenix and Loco (serving, not yet built). Rails moved
  to an extended group to keep the core run near an hour.

### The stall that remained in the benchmark stack: a sync the index never needed
- **Symptom:** after the unlogged search index, host builds were clean, but
  in the bench stack (PostgreSQL in a 1 GB, one-core container) 5 of 20
  builds still took up to 9.9 s, the delay in the search-index phase. The
  guess then was cgroup write throttling.
- **Method:** `bench/run.sh stall` now records, while each build runs,
  what every PostgreSQL process waits for (`pg_stat_activity` every 50 ms),
  the database container's cgroup (file_dirty, file_writeback, memory and
  I/O pressure, memory.events, io.stat), the host's Dirty/Writeback and the
  PostgreSQL log, and keeps the whole timeline of a stalled run in its
  JSON file; `pg_walinspect` showed which transaction wrote which WAL.
- **Finding:** not throttling. Dirty page cache peaked at 15 MB in the
  container (54 MB on the host), nothing under writeback in any 50 ms
  sample, memory.events all zero (no `high`/`max` hits), no checkpoint
  logged during any build (`log_checkpoints` is on by default in 17), and
  no backend ever waited in `DataFileSync`/`DataFileImmediateSync`. In every stall
  the build's backend sat at `COMMIT` of the search-index transaction in
  `LWLock:WALWrite`, then `IO:WalSync`, while the WAL writer was in
  `IO:WalSync` for the same 5–8 s that the independent `fdatasync` probe
  stalled. The commit waited for a WAL flush although the table is
  unlogged: the transaction had written one WAL record, `PRUNE_ON_ACCESS`
  on `pg_attribute`. A fresh database has dead catalog rows from its
  migrations (`ALTER TABLE ... SET UNLOGGED` rewrites the table), the
  planner prunes them on first access inside the index transaction, and a
  transaction that wrote any WAL commits synchronously. Reproduced outside
  the bench stack too (a plain `postgres:17.12` container: 7.7 s in
  *write documents*, the only WAL in that transaction the catalog prune and
  the commit record). The unlogged table's files were not fsynced at
  create, truncate or checkpoint during the builds; the delay was in no
  other statement.
- **Fix (engine):** every write to the search index (rebuild, one item,
  removal) commits with `synchronous_commit = off` (`Db::begin_relaxed`).
  The index is unlogged, so the commit has nothing durable to wait for;
  whatever WAL PostgreSQL writes on its own behalf in that transaction is
  flushed by the WAL writer moments later. Content saves keep their
  synchronous commits. Not a configuration matter: it would hit any host
  whose disk syncs slowly, capped or not, so no shipped PostgreSQL
  setting changes.
- **Result (`bench/run.sh stall 25 1`, bench stack, same day):** before
  min 1.85, median 2.52, max 9.58 s, 4 of 25 stalled
  (`results/stall-2026-11-27-1c-before.json`); after min 1.86, median
  2.15, max 2.85 s, 0 of 25 (`-after.json`), although the disk was worse
  during the after runs: 9 of the 25 builds overlapped a 4.5–8.6 s sync
  (3 before). Two of the four "before" stalls (4.4 and 6.8 s) were outside
  the engine: its own total was 1.9–2.0 s, the rest was the
  `docker compose exec` around it (not investigated; none in the after
  runs).
- **The disk:** an 8 KiB `fdatasync` on this VM's disk still takes 5–8.6 s
  several times an hour, for an unrelated writer (the probe) as much as
  for PostgreSQL; anything that must wait for a sync (a content save) will
  still see it. The build no longer waits for any.
- **Also seen, not explained:** the search-index phase grows slowly over a
  series of builds on one PostgreSQL server (760 ms on the first, about
  1.2 s on the 25th, before and after the fix alike); each build has a new
  database. Not a stall; left for later.

### Cheaper pre-compression in builds
- **Where the time went** (the 1000-article Markdown build's 1,121 text
  files, 5.4 MB, one isolated core; engine builds with copies switched off
  by a measuring hack, then each encoder alone in a micro-benchmark):
  *render and write* 228 ms without copies, 731 ms with them. Of the
  ~500 ms: Brotli q5 about 275 ms, gzip 6 about 96 ms, writing the 2,240
  copy files about 40 ms, the rest cache effects of interleaving. Within
  rust-brotli, 12 % of the time was clearing its 1 MiB hash table for each
  file (it zeroes the whole table, the C encoder does not).
- **Tried and dropped:** Brotli window 17–22 and one-shot streaming with a
  size hint (no change: 270–276 ms); window ≤ 16, which makes rust-brotli
  pick its H40 hasher (4.3 s, 16 times slower); Brotli q4 (200 ms but
  34.4 % instead of 32.4 % of the original, 6 % more bytes on every
  transfer: a resource saving paid for by every reader, rejected); gzip
  level 4–5 (2–8 ms faster, slightly larger); reusing the zlib-rs state
  (7 ms). A shared dictionary was not wanted.
- **Chosen:** Google's C Brotli through `brotlic` in one shot
  (`BrotliEncoderCompress`, which sizes its tables to the input), and
  libdeflate through `libdeflater` with one compressor per render thread.
  Same levels. Micro-benchmark: Brotli 275 → 208 ms with byte-identical
  output; gzip 96 → 74 ms and 0.6 % smaller. In the engine build:
  *render and write* 731 → 605 ms, total 791 → 663 ms (six builds each,
  one core). Both are C libraries built by `cc` (the tree already builds
  SQLite and ring that way); licences MIT/Apache, cargo-deny clean. The
  Rust decoders stay for reading copies back (bucket targets, tests).
- **Not done: compressing after the swap on a first build.** Copies made
  after the new output is live would race with publishes: a copy of the
  old bytes renamed in after a publish wrote the new file would be served
  by Caddy, which cannot check freshness. Making that safe needs a lock
  between the build's tail and every publish writer; it would also make
  "the build finished" mean less than it says. Kept as it was: copies are
  in the staging folder before the swap.
- **Measuring switch:** `ENGINE_BUILD_PRECOMPRESS=0` makes `engine build`
  write no copies (the next publish of a page writes them again), so the
  benchmark can report the build with copies on and off. Not a site
  setting; the decision against a configuration switch stands.

### Benchmark: Phoenix and Loco
- **What:** two more request-rendering targets in the core serving group,
  built the way the Rails one was: the framework's own generator at a
  pinned current version, the bench's few files copied over it, production
  mode, PostgreSQL 17.12, the same 200 articles, the same page shell and
  stylesheet, the same article / listing / topic / search pages and the
  token-protected title edit.
  - **Phoenix** 1.8.17 on Elixir 1.20.5 / OTP 29 (`mix phx.new
    --database postgres --no-assets --no-live --no-dashboard --no-mailer
    --no-gettext`), Ecto schemas, HEEx templates, an OTP release as the
    "Deploying with Releases" guide makes it (`mix phx.gen.release`,
    `mix phx.digest`, `mix release`, `bin/server`), Bandit, the generated
    config including its pool of 10 connections. `bench/phoenix/`.
  - **Loco** 1.3.1 (loco-rs 1.3.1, SeaORM 2.0.4, Rust 1.100.0; `loco new
    --db postgres --bg async --assets serverside`), Tera views, SQL through
    SeaORM's raw statements, a release build started with `-e production`,
    the generated production config (pool of 10, JSON logs at info).
    Loco's router drops trailing slashes, so its paths are requested
    without them. `bench/loco/`.
- **Search** in both (and Rails) is a `websearch_to_tsquery` match on the
  GIN expression index, newest first, 20 rows; Wrought's search page does
  more per request (rank, highlighted snippets, a total count), which the
  docs say beside the numbers.
- **Measured** per app (`run.py::measure_app`, now also used for Rails):
  cold start (stop, start, until the article answers), article, listing and
  search under the standard wrk load, memory of the application and of its
  database separately (cgroup total and anonymous), CPU per 1,000 requests
  of each page, page bytes, page weight in Chromium, edit to published.
  Wrought gets the same listing and search figures rendered per request
  (render dial `request`) beside its published-file figures.
- **Checked:** generator documentation read for both (Phoenix releases
  guide, Loco "Your First App"); both apps smoke-tested page by page
  before measuring.

### Benchmark: complete run (2026-11-27)
- **Two runs, each writing its own file:** `--group core` 66 minutes
  (`results/results.json`, wall time per target in `wall_s`; nothing cut,
  it stayed under the 75 minutes allowed), `--group extended` 7 minutes
  (`results/results-extended.json`). `report.py` merges the two for the
  docs; no figure was taken from a log. Cold starts, Rails and per-target
  wall time are in this time.
- **What the run says against Wrought** (docs, "Where Wrought loses"):
  rendered per request, Loco (1,802 article pages a second) and Phoenix
  (1,005) beat Wrought (566); their search answers ten times as many
  requests (Wrought's does ranking, snippets and a count, theirs does
  not); Hugo and Zola still build Markdown faster than Wrought with its
  compressed copies (0.78 / 0.64 s against 1.14 s; 0.66 s without copies);
  Loco idles in 4.5 MiB against the engine's 31 MiB after load; the
  engine's smallest workable limit was 16 MiB, not 12; SQLite edit to
  published 108 ms against 21 ms in the 0.6 run. The last three are not
  investigated.

### Faster request rendering and search
- **Measured first** (200-article bench site, render dial `request`,
  `engine serve` pinned to one core, PostgreSQL 17.12 in a 1 GB one-core
  container, wrk on a third core, 32 connections; engine CPU from
  `/proc/<pid>/stat`, database CPU from its cgroup; statements from
  `log_min_duration_statement = 0`; `perf` on a build with symbols). Before:
  article 540 requests/s (engine 1.6 ms and database 1.5 ms of CPU per
  page), search 164/s (engine 1.7 ms, database 6.2 ms).
- **Where the time went, article page:** eleven statements, each on a
  connection checked out of the pool and returned, and sqlx checks a
  connection with a round trip on return and (by default) on checkout:
  some 33 round trips a page. `related_items` was 42 % of the engine's
  time: two lookups, then for each related item its row and its terms
  (eight statements), then each item's Markdown rendered and sanitised
  although the component prints only titles. The route table was rebuilt
  for each request (checking template files on disk). Session lookups:
  none for anonymous visitors (no cookie, no query). Templates are
  compiled once; fragments were not used by these pages.
- **Where the time went, search:** `ts_headline` parses and stems each
  hit's text again (0.45 ms per 2.6 KB article on one core: 4.8 of the
  6.2 ms); the total was a second scan on a second connection; the
  ranking read every match's `tsvector` out of TOAST storage (1.6 ms
  against 0.5 ms with it in the row). The bench's words each match about
  198 of the 201 documents, so PostgreSQL rightly scans rather than using
  the GIN index; with a selective word, or a larger table, it uses the
  index (checked with `EXPLAIN`, and in a test with scans switched off).
- **What the others do per request** (bench/loco, bench/phoenix): the
  article is four statements (card, stored HTML body, topics, related as
  one join); search is one statement, newest first, no rank, no snippet,
  no total, 20 rows.
- **Changes** (no feature removed, nothing special-cased):
  - `Db::pinned`: a render's queries share one pooled connection, taken at
    the first query, returned at the end (a thread-local slot that the
    query dispatcher and `Db::acquire` use; transactions and concurrent
    queries take their own). Request renders resolve and render in one
    blocking task inside it.
  - The pool no longer checks each connection on checkout; only one idle
    for 30 s or more is checked first (sqlx still checks on return).
  - `related_items`: two statements (the ranked candidates joined to the
    content table in one; terms) instead of eight.
  - PostgreSQL: an item's terms come with its row (a `json_agg` subquery),
    so reading one item is one statement.
  - Items' template values (rendered, sanitised Markdown) are kept in a
    cache bounded in bytes (1/256 of memory, 1–64 MiB) and reused only
    for an equal item, so they are never stale and need no eviction.
  - The route table is built once per renderer.
  - Request renders at once are limited to the pool's connections (an
    async semaphore): more cannot run, and each waiting one used to hold
    a blocking thread.
  - Search: one statement per page (ranking with a window count over the
    same pass, narrow rows through the sort, the text read by primary key
    for the page's rows only, the query's normalised lexemes), snippets
    cut in the engine (`snippet.rs`, Snowball stemmers via
    `rust-stemmers`, MIT/BSD-3), `ts_headline` kept for custom
    configurations; `tsvector` in the row (`STORAGE MAIN`, migration
    `search/0003_tsv_inline`, which empties the index for the engine to
    refill). The total is exact: ranking already visits every match, so
    the count costs no extra scan, and no cap or estimate is needed.
  - A signed-out visitor's form-token cookie is set only when the page
    carries the token.
- **Checked:** a test compares the engine's marks with `ts_headline`'s
  (`HighlightAll`) for 20 queries over stemmed forms, hyphenated words,
  numbers, addresses and accented words: equal, except negated words
  (`ts_headline` marks them, the engine does not) and phrases (the engine
  marks the words anywhere). `lz4` compression of the `tsvector` was
  measured too: no gain, not used.
- **Result (same rig, release build):** article 540 → 1,496/s (engine 0.47
  ms, database 0.65 ms per page), listing → 1,547/s, search 164 → 1,047/s
  (engine 0.67 ms, database 0.93 ms). The database is now the limit for
  both (one core): search's 0.6 ms of execution is ranking every match
  (~1.3 µs each for ~198 matches), the window count and the sort.
- **Memory after load** (doubt from the last run: the engine container at
  31 MiB after the load tests against 23 MiB RSS in check-memory, and its
  smallest workable limit up from 12 to 16 MiB). check-memory measures a
  fresh process after 120 sequential requests; the bench's engine has
  served the whole sequence (files, search, listing, an edit, 32
  connections at a time). The figure is anonymous memory (29 of the 31
  MiB), and it is not the pool, the caches or the binary: it is freed
  memory the allocator kept. glibc gives each thread that allocates
  concurrently an arena, up to eight per *online* CPU (192 on this 24-CPU
  host, whatever the container's quota); `tokio::fs` file reads and the
  renders spread over up to 43 blocking threads, and their freed memory
  stays in those arenas, fragmented. The first re-run showed it plainly:
  the publish-mode engine at 32.5 MiB anonymous before its search test,
  57.6 at its peak and 50.3 MiB idle after (the same process had also
  published an edit), the smallest workable limit still 16 MiB (at 12 MiB
  the limit is below what it already holds: an OOM kill). Rig, publish
  mode, files then search then listing at 32 connections: 3.7 MiB at
  start, 12 MiB idle after; with arenas capped at 8 (`MALLOC_ARENA_MAX`)
  8.7 MiB; at 2, 11 MiB (contention fragments more). glibc's trim
  threshold and thread-stack cache changed nothing measurable.
- **Memory fix:** `engine_core::memory`: the arenas are capped at eight
  per CPU the process may use (glibc's rule on the right count; set before
  any thread starts), renders at once are limited to the pool's
  connections (above), and `malloc_trim(0)` runs when the server goes
  quiet (checked every 30 s: a quiet interval after a busy one; never
  during a steady load). Rig, same sequence: 14 MiB right after the load,
  6.4 MiB a minute later. Throughput unchanged (files 9,800/s, search
  1,045/s).
- **Benchmark re-runs** (`bench/run.sh --targets`, merged with the new
  `bench/merge.py`; files `rerun-1-wrought-phoenix-loco.json`,
  `rerun-2-wrought.json`): article rendered per request 566 → 1,670 /
  1,360 a second (Loco 1,801, Phoenix 731 in this run, 1,005 before),
  listing 526 → 1,630 / 1,627 (Loco 2,778, Phoenix 1,354), search 167 →
  1,117 / 1,111 (Loco 2,202, Phoenix 1,735); CPU per 1,000 searches 9.9 →
  1.8–2.1 s. The two Wrought runs differ only by the memory change; the
  article figure moved 19 % between them, which the rig did not reproduce
  (unexplained run-to-run spread; both are above Phoenix's). Smallest
  workable engine limit 16 → 12 MiB. The second run's binary checked for
  idleness every 30 s; the committed code checks every 10 s (the bench
  samples "idle after" 10 s after the load, before either trims).
- **Their search, for the remaining gap:** Loco (`bench/loco`): one
  statement, `to_tsvector('english', title || summary || body_html)`
  matched with `websearch_to_tsquery` through a GIN expression index,
  `ORDER BY published_at DESC LIMIT 20`, joined to the author; no rank, no
  snippet, no total. Phoenix: the same query through Ecto. Wrought ranks
  every match (`ts_rank`), counts them, and cuts a highlighted extract for
  each of the page's hits; on this data each query matches ~198 of 201
  documents, so ranking reads every match's tsvector.
- **Build search-index creep** (760 ms → 1.2 s over 25 builds in the bench
  stack): not reproduced outside it. On one PostgreSQL container, a fresh
  database per build as `bench/run.sh stall` does, 20–25 builds stayed
  flat (770–955 ms, no trend) with the container's data in its own layer,
  in a named volume, and with the stall harness's `pg_stat_activity`
  poller running; WAL stayed at 33 MB and the container at 131 MB.
  In the bench stack it is still there with this change:
  `bench/run.sh stall 25 1` (`results/stall-2026-11-27-1c-v08.json`):
  search index 737 → 1,250 ms, rising steadily build by build; builds min
  1.89, median 2.13, max 8.56 s (one stall, an autovacuum worker and the
  WAL writer waiting on this VM's slow sync). The cause is still unknown:
  it appears only with the engine running in the bench's tool container
  through `docker compose exec`, not with the same engine and PostgreSQL
  image driven from the host. Left for later.

### One pinned toolchain; a lost-keystroke race in the editor
- **CI red on every push, two causes.** (1) The `clippy` job installed
  whatever was stable (1.100.0 by then), whose `result_large_err` fired in
  `engine-payments`; 1.95.0 locally did not. (2) The editor's browser test
  failed at "the preview shows the new rich text, bold included" on
  GitHub's runner, never locally.
- **Toolchain:** `rust-toolchain.toml` pins 1.100.0 (the newest stable
  rustup offered on 2026-11-27) with clippy and rustfmt; the CI action reads
  the version from that file, and rustup uses it locally. Raising it is a
  commit of its own that also fixes what the new lints find. `rust-version`
  (the oldest compiler the crates claim to build with) is unchanged.
- **The lint, properly:** a guard's refusal (the page to send instead of
  going on) is now `Box<Response>` (`engine_admin::Refusal`, and the same in
  the payments and forms guards); the crate-level
  `allow(clippy::result_large_err)` in `engine-admin` is gone. The box is
  allocated only on the refusal path.
- **The editor failure was a product race, not a slow assertion.** CI's
  frame held "Forged in the editor ld": "with bo" lost and no bold. A
  pointer press on a toolbar button moves the focus to the button; Tiptap's
  `focus()` gives it back to the text on the next animation frame. Keys
  typed before that frame went to the button, and the space between "with"
  and "bold" pressed Bold again (off). Reproduced locally by delaying
  animation frames by 150 ms in the page: the old bundle lost "with bol"
  the same way; with the fix nothing is lost. Fix: the toolbar cancels the
  `mousedown` default when the text has the focus, so the focus never
  leaves it (the usual rich-text toolbar behaviour; Tab still reaches
  every button). The test now also waits for the text to have the focus
  and for Bold to be pressed before typing, and accepts
  `WROUGHT_E2E_CPU_THROTTLE` (Chromium CPU throttling) to shake out races.
- **Actions:** checkout, setup-node and cache v5, upload-artifact v6,
  download-artifact v7: the first majors that run on Node.js 24 (GitHub
  forces Node 20 actions onto 24 and warns).

### Speed guard
- **What:** `scripts/check-speed.sh`, run by the `speed` job of host.yml
  next to the memory budget. Release `cms` build serving
  examples/cms-site on PostgreSQL (wrought-test-postgres), the engine
  pinned to one CPU with `taskset`, the benchmark's own load tool (wrk in
  the `loadgen` image, as `wrought-bench-speed`, removed after each load)
  on another, 32 connections, 2 s warm-up and 5 s measured for each of: a
  published article file through `engine serve`, the same article
  rendered per request (render dial switched to `request`, engine
  restarted), and search with a different word per request
  (`bench/loadgen/vary.lua`). Any non-2xx response fails it. About 40 s
  after the build.
- **Floors from this script's own runs, not the benchmark's** (five runs on
  the benchmark machine, 2026-11-27, PostgreSQL not pinned): file 6,918–
  7,335 (mean 7,160), render 1,154–1,308 (mean 1,220), search 2,343–2,793
  (mean 2,530). Floors are 62 % of the mean: 4,400, 750 and 1,550. Search
  is faster here than in the benchmark (1,110) because the example has
  four articles, not 200; the guard is for regressions of this setup, not
  a benchmark.
- **Other runners:** GitHub's hosted machines are slower and noisier than
  the benchmark machine; the floors can be set per repository with the
  variables `WROUGHT_SPEED_FLOOR_FILE`, `_RENDER` and `_SEARCH` (measured
  on that runner, documented when set). Not yet run on GitHub: unknown
  whether the defaults hold there.

### Limits scaled to the machine; `engine doctor`
- **One module, `engine_core::limits`:** pure sizing functions of the
  detected resources (`engine_core::resources`: the CPUs the process may
  run on after a cgroup quota, and memory after a container limit). No
  "small mode": the same formulas on a 1-CPU, 512 MiB container and a
  64-CPU server; unit tests pin the values at those two and at a 4-CPU,
  8 GiB Pi-class machine (and a 4-CPU, 128 MiB case where memory binds).
- **Formulas** (unset values only; each one can be set):
  - async workers: one per CPU (`[limits] worker_threads`);
  - blocking threads: 16 per CPU, 32 to 512, at most one per 4 MiB
    (`[limits] blocking_threads`). Tokio's default was a flat 512 on every
    machine;
  - database connections, which also bound renders at once: CPUs + 4, 4 to
    32, at most one per 64 MiB (`[database] max_connections`, env
    `ENGINE_DATABASE_MAX_CONNECTIONS`). The old default was a flat 5; one
    CPU still gets 5, so the benchmark's one-core figures keep their pool.
    Ten Pi-class sites sharing one PostgreSQL get 80 connections, under its
    default 100;
  - job workers: one per CPU, 2 to 16 (`[jobs] workers`; was 4);
  - image variants at once: one per CPU and per 256 MiB (a decoded
    photograph can take ~100 MiB), at least 1 (`[limits]
    background_threads`);
  - rendered value cache 1/256 of memory, 1 to 64 MiB (unchanged; `[limits]
    value_cache_mb`); fragment cache 1/64 of memory, 4 to 256 MiB (`[cache]
    max_mb`, was a flat 64 MiB, an eighth of a 512 MiB container); file
    ETag cache 1/1024 of memory at ~200 bytes an entry, at least 4096
    (unchanged; `[limits] etag_cache_entries`); allocator arenas eight per
    CPU (unchanged, set before any thread).
- **Overrides stay where they always were** (`[database]`, `[jobs]`,
  `[cache]`); only values with no home got `[limits]`. Loading a site's
  configuration records them for the parts that size themselves (the
  runtime, the caches); `engine doctor` reports each value, its formula and
  the key that set it.
- **Background work is throttled by priority, not by a smaller pool.** Image
  processing (on upload) runs on its own threads at nice 10, at most
  `background_threads` at once; the threads that compress a bulk publish's
  files run at nice 10 too (one per CPU, as before; a single-page publish
  stays on the caller's thread at normal priority, so edit-to-published is
  unaffected). On an idle machine they still get every CPU; under load the
  kernel gives a serving thread about nine times their share. A fixed delay
  or fewer threads would have slowed them on idle machines too, against
  the small-host rule. Nice is per thread on Linux, and an unprivileged
  process cannot raise it back, so this is only done on threads made for
  the work (never on Tokio's shared blocking threads).
- **Queues:** the job queue is in PostgreSQL; in memory, requests waiting
  to render wait on the connection semaphore (bounded by the pool), and
  caches are bounded in bytes or entries as above. No queue was found that
  grows without a bound; none was added.
