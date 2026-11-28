//! Pagefind for static sites: `[search] pagefind = true` makes `engine
//! build` index the built site with Pagefind, so a site with no server
//! still has search (in the browser, with JavaScript).
//!
//! Pagefind is a separate program (a Rust binary distributed through npm)
//! and is not embedded: it would add its whole indexer to every engine
//! build for one optional step. Like TagCheck it is looked up as:
//!
//! 1. `HAMMERWRIGHT_PAGEFIND`: a command line, e.g. `/usr/local/bin/pagefind`;
//! 2. `pagefind` on `PATH`;
//! 3. `npx` on `PATH`, running the pinned `pagefind@1.5.2`.
//!
//! The index lands in `<output>/pagefind/`, next to a small start script
//! for the `pagefind_search` component. Sites with a server use the
//! `search` feature instead (results without JavaScript).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const PAGEFIND_VERSION: &str = "1.5.2";

/// Starts Pagefind's UI on every `[data-pagefind-search]` element (an
/// external file, so pages need no inline script).
const START_JS: &str = "// Written by `engine build` for the pagefind_search component.\n\
window.addEventListener('DOMContentLoaded', () => {\n\
  for (const el of document.querySelectorAll('[data-pagefind-search]')) {\n\
    new PagefindUI({ element: el, showSubResults: true, showImages: false });\n\
  }\n\
});\n";

fn on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn command() -> Option<(Command, String)> {
    if let Ok(custom) = std::env::var("HAMMERWRIGHT_PAGEFIND") {
        let mut parts = custom.split_whitespace();
        let program = parts.next()?;
        let mut c = Command::new(program);
        c.args(parts);
        return Some((c, format!("HAMMERWRIGHT_PAGEFIND ({custom})")));
    }
    if let Some(p) = on_path("pagefind") {
        let d = p.display().to_string();
        return Some((Command::new(p), d));
    }
    let npx = on_path("npx")?;
    let pkg = format!("pagefind@{PAGEFIND_VERSION}");
    let mut c = Command::new(npx);
    c.args(["--yes", &pkg]);
    Some((c, format!("npx {pkg}")))
}

/// Whether `[search] pagefind = true`.
pub fn wanted(cfg: &engine_core::config::SiteConfig) -> bool {
    cfg.search
        .as_ref()
        .and_then(|t| t.get("pagefind"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Index the built site in `output`.
pub fn run(output: &Path) -> Result<()> {
    let Some((mut cmd, how)) = command() else {
        bail!(
            "[search] pagefind = true, but Pagefind was not found: install it (`npm install -g \
             pagefind@{PAGEFIND_VERSION}`), put `npx` on PATH, or set HAMMERWRIGHT_PAGEFIND"
        );
    };
    let out = cmd
        .arg("--site")
        .arg(output)
        .output()
        .with_context(|| format!("running Pagefind ({how})"))?;
    if !out.status.success() {
        bail!(
            "Pagefind ({how}) failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let dir = output.join("pagefind");
    std::fs::write(dir.join("hammerwright-start.js"), START_JS)
        .with_context(|| format!("writing {}", dir.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let summary = text
        .lines()
        .find(|l| l.contains("Indexed") && l.contains("pages"))
        .unwrap_or("done")
        .trim()
        .to_string();
    println!("Pagefind ({how}): {summary}");
    Ok(())
}
