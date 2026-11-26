//! `engine`: the Wrought command line.

mod check;
mod dev;
mod new;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use engine_core::builder::{self, BuildOptions, BuildReport};
use engine_core::serve::{self, ServeConfig};

/// Shown by `engine --version`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\npreset: static (render modes: build)",
    "\nbundled: TagBrush.css 2.0.1, TagAction 0.2.0 (Apache-2.0, see /_engine/licenses/)",
    "\nexternal: @kovirlabs/tagcheck 0.1.3 for `engine check` (needs Node.js)"
);

#[derive(Parser)]
#[command(
    name = "engine",
    version,
    long_version = LONG_VERSION,
    about = "Wrought: one Rust engine for static sites and web apps",
    after_help = "Docs: run `engine dev --site docs` in the Wrought repository."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct SiteArg {
    /// Site folder (the one holding site.toml).
    #[arg(short, long, default_value = ".")]
    site: PathBuf,
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
        name: PathBuf,
        /// static (available), cms or app (later releases).
        #[arg(long, default_value = "static")]
        preset: String,
    },
    /// Render every `build` route to the output folder.
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
    /// Serve a built site with the built-in static file server.
    Serve {
        #[command(flatten)]
        site: SiteArg,
        /// Folder to serve (default: the site's output folder).
        #[arg(short, long)]
        dir: Option<PathBuf>,
        #[command(flatten)]
        listen: Listen,
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::New { name, preset } => {
            new::new_site(&name, &preset)?;
            println!(
                "Created {} from the `{preset}` preset.\nNext:\n  cd {}\n  engine dev",
                name.display(),
                name.display()
            );
        }
        Cmd::Build {
            site,
            output,
            drafts,
            verbose,
        } => {
            let report = builder::build_site(&site.site, &BuildOptions { output, drafts })?;
            print_report(&report, verbose);
        }
        Cmd::Dev {
            site,
            listen,
            drafts,
            no_check,
        } => dev::run(&site.site, addr(&listen), drafts, !no_check)?,
        Cmd::Serve { site, dir, listen } => {
            let dir = output_folder(&site.site, dir)?;
            let addr = addr(&listen);
            println!(
                "Serving {} at http://{addr}/ (Ctrl+C to stop)",
                dir.display()
            );
            runtime()?.block_on(serve::serve(
                ServeConfig {
                    root: dir,
                    live_reload: None,
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

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
}

pub(crate) async fn shutdown_signal() {
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
