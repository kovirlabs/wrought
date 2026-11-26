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
