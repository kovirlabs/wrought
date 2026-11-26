### Test containers renamed to `wrought-test-*`
- **Decision:** every compose file in `deploy/` has a `wrought-test-` project
  and container name (`wrought-test-postgres`, volume `wrought-test-pgdata`,
  `wrought-test-mailpit`, `wrought-test-caddy`, ...). The benchmark keeps
  `wrought-bench-*`.
- **Why:** the owner's Bloomery stack runs as compose project `wrought`
  (`wrought-postgres-1`, `wrought-caddy-1`, volumes `wrought_pgdata`, ...).
  `wrought-postgres` and `wrought-pgdata` were one character away from it, a
  prune or `down` by name could hit the wrong one.
- **Rejected:** keeping the names and relying on care.

### Flaky Postgres test: the `LISTEN` connections starved the pool
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

### Parallel `engine build`
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

### Whole-site renders read the content once (bulk preload)
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

### Indexes from `engine types sync`, checked with EXPLAIN
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
- **Not added:** GIN on JSON fields (no query filters inside JSON); covering
  `INCLUDE` indexes for lists (list pages select whole rows, which templates
  use freely); keyset pagination for list routes (pages are addressed by
  number, `/articles/page/7/`, and the bulk preload removes OFFSET from
  builds); summary tables for tag counts (one query per tag-cloud render,
  not a measured cost).

### Postgres connection settings
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

### Block trees as JSON in the admin (for the visual editor)
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

### `askama`: deferred, with reasons
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

### One admin gate for HTML and JSON (security review finding)
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
