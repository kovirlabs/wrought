//! `engine dev`: build, serve with live reload, watch, rebuild, check, and
//! the developer tools ([`crate::devtools`]).
//!
//! A site with a database (content, accounts, forms, jobs, handlers...) is
//! served by the same server as `engine serve` ([`crate::server::serve`]
//! with [`crate::server::DevServe`]): one code path, plus rebuilds, live
//! reload and the tools. A site without one gets the static server with
//! the same rebuild loop.

use std::net::SocketAddr;
use std::path::{Component, Path};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use engine_core::builder::{self, BuildOptions, BuildReport};
use engine_core::serve::{self, ServeConfig};
use notify::{RecursiveMode, Watcher};
use tokio::sync::watch;

use crate::check;

/// Options of `engine dev`.
pub struct DevOptions {
    pub drafts: bool,
    pub run_checks: bool,
    /// A statement shape run this many times in one request is reported
    /// as a likely N+1.
    pub n_plus_one: usize,
}

pub fn run(site: &Path, addr: SocketAddr, o: DevOptions) -> Result<()> {
    let DevOptions {
        drafts,
        run_checks,
        n_plus_one,
    } = o;
    let site = site
        .canonicalize()
        .with_context(|| format!("site folder {}", site.display()))?;
    // The developer tools are decided by this command, nothing else: no
    // setting of the site's turns them on (DECISIONS.md, "Dev server
    // tools").
    engine_core::dev::enable_for_engine_dev();
    let tools = crate::devtools::DevTools::new(&site, n_plus_one);
    let (cfg, _) = builder::load_config(&site)?;
    let opts = BuildOptions {
        output: None,
        drafts,
        jobs: None,
    };
    let output = builder::output_dir(&site, &cfg, &opts);
    if site.join("Cargo.toml").is_file() {
        println!(
            "This site is a Rust crate: templates, content and static files reload here; after \
             a change to its Rust code, run `cargo run -- dev` again (`cargo watch -x 'run -- \
             dev'` does it on every save)."
        );
    }

    let checker = if run_checks {
        let c = check::find();
        if c.is_none() {
            eprintln!(
                "note: TagCheck not found (needs Node.js); pages won't be checked. \
                 See `engine check` for how to install it."
            );
        }
        c
    } else {
        None
    };

    // The whole app: the same server as `engine serve`, with rebuilds,
    // live reload and the developer tools.
    #[cfg(any(
        feature = "content",
        feature = "auth",
        feature = "forms",
        feature = "jobs",
        feature = "media",
        feature = "realtime"
    ))]
    if crate::server::wanted(&site)? {
        match crate::build_any(&site, &opts) {
            Ok(report) => {
                crate::print_report(&report, false);
                if let Some(c) = &checker {
                    match check::dev_summary(c, &report.output) {
                        Ok(s) => println!("{s}"),
                        Err(e) => eprintln!("TagCheck failed: {e:#}"),
                    }
                }
            }
            // Served anyway: request pages, handlers and the error pages
            // still work, and the next change rebuilds.
            Err(e) => eprintln!("build failed: {e:#}"),
        }
        std::fs::create_dir_all(&output)?;
        println!("Dev tools at http://{addr}/_engine/dev/ (queries, errors, mail)");
        let (version, _) = watch::channel(0u64);
        crate::server::serve(
            &site,
            addr,
            crate::server::ServeOptions {
                workers: true,
                dev: Some(crate::server::DevServe {
                    tools,
                    version,
                    build: opts,
                    checker,
                }),
            },
        )?;
        return Ok(());
    }

    let build = std::sync::Arc::new(site_builder(&site)?);
    build_once(&build, &site, &opts, checker.as_ref());
    // Serve even after a failed first build, so fixing it reloads the page.
    std::fs::create_dir_all(&output)?;

    let (version_tx, version_rx) = watch::channel(0u64);
    let (ev_tx, ev_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = ev_tx.send(res);
    })
    .context("starting the file watcher")?;
    watcher
        .watch(&site, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", site.display()))?;

    let output_rel = output
        .strip_prefix(&site)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let watch_site = site.clone();
    let watch_opts = opts.clone();
    let watch_output = output.clone();
    std::thread::spawn(move || {
        let relevant = |ev: &notify::Event| {
            !matches!(ev.kind, notify::EventKind::Access(_))
                && ev
                    .paths
                    .iter()
                    .any(|p| is_source(&watch_site, &output_rel, p))
        };
        while let Ok(res) = ev_rx.recv() {
            let Ok(ev) = res else { continue };
            if !relevant(&ev) {
                continue;
            }
            // Debounce: editors write several events per save.
            while ev_rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
            println!("Change detected, rebuilding...");
            if build_once(&build, &watch_site, &watch_opts, None) {
                version_tx.send_modify(|v| *v += 1);
                if let Some(c) = &checker {
                    match check::dev_summary(c, &watch_output) {
                        Ok(s) => println!("{s}"),
                        Err(e) => eprintln!("TagCheck failed: {e:#}"),
                    }
                }
            }
        }
    });

    println!(
        "Dev server at http://{addr}/ (watching {}; Ctrl+C to stop)",
        site.display()
    );
    crate::runtime()?.block_on(serve::serve(
        ServeConfig {
            root: output,
            live_reload: Some(version_rx),
            outer: Some(tools.layer()),
            app: Some(tools.router()),
            ..Default::default()
        },
        addr,
        crate::shutdown_signal(),
    ))?;
    drop(watcher);
    Ok(())
}

type BuildFn = Box<dyn Fn(&Path, &BuildOptions) -> Result<BuildReport> + Send + Sync>;

/// The build `engine dev` runs: the plain one, or for sites with database
/// content one that renders their pages too.
fn site_builder(site: &Path) -> Result<BuildFn> {
    #[cfg(feature = "content")]
    if let Some(b) = crate::cms::dev_builder(site)? {
        return Ok(b);
    }
    let _ = site;
    Ok(Box::new(|site: &Path, opts: &BuildOptions| {
        #[cfg(any(feature = "auth", feature = "forms", feature = "realtime"))]
        if let Some(r) = crate::wiring::plain_build(site, opts)? {
            return Ok(r);
        }
        builder::build_site(site, opts)
    }))
}

/// Build once and print the outcome (and a TagCheck summary). Returns
/// whether the build succeeded.
fn build_once(
    build: &BuildFn,
    site: &Path,
    opts: &BuildOptions,
    checker: Option<&check::Checker>,
) -> bool {
    match build(site, opts) {
        Ok(report) => {
            crate::print_report(&report, false);
            if let Some(c) = checker {
                match check::dev_summary(c, &report.output) {
                    Ok(s) => println!("{s}"),
                    Err(e) => eprintln!("TagCheck failed: {e:#}"),
                }
            }
            true
        }
        Err(e) => {
            eprintln!("build failed: {e:#}");
            false
        }
    }
}

/// Whether a changed path is a site source (not build output, not hidden,
/// not editor temp files).
fn is_source(site: &Path, output_rel: &Path, p: &Path) -> bool {
    let Ok(rel) = p.strip_prefix(site) else {
        return false;
    };
    if !output_rel.as_os_str().is_empty() && rel.starts_with(output_rel) {
        return false;
    }
    let hidden = rel.components().any(|c| match c {
        Component::Normal(n) => {
            let n = n.to_string_lossy();
            n.starts_with('.') || n == "target" || n == "node_modules"
        }
        _ => false,
    });
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let temp = name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".tmp");
    !hidden && !temp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sources_trigger_rebuilds() {
        let site = Path::new("/s");
        let out = Path::new("public");
        assert!(is_source(site, out, Path::new("/s/content/a.md")));
        assert!(is_source(site, out, Path::new("/s/templates/page.html")));
        assert!(is_source(site, out, Path::new("/s/site.toml")));
        assert!(!is_source(site, out, Path::new("/s/public/index.html")));
        assert!(!is_source(
            site,
            out,
            Path::new("/s/.public.staging/x.html")
        ));
        assert!(!is_source(site, out, Path::new("/s/content/.a.md.swp")));
        assert!(!is_source(site, out, Path::new("/s/content/a.md~")));
        assert!(!is_source(site, out, Path::new("/elsewhere/a.md")));
    }
}
