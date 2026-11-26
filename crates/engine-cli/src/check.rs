//! TagCheck integration.
//!
//! TagCheck (`@kovirlabs/tagcheck`) is a Node.js program, so it is not
//! bundled. The engine runs it as an external tool, looked up in order:
//!
//! 1. `WROUGHT_TAGCHECK`: a command line, e.g. `node /src/tagcheck/bin/tagcheck.mjs`.
//! 2. `tagcheck` on `PATH` (`npm install -g @kovirlabs/tagcheck@0.1.3`).
//! 3. `npx` on `PATH`: runs the pinned `@kovirlabs/tagcheck@0.1.3`
//!    (downloaded by npm on first use).
//!
//! Without any of them, `engine check` explains how to install it and
//! `engine dev` skips the check with a one-line note.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};

pub const TAGCHECK_VERSION: &str = "0.1.3";

pub struct Checker {
    program: PathBuf,
    args: Vec<String>,
    describe: String,
}

impl Checker {
    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Find a way to run TagCheck.
pub fn find() -> Option<Checker> {
    if let Ok(custom) = std::env::var("WROUGHT_TAGCHECK") {
        let mut parts = custom.split_whitespace().map(str::to_string);
        if let Some(program) = parts.next() {
            return Some(Checker {
                program: PathBuf::from(program),
                args: parts.collect(),
                describe: format!("WROUGHT_TAGCHECK ({custom})"),
            });
        }
    }
    if let Some(p) = on_path("tagcheck") {
        return Some(Checker {
            describe: format!("{}", p.display()),
            program: p,
            args: vec![],
        });
    }
    if let Some(npx) = on_path("npx") {
        let pkg = format!("@kovirlabs/tagcheck@{TAGCHECK_VERSION}");
        return Some(Checker {
            program: npx,
            args: vec!["--yes".into(), pkg.clone()],
            describe: format!("npx {pkg}"),
        });
    }
    None
}

const MISSING: &str = "TagCheck is not available. It is a Node.js tool (Node 20 or later):\n  \
     npm install -g @kovirlabs/tagcheck@0.1.3\n\
     or install Node.js so `npx` can run it, or set WROUGHT_TAGCHECK to a command that runs it.";

/// `engine check`: run TagCheck over `dir`, passing its output through.
pub fn run_check(dir: &Path, browser: bool, strict: bool) -> Result<ExitCode> {
    let Some(checker) = find() else {
        bail!("{MISSING}");
    };
    eprintln!(
        "Running TagCheck ({}) on {}",
        checker.describe,
        dir.display()
    );
    let mut cmd = checker.command();
    cmd.arg(dir);
    if browser {
        cmd.arg("--browser");
    }
    if strict {
        cmd.arg("--strict");
    }
    let status = cmd
        .status()
        .with_context(|| format!("running {}", checker.describe))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}

/// `engine dev`: run TagCheck quietly and return a short summary.
pub fn dev_summary(checker: &Checker, dir: &Path) -> Result<String> {
    let out = checker
        .command()
        .arg(dir)
        .arg("--json")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running {}", checker.describe))?;
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "TagCheck gave no JSON report: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })?;
    Ok(summarise(&report, dir))
}

fn summarise(report: &serde_json::Value, dir: &Path) -> String {
    let errors = report["errors"].as_u64().unwrap_or(0);
    let warnings = report["warnings"].as_u64().unwrap_or(0);
    let mut s = format!("TagCheck: {errors} errors, {warnings} warnings");
    let mut shown = 0;
    for file in report["files"].as_array().into_iter().flatten() {
        let name = file["file"].as_str().unwrap_or("?");
        let name = Path::new(name)
            .strip_prefix(dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| name.to_string());
        for p in file["problems"].as_array().into_iter().flatten() {
            if shown == 10 {
                s.push_str("\n  ... run `engine check` for the full list");
                return s;
            }
            s.push_str(&format!(
                "\n  {name}:{}  {}  {}  {}",
                p["line"],
                p["level"].as_str().unwrap_or(""),
                p["rule"].as_str().unwrap_or(""),
                p["message"].as_str().unwrap_or("")
            ));
            shown += 1;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_json_report() {
        let report: serde_json::Value = serde_json::from_str(
            r#"{"errors":1,"warnings":0,"files":[{"file":"public/index.html","problems":[
                {"line":3,"column":1,"level":"error","rule":"missing-alt","message":"needs alt"}]}]}"#,
        )
        .unwrap();
        let s = summarise(&report, Path::new("public"));
        assert!(s.starts_with("TagCheck: 1 errors, 0 warnings"), "{s}");
        assert!(
            s.contains("index.html:3  error  missing-alt  needs alt"),
            "{s}"
        );
    }
}
