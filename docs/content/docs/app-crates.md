+++
title = "App crates"
description = "A site as its own Rust crate: HTTP handlers, named SQL queries for templates, jobs, lifecycle hooks, migrations and commands registered on the engine, in one binary."
weight = 21
+++

Most sites need no Rust: templates, content types, forms and the admin
cover them, and the stock `engine` binary runs them. When a site needs code
of its own (an endpoint, a query no template function makes, background
work, a reaction to a publish), it becomes a Rust crate: its `main.rs`
registers that code on an `App` and runs it. The result is the site's own
binary: the whole `engine` command line (`new`, `dev`, `build`, `serve`,
`doctor`, `migrate`...) plus the site's additions. Compile-time crates are
the plugin model; there are no runtime plugins.

The stock `engine` binary is the empty app:

```rust
fn main() -> std::process::ExitCode {
    engine_cli::App::new().run()
}
```

The example in the repository, `examples/custom-app`, registers one of each
kind and is tested end to end on PostgreSQL.

## The crate

The site folder is the crate's folder: `site.toml`, `templates/`, `types/`
and `migrations/` sit next to `Cargo.toml` and `src/`.

```toml
[package]
name = "my-site"
edition = "2024"

[dependencies]
# The engine with the features the site uses: a preset or single features.
engine-cli = { version = "0.7", features = ["app"] }
# Only for the extractors you use (same version as the engine's).
axum = { version = "0.8", default-features = false, features = ["json"] }
serde = { version = "1", features = ["derive"] }
```

`engine_cli` re-exports what its API is made of, at the engine's versions:
`axum`, `clap`, `minijinja`, `serde_json`, `tokio`, `anyhow`,
`engine_core`, and with their features `engine_db`, `engine_jobs`,
`engine_mail`, `engine_auth`, `engine_forms` and `engine_publish`.

Build it as usual (`cargo build --release`), and use the binary wherever
the docs say `engine`: `my-site migrate`, `my-site build`, `my-site serve`.
For development, `cargo run -- serve` from the site folder.

## What a site registers

```rust
use axum::routing::post;
use engine_cli::App;

fn main() -> std::process::ExitCode {
    App::new()
        .migration("0001_signups", include_str!("migrations/0001_signups.sql"))
        .route("/signups/", post(add_signup))
        .query(
            "recent_signups",
            "SELECT id, email FROM app_signups ORDER BY id DESC LIMIT $1",
            &["c:signups"],
        )
        .job("signup.welcome", welcome)
        .on_publish(log_publish)
        .command(clap::Command::new("signups"), list_signups)
        .run()
}
```

Everything is registered before the command line is parsed and resolved
once, at startup. A registration the engine refuses (a path under one of
the engine's routes, a reserved job kind, a duplicate name, an invalid tag)
stops the binary with a message before anything runs.

| Method | Needs the feature | What it adds |
| --- | --- | --- |
| `route(path, handler)` | a database feature | an HTTP handler |
| `query(name, sql, tags)` | a database feature | `query(name, ...)` in templates |
| `migration(name, sql)` | a database feature | a migration embedded in the binary |
| `job(kind, handler)` | `jobs` | a handler on the job queue |
| `on_publish(hook)` | `publish` | a hook after each publish batch |
| `on_form_submitted(hook)` | `forms` | a hook after an accepted form post |
| `command(clap::Command, run)` | none | a command of the binary |

"A database feature" is any of `content`, `auth`, `forms`, `jobs` or
`media` (the `cms` and `app` presets have them all).

### HTTP handlers

`route` takes an [axum](https://docs.rs/axum/0.8) path (`/notes/{id}/`)
and method router (`get(f)`, `post(f).delete(g)`). Handlers use any axum
extractor; `State<Context>` gives them the engine:

| `Context` | |
| --- | --- |
| `db()` | the site's database pool (`engine_db::query(...)`), with the engine's outage handling |
| `session(&headers)` | the signed-in visitor (`auth`) |
| `auth()` | accounts, roles and sessions (`auth`) |
| `render(template, ctx)` | a site template with the site's layout, components and template functions; returns the HTML and the cache tags it recorded |
| `query(name, args)` | a named query's rows as JSON |
| `evict(&tags)` | drop cached fragments and request-page ETags carrying these tags |
| `enqueue(kind, payload)`, `jobs()` | the job queue (`jobs`) |
| `mailer()` | email through the queue, when configured (`mail`) |
| `public_url()`, `site()` | the site's address and folder |

```rust
use axum::{Json, extract::State, http::StatusCode};
use engine_cli::app::{Context, HandlerResult};

async fn add_signup(
    State(cx): State<Context>,
    Json(body): Json<NewSignup>,
) -> HandlerResult<StatusCode> {
    let id: i64 = engine_cli::engine_db::query(
        "INSERT INTO app_signups (email) VALUES ($1) RETURNING id",
    )
    .bind(body.email.as_str())
    .scalar(cx.db())
    .await?;
    cx.enqueue("signup.welcome", serde_json::json!({ "id": id })).await?;
    Ok(StatusCode::CREATED)
}
```

`HandlerResult<T>` lets a handler use `?`: an error is logged with its
cause and answered with a bare `500` (no detail reaches the visitor), or
with `503` and `Retry-After` when the database was unavailable.

`engine serve` mounts the handlers next to the engine's own routes, on the
same router, tried before published files. The paths of the engine's
routes are refused: `/_engine/`, `/admin`, `/account`, `/api/v1/`,
`/media/`. Responses that set no `Cache-Control` get `private, no-store`,
like every engine response. A handler that changes data on a `POST` and
trusts the session cookie must check the request's origin
(`engine_core::web::same_origin`) or a CSRF token, as the engine's forms
do. Behind the front door ([deployment](/docs/deployment/)), a handler's
path is answered by the engine whenever no published file has that path.

Handlers need the site's database: `serve` refuses to start a site that
registers handlers or queries without one. `engine dev` builds and serves
the published files only; run `serve` to try handlers.

### Named queries in templates

`query(name, sql, tags)` registers one `SELECT` (or `WITH ... SELECT`)
under a name. Templates call it with its arguments, bound as `$1`, `$2`...,
and get its rows as maps:

```jinja
{% for s in query("recent_signups", 5) %}
<li>{{ s.email }}</li>
{% endfor %}
```

- Arguments bind with their template type: integers as `bigint`, other
  numbers as `double precision`, strings as `text`, booleans, `none` as a
  text `NULL`, lists and maps as `jsonb`. Cast in the SQL where a column
  needs another type (`$1::int`).
- The rows come back as JSON from PostgreSQL itself (one `to_jsonb` per
  row, one round trip), so any column type works and the query's order is
  kept.
- Every query is prepared against the database when the site starts
  (`build`, `serve`): a misspelt column fails the start, naming the query,
  instead of a page.
- In a whole-site build, a query with the same arguments runs once and is
  reused by every page.
- Handlers run the same queries with `Context::query`.

**Cache tags.** `tags` are the [cache tags](/docs/publishing/) a page that
uses the query depends on, recorded on every call. A published page using
it is re-rendered when one of them changes, and cached fragments carrying
them are dropped. For the site's own tables, the engine's change log does
the work: give the table the change-log trigger, with the name its tags
use,

```sql
CREATE TRIGGER app_signups_changes
    AFTER INSERT OR UPDATE OR DELETE ON app_signups
    FOR EACH ROW EXECUTE FUNCTION engine_record_change('signups');
```

and every change of a row publishes the pages that recorded `c:signups` or
`i:signups:<id>`, whoever wrote it (a handler, a job, `psql`). The
function comes with the `content` feature. For changes outside the change
log, a handler calls `Context::evict` for fragments.

### Jobs

`job(kind, handler)` adds a handler to the engine's PostgreSQL job queue
(`SKIP LOCKED`, retries with backoff, dead jobs kept for the admin, see
[jobs and mail](/docs/jobs-and-mail/)). It runs in `serve`'s workers and
in `jobs work`. Kinds are `[a-z][a-z0-9_.-]*`; `engine.`, `mail.` and
`payments.` belong to the engine.

```rust
use engine_cli::app::JobContext;

async fn welcome(cx: JobContext) -> anyhow::Result<()> {
    let id = cx.job.payload["id"].as_i64().unwrap_or_default();
    // cx.db, cx.jobs, cx.mailer (with `mail`); cx.job.attempt...
    Ok(())
}
```

An `Err` fails the attempt; its message is stored on the job.

### Hooks

Hooks are called from places that exist anyway, so a site without hooks
pays nothing:

- **`on_publish(|cx, report| ...)`** (`publish`): after each publish
  batch that did something, in `serve`. The batch follows the change log:
  content saved in the admin, the API or the command line, and rows of
  tables with the change-log trigger. The report has the changed tags, the
  pages re-rendered (`rendered`) and removed (`deleted`), and the reason.
  Hooks run one batch at a time, in order, beside the publisher (a slow
  hook delays no page); an error is logged.
- **`on_form_submitted(|cx, submission| ...)`** (`forms`): after a form
  post was accepted and stored, and after the engine queued its receipt and
  notification mail; also for a post that waited in the
  [spool](/docs/forms/) while the database was unavailable, when it is
  written. An error is logged and does not change the visitor's answer.
  Delivery is at least once: put slow or retryable work in a job.

Hooks before a save (validations, signals on content types) are part of
the model lifecycle planned for v0.9.

### Migrations

`migration("0001_create_signups", include_str!(...))` embeds a migration in
the binary, recorded as `app/0001_create_signups`. `migrate` applies the
engine's migrations, then the app's, then the site folder's
(`migrations/postgres/`, written by `types sync` or by hand), in one
history with checksums, as before. Numbers are unique within the app's
migrations.

### Commands

`command(clap::Command, run)` adds a command to the binary. It gets a
`--site` argument (default `.`) unless it defines one, and the engine's
command names are refused.

```rust
use engine_cli::app::TaskContext;

fn list_signups(cx: &TaskContext) -> anyhow::Result<()> {
    let c = cx.connect()?; // the site's database, migrations checked
    let n: i64 = c.rt.block_on(
        engine_cli::engine_db::query("SELECT count(*) FROM app_signups").scalar(&c.db),
    )?;
    println!("{n} signup(s)");
    Ok(())
}
```

## Speed

Nothing registered is looked up per request beyond what axum's router does
for the engine's own routes: handlers are mounted on the same router at
startup, queries are prepared once and called directly by the template
function, job handlers join the engine's in one table, and hooks are called
only where the engine already has the event (a site without hooks spawns no
task and makes no call). A site that registers nothing is the stock engine.

## Testing an app crate

The example's tests (`examples/custom-app/tests/`) run the crate's binary
against a throwaway PostgreSQL database
(`engine_db::testing::TestDb`, which needs `ENGINE_TEST_DATABASE_URL` and
skips otherwise) and drive it over HTTP: migrate, build, serve, post to the
handler, wait for the job and the republished page, and run the command.
