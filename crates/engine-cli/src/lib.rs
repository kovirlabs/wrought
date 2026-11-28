//! The Hammerwright command line, as a library: the `engine` binary and
//! every site crate's own binary are built from [`App`].
//!
//! A site crate (a Rust package of its own, with the site's folder next to
//! it) depends on this crate with the features it needs and registers its
//! HTTP handlers, named SQL queries, job handlers, lifecycle hooks and
//! extra commands on an [`App`]; [`App::run`] then gives the whole `engine`
//! command line (`new`, `dev`, `build`, `serve`, `doctor`, `migrate`...)
//! plus the site's own commands. Compile-time crates are the plugin model:
//! everything is registered once, at startup, and resolved before the
//! first request. See the "App crates" page of the documentation.

pub mod app;
pub use app::App;

// The engine's crates and the libraries its API is made of, at the versions
// this engine uses, for site crates.
#[cfg(feature = "auth")]
pub use engine_auth;
#[cfg(feature = "db-postgres")]
pub use engine_db;
#[cfg(feature = "forms")]
pub use engine_forms;
#[cfg(feature = "jobs")]
pub use engine_jobs;
#[cfg(feature = "mail")]
pub use engine_mail;
#[cfg(feature = "publish")]
pub use engine_publish;
pub use {anyhow, axum, clap, engine_core, minijinja, serde_json, tokio};

#[cfg(feature = "api")]
mod api;
mod check;
#[cfg(feature = "content")]
mod cms;
mod components;
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
mod db;
mod dev;
mod doctor;
#[cfg(feature = "forms")]
mod formhooks;
mod generate;
#[cfg(feature = "jobs")]
mod jobs;
#[cfg(feature = "media")]
mod media;
mod new;
mod pagefind;
#[cfg(feature = "payments")]
mod payments;
#[cfg(any(feature = "workflow", feature = "visual-editor"))]
mod preview;
#[cfg(any(feature = "auth", feature = "forms"))]
mod request;
mod routes;
#[cfg(feature = "s3")]
mod s3;
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
mod server;
#[cfg(feature = "auth")]
mod user;
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
mod wiring;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use engine_core::builder::{self, BuildOptions, BuildReport};
use engine_core::serve::{self, ServeConfig};

/// Shown by `engine --version`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\npreset: ",
    preset!(),
    extras!(),
    "\nbundled: TagBrush.css 2.0.1, TagAction 0.2.0 (Apache-2.0, see /_engine/licenses/)",
    "\nexternal: @kovirlabs/tagcheck 0.1.3 for `engine check` (needs Node.js)"
);

#[cfg(feature = "app")]
macro_rules! preset {
    () => {
        "app (features: db-postgres, content, publish, auth, roles, admin, workflow, media, search, s3, forms, jobs, mail, api; render modes: build, publish, request)"
    };
}
#[cfg(all(feature = "cms", not(feature = "app")))]
macro_rules! preset {
    () => {
        "cms (features: db-postgres, content, publish, auth, roles, admin, workflow, media, search, s3; render modes: build, publish, request)"
    };
}
#[cfg(not(feature = "cms"))]
macro_rules! preset {
    () => {
        "static (render modes: build)"
    };
}
use preset;

// Features outside the presets that this binary also has.
#[cfg(all(feature = "oidc", feature = "payments"))]
macro_rules! extras {
    () => {
        "\nalso: oidc, payments"
    };
}
#[cfg(all(feature = "oidc", not(feature = "payments")))]
macro_rules! extras {
    () => {
        "\nalso: oidc"
    };
}
#[cfg(all(not(feature = "oidc"), feature = "payments"))]
macro_rules! extras {
    () => {
        "\nalso: payments"
    };
}
#[cfg(not(any(feature = "oidc", feature = "payments")))]
macro_rules! extras {
    () => {
        ""
    };
}
use extras;

/// Commands that need features this build may lack, with the feature.
const FEATURE_COMMANDS: &[(&str, &str)] = &[
    ("migrate", "db"),
    ("user", "auth"),
    ("types", "content"),
    ("content", "content"),
    ("rebuild", "publish"),
    ("jobs", "jobs"),
    ("media", "media"),
    ("search", "search"),
    ("api", "api"),
];

#[derive(Parser)]
#[command(
    name = "engine",
    version,
    long_version = LONG_VERSION,
    about = "Hammerwright: one Rust engine for static sites and web apps",
    after_help = "Docs: run `engine dev --site docs` in the Hammerwright repository."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
pub(crate) struct SiteArg {
    /// Site folder (the one holding site.toml).
    #[arg(short, long, default_value = ".")]
    pub(crate) site: PathBuf,
}

#[derive(clap::Args, Clone)]
struct Listen {
    /// Address to bind. Defaults to localhost only; use 0.0.0.0 to expose it.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(short, long, default_value_t = 4400)]
    port: u16,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new site from a preset.
    New {
        /// Folder to create.
        #[arg(required_unless_present = "theme")]
        name: Option<PathBuf>,
        /// static, cms (needs a cms or app build) or app (needs an app build).
        #[arg(long, default_value = "static")]
        preset: String,
        /// A starter theme laid over the preset: docs, business or
        /// dashboard (`--theme list` shows them).
        #[arg(long)]
        theme: Option<String>,
    },
    /// Render every `build` and `publish` route to the output folder.
    Build {
        #[command(flatten)]
        site: SiteArg,
        /// Output folder (default: [site] output in site.toml).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Include pages marked `draft = true`.
        #[arg(long)]
        drafts: bool,
        /// List every route with its template and cache tags.
        #[arg(short, long)]
        verbose: bool,
        /// Render threads (default: ENGINE_BUILD_JOBS, else one per CPU).
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Print how long each phase took (connect, search index, render,
        /// write, tag index...), to stderr.
        #[arg(long)]
        timings: bool,
    },
    /// Build, serve, watch for changes, rebuild and live-reload the browser.
    Dev {
        #[command(flatten)]
        site: SiteArg,
        #[command(flatten)]
        listen: Listen,
        /// Include pages marked `draft = true`.
        #[arg(long)]
        drafts: bool,
        /// Don't run TagCheck after each build.
        #[arg(long)]
        no_check: bool,
    },
    /// Serve a built site. With database features (cms and app builds),
    /// also render missing publish pages, run the publish listener, render
    /// `request` pages, take form posts and run job workers.
    Serve {
        #[command(flatten)]
        site: SiteArg,
        /// Folder to serve (default: the site's output folder).
        #[arg(short, long)]
        dir: Option<PathBuf>,
        #[command(flatten)]
        listen: Listen,
        /// Don't run job workers in this process (run `engine jobs work`
        /// elsewhere).
        #[arg(long)]
        no_workers: bool,
    },
    /// Print the limits sized from this machine (threads, database pool,
    /// caches, background work), each with its formula and whether the
    /// site's configuration set it.
    Doctor {
        #[command(flatten)]
        site: SiteArg,
    },
    /// Check the built site's HTML with TagCheck (semantic HTML and access checks).
    Check {
        #[command(flatten)]
        site: SiteArg,
        /// Folder to check (default: the site's output folder).
        #[arg(short, long)]
        dir: Option<PathBuf>,
        /// Also render pages in Chromium for contrast and overflow checks
        /// (needs Playwright next to TagCheck).
        #[arg(long)]
        browser: bool,
        /// Fail on warnings too.
        #[arg(long)]
        strict: bool,
    },
    /// List the component library of this build (and the site's own
    /// components and overrides) and the site's design tokens.
    Components {
        #[command(flatten)]
        site: SiteArg,
        /// Print JSON: every component's version, group, features,
        /// arguments, assets and editor hints, and the design tokens.
        #[arg(long)]
        json: bool,
    },
    /// Scaffold a content type, route, form, handler, job or mailer, or
    /// make the site its own Rust crate (`generate app`). Refuses to
    /// replace files unless --force; --dry-run lists them.
    Generate {
        #[command(subcommand)]
        cmd: generate::GenerateCmd,
    },
    /// List every route: the site's pages (with render mode, template and
    /// the cache tags of the last build), the app's handlers, form
    /// endpoints and the engine's own endpoints.
    Routes {
        #[command(flatten)]
        site: SiteArg,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Apply pending database migrations (engine and site).
    #[cfg(any(
        feature = "content",
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media"
    ))]
    Migrate {
        #[command(flatten)]
        site: SiteArg,
        /// List migrations and whether they are applied; change nothing.
        #[arg(long)]
        status: bool,
    },
    /// Content types: generate migrations from types/*.toml, list types.
    #[cfg(feature = "content")]
    Types {
        #[command(subcommand)]
        cmd: cms::TypesCmd,
    },
    /// Create, change, list and seed content from the command line.
    #[cfg(feature = "content")]
    Content {
        #[command(subcommand)]
        cmd: cms::ContentCmd,
    },
    /// Re-render every page in place, or only pages carrying a cache tag.
    #[cfg(feature = "publish")]
    Rebuild {
        #[command(flatten)]
        site: SiteArg,
        /// Only pages whose last render recorded this tag (repeatable),
        /// e.g. `--tag c:article`, `--tag i:article:3`, `--tag s:site`.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Accounts: create the first administrator, reset links, roles.
    #[cfg(feature = "auth")]
    User {
        #[command(subcommand)]
        cmd: user::UserCmd,
    },
    /// Background jobs: list, retry dead ones, run workers.
    #[cfg(feature = "jobs")]
    Jobs {
        #[command(subcommand)]
        cmd: jobs::JobsCmd,
    },
    /// The media library: add files, list them.
    #[cfg(feature = "media")]
    Media {
        #[command(subcommand)]
        cmd: media::MediaCmd,
    },
    /// The JSON API: tokens.
    #[cfg(feature = "api")]
    Api {
        #[command(subcommand)]
        cmd: api::ApiCmd,
    },
    /// Full-text search: rebuild the index, try a query.
    #[cfg(feature = "search")]
    Search {
        #[command(subcommand)]
        cmd: cms::SearchCmd,
    },
}

/// Run the command line with the commands of a site crate (`tasks`); the
/// registry is already in place ([`app::App::run`]).
fn main_with(tasks: &[app::Task]) -> ExitCode {
    let mut command = Cli::command();
    for t in tasks {
        command = command.subcommand(t.command());
    }
    let site_cmd = std::env::args()
        .nth(1)
        .is_some_and(|c| tasks.iter().any(|t| t.name() == c));
    if !site_cmd
        && let Some(cmd) = std::env::args().nth(1)
        && let Some((_, feature)) = FEATURE_COMMANDS.iter().find(|(c, _)| *c == cmd)
        && !engine_core::features::has_feature(feature)
    {
        eprintln!(
            "error: `engine {cmd}` needs the `{feature}` feature, which is not in this build \
             (preset `{}`). Build the engine with `cargo build --release --features app` (or \
             `cms`).",
            engine_core::features::PRESET
        );
        return ExitCode::FAILURE;
    }
    // Before any thread starts: allocator arenas for the CPUs this process
    // may use (see engine_core::memory).
    engine_core::memory::tune(8 * engine_core::resources::get().cpus);
    let started = std::time::Instant::now();
    let matches = command.get_matches();
    let result = match matches
        .subcommand()
        .and_then(|(name, m)| Some((tasks.iter().find(|t| t.name() == name)?, m)))
    {
        Some((task, m)) => task.run(m).map(|()| ExitCode::SUCCESS),
        None => match Cli::from_arg_matches(&matches) {
            Ok(cli) => run(cli),
            Err(e) => e.exit(),
        },
    };
    if engine_core::timings::enabled() {
        eprint!("{}", engine_core::timings::report(started.elapsed()));
    }
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The names of the engine's own commands (a site crate's may not reuse
/// them).
fn engine_commands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .chain(["help".to_string()])
        .collect()
}

/// `engine build` with every compiled feature's extensions.
fn build_any(site: &std::path::Path, opts: &BuildOptions) -> Result<builder::BuildReport> {
    #[cfg(feature = "content")]
    if let Some(report) = cms::build(site, opts)? {
        return Ok(report);
    }
    #[cfg(any(feature = "auth", feature = "forms", feature = "media"))]
    if let Some(report) = wiring::plain_build(site, opts)? {
        return Ok(report);
    }
    builder::build_site(site, opts)
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::New {
            name,
            preset,
            theme,
        } => {
            if theme.as_deref() == Some("list") {
                for (t, label, desc, presets) in new::themes() {
                    println!("{t:<10} {label}: {desc} (presets: {})", presets.join(", "));
                }
                return Ok(ExitCode::SUCCESS);
            }
            let Some(name) = name else {
                bail!("say which folder to create: engine new <name>");
            };
            new::new_site_with(&name, &preset, theme.as_deref())?;
            let with = theme
                .as_deref()
                .map(|t| format!(" with the `{t}` theme"))
                .unwrap_or_default();
            // Database presets get their own PostgreSQL (`new::local_database`).
            #[cfg(feature = "cms")]
            {
                let db_steps = if name.join("compose.yml").is_file() {
                    "\n  docker compose up -d --wait   # this site's PostgreSQL (compose.yml)\n  \
                     engine migrate && engine content seed"
                } else {
                    ""
                };
                println!(
                    "Created {} from the `{preset}` preset{with}.\nNext:\n  cd {}{db_steps}\n  \
                     engine dev",
                    name.display(),
                    name.display()
                );
            }
            #[cfg(not(feature = "cms"))]
            println!(
                "Created {} from the `{preset}` preset{with}.\nNext:\n  cd {}\n  engine dev",
                name.display(),
                name.display()
            );
        }
        Cmd::Build {
            site,
            output,
            drafts,
            verbose,
            jobs,
            timings,
        } => {
            if timings {
                engine_core::timings::enable();
            }
            let opts = BuildOptions {
                output,
                drafts,
                jobs,
            };
            #[cfg(all(feature = "publish", not(feature = "s3")))]
            {
                let (cfg, _) = builder::load_config(&site.site)?;
                engine_publish::PublishConfig::from_site(cfg.publish.as_ref())?;
                if engine_core::target::target_kind(cfg.publish.as_ref())?
                    == engine_core::target::TargetKind::S3
                {
                    bail!(
                        "[publish] target = \"s3\" needs the `s3` feature, which is not in this \
                         build (it is in the cms and app presets)"
                    );
                }
            }
            // Bucket settings are checked before anything is rendered.
            #[cfg(feature = "s3")]
            s3::open(&site.site, &builder::load_config(&site.site)?.0)?;
            let report = build_any(&site.site, &opts)?;
            print_report(&report, verbose);
            let (cfg, _) = builder::load_config(&site.site)?;
            if pagefind::wanted(&cfg) {
                let _p = engine_core::timings::phase("pagefind");
                pagefind::run(&builder::output_dir(&site.site, &cfg, &opts))?;
            }
            #[cfg(feature = "s3")]
            {
                let _p = engine_core::timings::phase("bucket sync");
                s3::sync_after_build(&site.site, &opts)?;
            }
        }
        Cmd::Dev {
            site,
            listen,
            drafts,
            no_check,
        } => dev::run(&site.site, addr(&listen), drafts, !no_check)?,
        Cmd::Serve {
            site,
            dir,
            listen,
            no_workers,
        } => {
            let addr = addr(&listen);
            #[cfg(any(
                feature = "content",
                feature = "auth",
                feature = "forms",
                feature = "jobs",
                feature = "media"
            ))]
            if dir.is_none() {
                output_folder(&site.site, None)?;
                let opts = server::ServeOptions {
                    workers: !no_workers,
                };
                if server::serve(&site.site, addr, opts)? {
                    return Ok(ExitCode::SUCCESS);
                }
            }
            let _ = no_workers;
            let dir = output_folder(&site.site, dir)?;
            println!(
                "Serving {} at http://{addr}/ (Ctrl+C to stop)",
                dir.display()
            );
            runtime()?.block_on(serve::serve(
                ServeConfig {
                    root: dir,
                    idle_exit: engine_core::host::engine_idle_exit()?,
                    ..Default::default()
                },
                addr,
                shutdown_signal(),
            ))?;
        }
        Cmd::Check {
            site,
            dir,
            browser,
            strict,
        } => {
            let dir = output_folder(&site.site, dir)?;
            return check::run_check(&dir, browser, strict);
        }
        Cmd::Components { site, json } => components::run(&site.site, json)?,
        Cmd::Doctor { site } => doctor::run(&site.site)?,
        Cmd::Generate { cmd } => generate::run(cmd)?,
        Cmd::Routes { site, json } => routes::run(&site.site, json)?,
        #[cfg(any(
            feature = "content",
            feature = "auth",
            feature = "forms",
            feature = "jobs",
            feature = "media"
        ))]
        Cmd::Migrate { site, status } => db::migrate_cmd(&site.site, status)?,
        #[cfg(feature = "content")]
        Cmd::Types { cmd } => return cms::types_cmd(cmd),
        #[cfg(feature = "content")]
        Cmd::Content { cmd } => cms::content_cmd(cmd)?,
        #[cfg(feature = "publish")]
        Cmd::Rebuild { site, tags } => cms::rebuild(&site.site, &tags)?,
        #[cfg(feature = "auth")]
        Cmd::User { cmd } => user::user_cmd(cmd)?,
        #[cfg(feature = "jobs")]
        Cmd::Jobs { cmd } => jobs::jobs_cmd(cmd)?,
        #[cfg(feature = "media")]
        Cmd::Media { cmd } => media::media_cmd(cmd)?,
        #[cfg(feature = "search")]
        Cmd::Search { cmd } => cms::search_cmd(cmd)?,
        #[cfg(feature = "api")]
        Cmd::Api { cmd } => api::api_cmd(cmd)?,
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn print_report(r: &BuildReport, verbose: bool) {
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    if verbose {
        for route in &r.routes {
            println!(
                "  {:<32} {:<7} {:<22} {}",
                route.path,
                route.mode.as_str(),
                route.template,
                route.tags.join(" ")
            );
        }
    }
    println!(
        "Built {} pages and {} static files into {} in {} ms",
        r.routes.len(),
        r.static_files,
        r.output.display(),
        r.elapsed.as_millis()
    );
}

fn addr(l: &Listen) -> SocketAddr {
    if !l.host.is_loopback() {
        eprintln!(
            "warning: listening on {} exposes this server beyond this machine",
            l.host
        );
    }
    SocketAddr::new(l.host, l.port)
}

/// The folder to serve or check: `--dir`, else the site's output folder.
fn output_folder(site: &Path, dir: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match dir {
        Some(d) => d,
        None => {
            let (cfg, _) = builder::load_config(site)?;
            builder::output_dir(site, &cfg, &BuildOptions::default())
        }
    };
    if !dir.is_dir() {
        bail!("{} does not exist; run `engine build` first", dir.display());
    }
    Ok(dir)
}

/// The async runtime, its threads sized by engine_core::limits (the site's
/// `[limits]` when its configuration was loaded first).
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    let l = engine_core::limits::current();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(l.worker_threads)
        .max_blocking_threads(l.blocking_threads)
        .enable_all()
        .build()
        .context("starting the async runtime")
}

/// Ctrl+C, or SIGTERM (systemd, Docker): stop taking connections, let
/// requests and jobs finish, exit 0.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match term.as_mut() {
                    Some(t) => { t.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    println!("\nStopping.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn long_version_names_the_preset() {
        assert!(LONG_VERSION.contains(&format!("preset: {}", engine_core::features::PRESET)));
    }

    #[test]
    fn long_version_matches_bundled_assets() {
        assert!(LONG_VERSION.contains(&format!(
            "TagBrush.css {}",
            engine_core::ui::TAGBRUSH_VERSION
        )));
        assert!(
            LONG_VERSION.contains(&format!("TagAction {}", engine_core::ui::TAGACTION_VERSION))
        );
        assert!(LONG_VERSION.contains(check::TAGCHECK_VERSION));
    }
}
