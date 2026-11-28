//! The App crate API: a site as a Rust crate of its own.
//!
//! A site crate's `main.rs` builds an [`App`], registers what the site adds
//! to the engine, and runs it:
//!
//! ```text
//! fn main() -> std::process::ExitCode {
//!     engine_cli::App::new()
//!         // .route("/signups/", post(add_signup))      HTTP handlers
//!         // .query("recent", "SELECT ... LIMIT $1")     named SQL for templates
//!         // .job("signup.welcome", welcome)             job handlers
//!         // .on_publish(log_publish)                    lifecycle hooks
//!         // .command(clap::Command::new("stats"), stats)  extra commands
//!         .run()
//! }
//! ```
//!
//! [`App::run`] is the whole `engine` command line (`new`, `dev`, `build`,
//! `serve`, `doctor`, `migrate`...) plus the site's own commands; the stock
//! `engine` binary is `App::new().run()`.
//!
//! Everything is registered before the command line is parsed and resolved
//! once, at startup: handlers are mounted on the server's router next to
//! the engine's own routes (no lookup of their own per request), queries
//! are prepared against the database when the site starts (a typo fails
//! the start, not a page), job handlers join the engine's in the worker's
//! table, hooks are called from the places that already exist (the publish
//! listener's batch reports, accepted form posts). A site that registers
//! nothing gets exactly the stock engine.
//!
//! What a site can register depends on the features it builds the engine
//! with (`engine-cli = { ..., features = ["app"] }`): handlers, queries and
//! migrations need the database (any of `content`, `auth`, `forms`, `jobs`,
//! `media`), jobs need `jobs`, [`App::on_publish`] needs `publish` and
//! [`App::on_form_submitted`] needs `forms`.
//!
//! The documentation's "App crates" page walks through an example
//! (`examples/custom-app`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;

use anyhow::Result;

#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
pub use server_side::*;

/// A site's additions to the engine, registered before [`App::run`].
#[must_use = "an App does nothing until `.run()`"]
pub struct App {
    reg: Registry,
    tasks: Vec<Task>,
    errors: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// What the engine looks up at startup (set once by [`App::run`]).
#[derive(Default)]
pub(crate) struct Registry {
    #[cfg(any(
        feature = "content",
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media"
    ))]
    pub(crate) server: server_side::Parts,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// The site crate's registrations (empty for the stock binary, and in unit
/// tests that never ran an [`App`]).
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
pub(crate) fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::default)
}

impl App {
    /// An app with nothing registered: the stock `engine`.
    pub fn new() -> Self {
        App {
            reg: Registry::default(),
            tasks: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// An extra command, `<binary> <name> [--site DIR] ...`, run by `run`
    /// with the parsed arguments. A `--site` argument (default `.`) is
    /// added unless the command defines one. The name may not be one of the
    /// engine's own commands.
    pub fn command<F>(mut self, command: clap::Command, run: F) -> Self
    where
        F: Fn(&TaskContext) -> Result<()> + Send + Sync + 'static,
    {
        let name = command.get_name().to_string();
        if crate::engine_commands().contains(&name) {
            self.errors.push(format!(
                "command `{name}`: the engine has a command of that name"
            ));
        } else if self.tasks.iter().any(|t| t.name() == name) {
            self.errors
                .push(format!("command `{name}` is registered twice"));
        }
        let command = if command.get_arguments().any(|a| a.get_id() == "site") {
            command
        } else {
            command.arg(
                clap::Arg::new("site")
                    .short('s')
                    .long("site")
                    .default_value(".")
                    .value_parser(clap::value_parser!(PathBuf))
                    .help("Site folder (the one holding site.toml)."),
            )
        };
        self.tasks.push(Task {
            command,
            run: Box::new(run),
        });
        self
    }

    /// Run the command line: the engine's commands plus the site's. Any
    /// registration that was refused (a reserved path, a duplicate name...)
    /// is reported here and nothing runs.
    pub fn run(self) -> ExitCode {
        if !self.errors.is_empty() {
            for e in &self.errors {
                eprintln!("error: {e}");
            }
            return ExitCode::FAILURE;
        }
        if REGISTRY.set(self.reg).is_err() {
            eprintln!("error: App::run was called twice");
            return ExitCode::FAILURE;
        }
        crate::main_with(&self.tasks)
    }
}

type TaskFn = Box<dyn Fn(&TaskContext) -> Result<()> + Send + Sync>;

/// A site crate's command (see [`App::command`]).
pub(crate) struct Task {
    command: clap::Command,
    run: TaskFn,
}

impl Task {
    pub(crate) fn name(&self) -> &str {
        self.command.get_name()
    }

    pub(crate) fn command(&self) -> clap::Command {
        self.command.clone()
    }

    pub(crate) fn run(&self, args: &clap::ArgMatches) -> Result<()> {
        let site = args
            .try_get_one::<PathBuf>("site")
            .ok()
            .flatten()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));
        (self.run)(&TaskContext {
            args: args.clone(),
            site,
        })
    }
}

/// What a site's command gets.
pub struct TaskContext {
    /// The command's parsed arguments.
    pub args: clap::ArgMatches,
    /// The site folder (`--site`).
    pub site: PathBuf,
}

impl TaskContext {
    /// Connect to the site's database (`[database]`, or
    /// `ENGINE_DATABASE_URL`), failing unless every migration is applied.
    /// The connection comes with a runtime to run queries on
    /// (`conn.rt.block_on(...)`).
    #[cfg(any(
        feature = "content",
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media"
    ))]
    pub fn connect(&self) -> Result<Conn> {
        let c = crate::db::connect(&self.site)?;
        c.ensure_migrated()?;
        Ok(c)
    }
}

#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
mod server_side {
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use anyhow::{Context as _, Result};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::MethodRouter;
    use engine_core::builder::SiteExtension;
    use engine_core::tags::{self, CacheTag};
    use engine_db::{Arg, Db, Migration};
    use minijinja::Value;
    use minijinja::value::{Rest, ValueKind};

    pub use crate::db::Conn;

    use super::App;
    use crate::server::CurrentRenderer;

    /// Paths the engine's own routes live under; a site's handlers go
    /// elsewhere.
    const RESERVED: &[&str] = &["/_engine/", "/admin", "/account", "/api/v1/", "/media/"];

    /// Job kinds of the engine's own handlers.
    #[cfg(feature = "jobs")]
    const RESERVED_JOBS: &[&str] = &["engine.", "mail.", "payments."];

    #[cfg(any(feature = "jobs", feature = "publish", feature = "forms"))]
    pub(crate) type BoxFuture<T> = std::pin::Pin<Box<dyn Future<Output = T> + Send>>;

    /// A job handler of the site.
    #[cfg(feature = "jobs")]
    pub(crate) type JobFn = Arc<dyn Fn(JobContext) -> BoxFuture<Result<()>> + Send + Sync>;

    /// A hook after a publish batch.
    #[cfg(feature = "publish")]
    pub(crate) type PublishHook = Arc<
        dyn Fn(Context, Arc<engine_publish::PublishReport>) -> BoxFuture<Result<()>> + Send + Sync,
    >;

    /// A hook after an accepted form post.
    #[cfg(feature = "forms")]
    pub(crate) type FormHook =
        Arc<dyn Fn(Context, FormSubmitted) -> BoxFuture<Result<()>> + Send + Sync>;

    /// The registrations that need the database.
    #[derive(Default)]
    pub(crate) struct Parts {
        pub(crate) routes: Vec<(String, MethodRouter<Context>)>,
        pub(crate) queries: Arc<BTreeMap<String, NamedQuery>>,
        pub(crate) migrations: Vec<Migration>,
        #[cfg(feature = "jobs")]
        pub(crate) jobs: Vec<(String, JobFn)>,
        #[cfg(feature = "publish")]
        pub(crate) on_publish: Vec<PublishHook>,
        #[cfg(feature = "forms")]
        pub(crate) on_form: Vec<FormHook>,
    }

    impl App {
        /// An HTTP handler at `path` (axum's syntax: `/notes/{id}/`), for
        /// the methods of `handler` (`axum::routing::get(f)`,
        /// `post(f).delete(g)`...). Handlers take any axum extractor,
        /// and `State<Context>` for the engine (database, sessions,
        /// rendering, cache tags, jobs). Mounted by `serve` next to the
        /// engine's routes and tried before published files; the paths of
        /// the engine's own routes (`/_engine/`, `/admin`, `/account`,
        /// `/api/v1/`, `/media/`) are refused.
        ///
        /// A handler that changes data on an unsafe method and trusts the
        /// session cookie must check the request's origin
        /// ([`engine_core::web::same_origin`]) or a CSRF token, as the
        /// engine's own forms do.
        pub fn route(mut self, path: &str, handler: MethodRouter<Context>) -> Self {
            if !path.starts_with('/') {
                self.errors
                    .push(format!("route `{path}`: paths start with `/`"));
            } else if let Some(r) = RESERVED.iter().find(|r| {
                // Whole segments: `/admin` and `/admin/...`, not
                // `/administration/`.
                let base = r.trim_end_matches('/');
                path == base
                    || path
                        .strip_prefix(base)
                        .is_some_and(|rest| rest.starts_with('/'))
            }) {
                self.errors.push(format!(
                    "route `{path}`: paths under `{r}` belong to the engine"
                ));
            } else if self.reg.server.routes.iter().any(|(p, _)| p == path) {
                self.errors.push(format!(
                    "route `{path}` is registered twice (give one `MethodRouter` all its methods)"
                ));
            }
            self.reg.server.routes.push((path.to_string(), handler));
            self
        }

        /// A named SQL query for templates: `query("name", arg1, arg2...)`
        /// returns its rows as a list of maps (column name to value), the
        /// arguments bound as `$1`, `$2`... (integers as `bigint`, other
        /// numbers as `double precision`, strings as `text`, booleans,
        /// `none` as a text `NULL`, lists and maps as `jsonb`; cast in the
        /// SQL where a column needs another type: `$1::int`). One `SELECT`
        /// (or `WITH ... SELECT`), no trailing `;`. Prepared against the
        /// database when the site starts.
        ///
        /// `tags`: the cache tags a page using it depends on (`c:signups`
        /// for a table whose trigger feeds the change log as `signups`,
        /// see the docs), recorded on every call so published pages are
        /// re-rendered and cached fragments dropped when they change.
        pub fn query(mut self, name: &str, sql: &str, tags: &[&str]) -> Self {
            let valid = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !valid {
                self.errors.push(format!(
                    "query `{name}`: names are lowercase letters, digits and `_`"
                ));
            }
            let body = sql.trim().trim_end_matches(';').trim();
            if body.is_empty() {
                self.errors.push(format!("query `{name}`: no SQL"));
            }
            let mut parsed = Vec::new();
            for t in tags {
                match t.parse::<CacheTag>() {
                    Ok(t) => parsed.push(t),
                    Err(e) => self.errors.push(format!("query `{name}`: {e}")),
                }
            }
            let queries = Arc::make_mut(&mut self.reg.server.queries);
            if queries.contains_key(name) {
                self.errors
                    .push(format!("query `{name}` is registered twice"));
            }
            queries.insert(
                name.to_string(),
                NamedQuery {
                    name: name.to_string(),
                    // One JSON object per row, in the query's order (a
                    // plain projection of the subquery keeps its order).
                    sql: format!("SELECT to_jsonb(q) FROM ({body}) AS q"),
                    tags: parsed,
                },
            );
            self
        }

        /// A migration embedded in the site crate (`include_str!`), named
        /// like a migration file without `.sql` (`0001_create_signups`)
        /// and recorded as `app/<name>`. Applied by `migrate` after the
        /// engine's migrations and before the site folder's own
        /// (`migrations/postgres/`, which keep working as before).
        pub fn migration(mut self, name: &str, sql: &str) -> Self {
            if !engine_db::migrate::valid_file_name(&format!("{name}.sql")) {
                self.errors.push(format!(
                    "migration `{name}`: names are like `0001_create_things` (four digits, `_`, \
                     lowercase words)"
                ));
                return self;
            }
            // Only valid names are stored: `app/NNNN_...`.
            if self
                .reg
                .server
                .migrations
                .iter()
                .any(|m| m.id.get(4..8) == name.get(..4))
            {
                self.errors.push(format!(
                    "migration `{name}`: another app migration has the number {}",
                    &name[..4]
                ));
            }
            self.reg
                .server
                .migrations
                .push(Migration::new(format!("app/{name}"), sql));
            self
        }

        /// A handler for jobs of `kind` on the engine's queue (PostgreSQL,
        /// `SKIP LOCKED`; retries with backoff, dead after the last
        /// attempt). Queue them with [`Context::enqueue`] or
        /// [`engine_jobs::Jobs`]; they run in `serve`'s workers and in
        /// `jobs work`. Kinds are `[a-z][a-z0-9_.-]*`; `engine.`, `mail.`
        /// and `payments.` belong to the engine.
        #[cfg(feature = "jobs")]
        pub fn job<F, Fut>(mut self, kind: &str, handler: F) -> Self
        where
            F: Fn(JobContext) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Result<()>> + Send + 'static,
        {
            if !engine_jobs::valid_kind(kind) {
                self.errors.push(format!(
                    "job `{kind}`: kinds are `[a-z][a-z0-9_.-]`, at most 64 characters"
                ));
            } else if RESERVED_JOBS.iter().any(|r| kind.starts_with(r)) {
                self.errors
                    .push(format!("job `{kind}`: that prefix belongs to the engine"));
            } else if self.reg.server.jobs.iter().any(|(k, _)| k == kind) {
                self.errors
                    .push(format!("job `{kind}` is registered twice"));
            }
            let handler = Arc::new(handler);
            self.reg.server.jobs.push((
                kind.to_string(),
                Arc::new(move |cx| {
                    let h = handler.clone();
                    Box::pin(async move { h(cx).await })
                }),
            ));
            self
        }

        /// A hook called by `serve` after each publish batch that did
        /// something: content saved in the admin, the API or the command
        /// line, or any row of a table whose trigger feeds the change log,
        /// was published (the report lists the tags that changed and the
        /// pages re-rendered or removed). Hooks run one batch at a time,
        /// in order, off the publish path (a slow hook delays no page);
        /// an error is logged.
        #[cfg(feature = "publish")]
        pub fn on_publish<F, Fut>(mut self, hook: F) -> Self
        where
            F: Fn(Context, Arc<engine_publish::PublishReport>) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Result<()>> + Send + 'static,
        {
            let hook = Arc::new(hook);
            self.reg.server.on_publish.push(Arc::new(move |cx, r| {
                let h = hook.clone();
                Box::pin(async move { h(cx, r).await })
            }));
            self
        }

        /// A hook called after a form post was accepted and stored (after
        /// the engine's receipt and notification mail were queued), also
        /// for a post that waited in the spool while the database was
        /// unavailable, when it is written. An error is logged; the
        /// visitor's answer does not change. Delivery is at least once: do
        /// slow or retryable work in a job.
        #[cfg(feature = "forms")]
        pub fn on_form_submitted<F, Fut>(mut self, hook: F) -> Self
        where
            F: Fn(Context, FormSubmitted) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Result<()>> + Send + 'static,
        {
            let hook = Arc::new(hook);
            self.reg.server.on_form.push(Arc::new(move |cx, s| {
                let h = hook.clone();
                Box::pin(async move { h(cx, s).await })
            }));
            self
        }
    }

    /// An accepted form post (see [`App::on_form_submitted`]).
    #[cfg(feature = "forms")]
    #[derive(Debug, Clone)]
    pub struct FormSubmitted {
        /// The form's name (`forms/<name>.toml`, or the admin's).
        pub form: String,
        /// The stored submission's id.
        pub id: i64,
        /// The validated fields.
        pub data: serde_json::Value,
        /// The signed-in visitor who sent it.
        pub user_id: Option<i64>,
    }

    /// What a site's job handler gets.
    #[cfg(feature = "jobs")]
    #[derive(Clone)]
    pub struct JobContext {
        /// The job (id, kind, payload, attempt).
        pub job: engine_jobs::JobContext,
        /// The site's database.
        pub db: Db,
        /// The queue, to queue follow-up jobs.
        pub jobs: engine_jobs::Jobs,
        /// Email through the job queue (when `[mail]` and an SMTP server
        /// are configured).
        #[cfg(feature = "mail")]
        pub mailer: Option<engine_mail::Mailer>,
    }

    /// Register the site's job handlers next to the engine's.
    #[cfg(feature = "jobs")]
    pub(crate) fn register_jobs(
        handlers: &mut engine_jobs::Handlers,
        db: &Db,
        #[cfg(feature = "mail")] mailer: Option<&engine_mail::Mailer>,
    ) {
        for (kind, f) in &super::registry().server.jobs {
            let f = f.clone();
            let db = db.clone();
            let jobs = engine_jobs::Jobs::new(db.clone());
            #[cfg(feature = "mail")]
            let mailer = mailer.filter(|m| m.configured()).cloned();
            handlers.register(kind, move |job| {
                f(JobContext {
                    job,
                    db: db.clone(),
                    jobs: jobs.clone(),
                    #[cfg(feature = "mail")]
                    mailer: mailer.clone(),
                })
            });
        }
    }

    // ----- handlers ---------------------------------------------------------

    /// What the site's handlers and hooks get from the engine: the
    /// database, sessions, rendering through the site's templates, cache
    /// tags and the job queue. Cheap to clone (`State<Context>` in a
    /// handler).
    #[derive(Clone)]
    pub struct Context(Arc<Inner>);

    struct Inner {
        db: Db,
        site: std::path::PathBuf,
        public_url: String,
        current: CurrentRenderer,
        /// Renders at once: as many as the pool has connections (each
        /// holds one), like `request` pages.
        renders: Arc<tokio::sync::Semaphore>,
        #[cfg(feature = "auth")]
        auth: engine_auth::Auth,
        #[cfg(feature = "jobs")]
        jobs: engine_jobs::Jobs,
        #[cfg(feature = "mail")]
        mailer: Option<engine_mail::Mailer>,
    }

    /// The parts `serve` builds a [`Context`] from.
    pub(crate) struct ContextParts {
        pub db: Db,
        pub site: std::path::PathBuf,
        pub public_url: String,
        pub current: CurrentRenderer,
        #[cfg(feature = "auth")]
        pub auth: engine_auth::Auth,
        #[cfg(feature = "mail")]
        pub mailer: Option<engine_mail::Mailer>,
    }

    /// A signed-in visitor.
    #[cfg(feature = "auth")]
    pub type Session = engine_auth::Session;

    impl Context {
        pub(crate) fn new(p: ContextParts) -> Context {
            let renders = Arc::new(tokio::sync::Semaphore::new(
                p.db.max_connections().max(1) as usize
            ));
            Context(Arc::new(Inner {
                #[cfg(feature = "jobs")]
                jobs: engine_jobs::Jobs::new(p.db.clone()),
                db: p.db,
                site: p.site,
                public_url: p.public_url,
                current: p.current,
                renders,
                #[cfg(feature = "auth")]
                auth: p.auth,
                #[cfg(feature = "mail")]
                mailer: p.mailer,
            }))
        }

        /// The site's database (the pool; health, outages and the sleeping
        /// database handled as for the engine's own queries).
        pub fn db(&self) -> &Db {
            &self.0.db
        }

        /// The site folder.
        pub fn site(&self) -> &Path {
            &self.0.site
        }

        /// The site's public address (`ENGINE_PUBLIC_URL`, else `[site]
        /// base_url`).
        pub fn public_url(&self) -> &str {
            &self.0.public_url
        }

        /// The signed-in visitor of a request, if any (two-factor sign-in
        /// complete).
        #[cfg(feature = "auth")]
        pub async fn session(&self, headers: &axum::http::HeaderMap) -> Option<Session> {
            self.0.auth.signed_in(headers).await
        }

        /// Accounts: users, roles, sessions.
        #[cfg(feature = "auth")]
        pub fn auth(&self) -> &engine_auth::Auth {
            &self.0.auth
        }

        /// Render the site template `name` (in `templates/`, with the
        /// site's layout, components and template functions) with the site
        /// globals plus `ctx`. Its queries share one connection; the cache
        /// tags it recorded come back with the page.
        pub async fn render(&self, name: &str, ctx: Value) -> Result<Page> {
            let r = (self.0.current)();
            let db = self.0.db.clone();
            let name = name.to_string();
            let _permit = self.0.renders.acquire().await?;
            let (html, tags) =
                tokio::task::spawn_blocking(move || db.pinned(|| r.render_template(&name, &ctx)))
                    .await??;
            Ok(Page {
                html,
                tags: tags.iter().map(ToString::to_string).collect(),
            })
        }

        /// Drop the cached fragments and request-page ETags that recorded
        /// any of `tags` (`c:signups`, `i:signups:3`; `*` for all), after a
        /// change the change log does not carry. Changes to tables with the
        /// change-log trigger need no call: the publish listener does it.
        pub fn evict(&self, tags: &[&str]) {
            if let Some(c) = (self.0.current)().fragments() {
                let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
                c.evict(&tags);
            }
        }

        /// Queue a job (see [`App::job`]).
        #[cfg(feature = "jobs")]
        pub async fn enqueue(&self, kind: &str, payload: serde_json::Value) -> Result<i64> {
            self.0.jobs.enqueue(kind, payload).await
        }

        /// The job queue.
        #[cfg(feature = "jobs")]
        pub fn jobs(&self) -> &engine_jobs::Jobs {
            &self.0.jobs
        }

        /// Email through the job queue (when `[mail]` and an SMTP server
        /// are configured).
        #[cfg(feature = "mail")]
        pub fn mailer(&self) -> Option<&engine_mail::Mailer> {
            self.0.mailer.as_ref().filter(|m| m.configured())
        }

        /// Run a named query (see [`App::query`]): one JSON object per row.
        pub async fn query(&self, name: &str, args: Vec<Arg>) -> Result<Vec<serde_json::Value>> {
            let q = super::registry()
                .server
                .queries
                .get(name)
                .with_context(|| format!("no query named `{name}`"))?;
            q.run(&self.0.db, args).await
        }
    }

    /// A page rendered by [`Context::render`]. As a response: `200`,
    /// HTML, `Cache-Control: private, no-store` (set your own headers on
    /// `html` otherwise).
    #[derive(Debug, Clone)]
    pub struct Page {
        pub html: String,
        /// The cache tags its render recorded.
        pub tags: Vec<String>,
    }

    impl IntoResponse for Page {
        fn into_response(self) -> Response {
            (
                [
                    (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (axum::http::header::CACHE_CONTROL, "private, no-store"),
                ],
                self.html,
            )
                .into_response()
        }
    }

    /// An error from a handler, for `?`: logged with its chain, answered
    /// with a bare `500` (no detail to the visitor), or `503` with
    /// `Retry-After` when the database was unavailable.
    #[derive(Debug)]
    pub struct Error(pub anyhow::Error);

    impl<E: Into<anyhow::Error>> From<E> for Error {
        fn from(e: E) -> Self {
            Error(e.into())
        }
    }

    impl IntoResponse for Error {
        fn into_response(self) -> Response {
            if engine_db::health::is_unavailable_any(&self.0) {
                return engine_core::serve::unavailable(engine_core::serve::UNAVAILABLE_PAGE);
            }
            eprintln!("handler: {:#}", self.0);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "Something went wrong.\n",
            )
                .into_response()
        }
    }

    /// What a handler returns with `?` inside: `Result<impl IntoResponse>`.
    pub type HandlerResult<T> = std::result::Result<T, Error>;

    /// The site's routes on one router, with the engine's [`Context`].
    /// `None` when the site registered none.
    pub(crate) fn router(cx: &Context) -> Option<axum::Router> {
        let routes = &super::registry().server.routes;
        if routes.is_empty() {
            return None;
        }
        let mut r = axum::Router::new();
        for (path, mr) in routes {
            r = r.route(path, mr.clone());
        }
        Some(r.with_state(cx.clone()))
    }

    /// Run the site's publish hooks on every report of `publisher`, in a
    /// task of their own (none when there are no hooks).
    #[cfg(feature = "publish")]
    pub(crate) fn spawn_publish_hooks(publisher: &engine_publish::Publisher, cx: &Context) {
        let hooks = &super::registry().server.on_publish;
        if hooks.is_empty() {
            return;
        }
        let mut reports = publisher.subscribe();
        let cx = cx.clone();
        tokio::spawn(async move {
            loop {
                let r = match reports.recv().await {
                    Ok(r) => Arc::new(r),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("on_publish: {n} batch report(s) skipped (hooks too slow)");
                        continue;
                    }
                    Err(_) => break,
                };
                for h in &super::registry().server.on_publish {
                    if let Err(e) = h(cx.clone(), r.clone()).await {
                        eprintln!("on_publish hook: {e:#}");
                    }
                }
            }
        });
    }

    /// Run the site's form hooks for one accepted post.
    #[cfg(feature = "forms")]
    pub(crate) async fn form_submitted(cx: &Context, sub: &engine_forms::Submission) {
        for h in &super::registry().server.on_form {
            let s = FormSubmitted {
                form: sub.form.clone(),
                id: sub.id,
                data: sub.data.clone(),
                user_id: sub.user_id,
            };
            if let Err(e) = h(cx.clone(), s).await {
                eprintln!("on_form_submitted hook ({}): {e:#}", sub.form);
            }
        }
    }

    /// Whether the site registered anything `serve` needs a database for.
    pub(crate) fn needs_database() -> bool {
        let p = &super::registry().server;
        !p.routes.is_empty() || !p.queries.is_empty()
    }

    // ----- named queries ----------------------------------------------------

    /// A query registered with [`App::query`].
    #[derive(Debug, Clone)]
    pub(crate) struct NamedQuery {
        name: String,
        /// Wrapped: one `jsonb` per row.
        sql: String,
        tags: Vec<CacheTag>,
    }

    impl NamedQuery {
        async fn run(&self, db: &Db, args: Vec<Arg>) -> Result<Vec<serde_json::Value>> {
            let mut q = engine_db::sql::query(self.sql.as_str());
            for a in args {
                q = q.bind(a);
            }
            q.scalars::<serde_json::Value>(db)
                .await
                .with_context(|| format!("query `{}`", self.name))
        }
    }

    /// A template value as a bound argument.
    fn arg(v: &Value) -> Arg {
        match v.kind() {
            ValueKind::Undefined | ValueKind::None => Arg::Text(None),
            ValueKind::Bool => Arg::Bool(Some(v.is_true())),
            ValueKind::Number => match i64::try_from(v.clone()) {
                Ok(i) => Arg::I64(Some(i)),
                Err(_) => Arg::F64(f64::try_from(v.clone()).ok()),
            },
            ValueKind::String => Arg::Text(v.as_str().map(str::to_string)),
            _ => Arg::Json(serde_json::to_value(v).ok()),
        }
    }

    /// Prepare every named query against the database (a typo or a
    /// missing table fails the start with the query's name).
    pub(crate) async fn check_queries(db: &Db) -> Result<()> {
        for q in super::registry().server.queries.values() {
            engine_db::sql::check(db, &q.sql)
                .await
                .with_context(|| format!("named query `{}` does not prepare", q.name))?;
        }
        Ok(())
    }

    /// `query(name, args...)` in templates, when the site registered
    /// queries.
    pub(crate) fn query_extension(
        db: &Db,
        rt: &tokio::runtime::Handle,
    ) -> Option<Arc<dyn SiteExtension>> {
        let queries = super::registry().server.queries.clone();
        if queries.is_empty() {
            return None;
        }
        Some(Arc::new(QueryExtension {
            queries,
            db: db.clone(),
            rt: rt.clone(),
            memo: Arc::default(),
        }))
    }

    type Memo = HashMap<(String, String), Value>;

    #[derive(Clone)]
    struct QueryExtension {
        queries: Arc<BTreeMap<String, NamedQuery>>,
        db: Db,
        rt: tokio::runtime::Handle,
        /// During a whole-site render: each (query, arguments) run once.
        memo: Arc<Mutex<Option<Memo>>>,
    }

    impl QueryExtension {
        fn call(&self, name: &str, args: &[Value]) -> Result<Value, minijinja::Error> {
            let q = self.queries.get(name).ok_or_else(|| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("query(): no query named `{name}` (see App::query)"),
                )
            })?;
            for t in &q.tags {
                tags::record(t.clone());
            }
            let key = (name.to_string(), format!("{args:?}"));
            if let Some(v) = self
                .memo
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .and_then(|m| m.get(&key))
            {
                return Ok(v.clone());
            }
            let rows = self
                .rt
                .block_on(q.run(&self.db, args.iter().map(arg).collect()))
                .map_err(|e| engine_core::templates::fn_error_in(&format!("query({name})"), e))?;
            let v = Value::from_serialize(&rows);
            if let Some(m) = self.memo.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
                m.insert(key, v.clone());
            }
            Ok(v)
        }
    }

    impl SiteExtension for QueryExtension {
        fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
            let me = self.clone();
            env.add_function("query", move |name: String, args: Rest<Value>| {
                me.call(&name, &args)
            });
        }

        fn begin_bulk(&self) -> Result<()> {
            *self.memo.lock().unwrap_or_else(|p| p.into_inner()) = Some(HashMap::new());
            Ok(())
        }

        fn end_bulk(&self) {
            *self.memo.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn errors(app: App) -> Vec<String> {
            app.errors
        }

        #[test]
        fn reserved_and_invalid_registrations_are_refused() {
            let h = || axum::routing::get(|| async { "x" });
            let e = errors(
                App::new()
                    .route("/admin/x/", h())
                    .route("/_engine/health", h())
                    .route("no-slash", h())
                    .route("/ok/", h())
                    .route("/ok/", h())
                    .query("Bad-Name", "SELECT 1", &[])
                    .query("fine", "SELECT 1", &["nonsense"])
                    .migration("1_x", "SELECT 1")
                    .migration("0001_a", "SELECT 1")
                    .migration("0001_b", "SELECT 1"),
            );
            assert_eq!(e.len(), 8, "{e:#?}");
            assert!(e[0].contains("belong to the engine"));
            assert!(e[3].contains("registered twice"));
            assert!(e[7].contains("number 0001"));
            let ok = errors(
                App::new()
                    .route("/administration-notes/", h())
                    .route("/notes/{id}/", h())
                    .query("recent", "SELECT 1;", &["c:signups", "i:note:3"])
                    .migration("0001_a", "SELECT 1")
                    .migration("0002_b", "SELECT 1"),
            );
            assert!(ok.is_empty(), "{ok:#?}");
        }

        #[cfg(feature = "jobs")]
        #[test]
        fn engine_job_kinds_are_reserved() {
            let e = errors(
                App::new()
                    .job("mail.send", |_| async { Ok(()) })
                    .job("Bad", |_| async { Ok(()) })
                    .job("signup.welcome", |_| async { Ok(()) }),
            );
            assert_eq!(e.len(), 2, "{e:#?}");
        }

        #[test]
        fn template_values_bind_with_their_types() {
            assert_eq!(arg(&Value::from(5)), Arg::I64(Some(5)));
            assert_eq!(arg(&Value::from(1.5)), Arg::F64(Some(1.5)));
            assert_eq!(arg(&Value::from("a")), Arg::Text(Some("a".into())));
            assert_eq!(arg(&Value::from(true)), Arg::Bool(Some(true)));
            assert_eq!(arg(&Value::from(())), Arg::Text(None));
            assert_eq!(
                arg(&Value::from(vec![1, 2])),
                Arg::Json(Some(serde_json::json!([1, 2])))
            );
        }

        #[test]
        fn queries_are_wrapped_to_rows_of_json() {
            let app = App::new().query("recent", " SELECT a FROM t ORDER BY a ; ", &[]);
            let q = &app.reg.server.queries["recent"];
            assert_eq!(
                q.sql,
                "SELECT to_jsonb(q) FROM (SELECT a FROM t ORDER BY a) AS q"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_commands_may_not_shadow_the_engine() {
        let app = App::new()
            .command(clap::Command::new("serve"), |_| Ok(()))
            .command(clap::Command::new("stats"), |_| Ok(()))
            .command(clap::Command::new("stats"), |_| Ok(()));
        assert_eq!(app.errors.len(), 2, "{:#?}", app.errors);
        // `--site` is added.
        let t = &app.tasks[1];
        let m = t
            .command()
            .try_get_matches_from(["stats", "--site", "x"])
            .unwrap();
        assert_eq!(m.get_one::<PathBuf>("site").unwrap(), &PathBuf::from("x"));
    }
}
