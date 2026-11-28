//! Roadmap v0.6: `engine new` with every preset and starter theme builds.
//! Each combination this build supports is created, built (the database
//! presets on a throwaway PostgreSQL database each, from
//! ENGINE_TEST_DATABASE_URL; without it only the static preset runs) and
//! checked for the theme's own pages; the dashboard is served and opened
//! signed in. Combinations a theme refuses are refused with a message.
//! TagCheck and axe-core over the same sites run in scripts/check-a11y.sh.

mod common;

use std::path::Path;

use common::*;

const THEMES: [&str; 3] = ["docs", "business", "dashboard"];

fn presets() -> Vec<&'static str> {
    let mut p = vec!["static"];
    if cfg!(feature = "cms") {
        p.push("cms");
    }
    if cfg!(feature = "app") {
        p.push("app");
    }
    p
}

fn run(args: &[&str], db: Option<&str>) -> (bool, String) {
    let mut c = std::process::Command::new(ENGINE);
    c.args(args).env("ENGINE_SECRET_KEY", SITE_KEY);
    match db {
        Some(u) => c.env("ENGINE_DATABASE_URL", u),
        None => c.env_remove("ENGINE_DATABASE_URL"),
    };
    let o = c.output().unwrap();
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

/// A fresh database for a database preset, or `None` without
/// ENGINE_TEST_DATABASE_URL (the combination is then skipped).
#[cfg(feature = "cms")]
fn database(rt: &tokio::runtime::Runtime, name: &str) -> Option<engine_db::testing::TestDb> {
    rt.block_on(engine_db::testing::TestDb::create(name))
}

#[test]
fn every_preset_and_theme_builds() {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(feature = "cms")]
    let rt = tokio::runtime::Runtime::new().unwrap();
    for preset in presets() {
        for theme in THEMES {
            let dir = tmp.path().join(format!("{preset}-{theme}"));
            let d = dir.to_str().unwrap();
            let (ok, out) = run(&["new", d, "--preset", preset, "--theme", theme], None);
            if theme == "dashboard" && preset == "static" {
                assert!(!ok && out.contains("needs the cms or app"), "{out}");
                continue;
            }
            assert!(ok, "{preset}/{theme}: {out}");
            // A database preset gets its own compose.yml for PostgreSQL.
            assert_eq!(dir.join("compose.yml").is_file(), preset != "static");
            #[cfg(feature = "cms")]
            let test_db = match preset {
                "static" => None,
                _ => match database(&rt, &format!("themes_{preset}_{theme}")) {
                    Some(t) => Some(t),
                    None => continue,
                },
            };
            #[cfg(feature = "cms")]
            let db = test_db.as_ref().map(|t| t.url.clone());
            #[cfg(not(feature = "cms"))]
            let db: Option<String> = None;
            if let Some(u) = &db {
                for cmd in [
                    &["migrate", "--site", d][..],
                    &["content", "seed", "--site", d][..],
                ] {
                    let (ok, out) = run(cmd, Some(u));
                    assert!(ok, "{preset}/{theme} {cmd:?}: {out}");
                }
            }
            let (ok, out) = run(&["build", "--site", d], db.as_deref());
            assert!(ok, "{preset}/{theme} build: {out}");
            let public = dir.join("public");
            let home = read(&public.join("index.html"));
            match theme {
                "docs" => {
                    assert!(home.contains("Getting started"), "{home}");
                    let page = read(&public.join("docs/configuration/index.html"));
                    assert!(
                        page.contains("On this page") || page.contains("<nav"),
                        "{page}"
                    );
                    assert!(page.contains("/docs/getting-started/"), "sidebar: {page}");
                }
                "business" => {
                    assert!(home.contains("What we do") && home.contains("Our history"));
                    assert!(read(&public.join("contact/index.html")).contains("mailto:"));
                }
                _ => {
                    assert!(home.contains("/dashboard/"), "nav entry");
                    assert!(
                        !public.join("dashboard/index.html").exists(),
                        "request pages are never written"
                    );
                    if preset == "cms" {
                        dashboard_signed_in(&dir, db.as_deref().unwrap());
                    }
                }
            }
            #[cfg(feature = "cms")]
            if let Some(t) = test_db {
                rt.block_on(t.cleanup());
            }
        }
    }
}

fn dashboard_signed_in(site: &Path, db: &str) {
    let s = site.to_str().unwrap();
    let pw = "dashboard password, long enough";
    let mut c = std::process::Command::new(ENGINE);
    c.args([
        "user",
        "create",
        "--site",
        s,
        "--email",
        "dash@example.org",
        "--password-stdin",
    ])
    .env("ENGINE_DATABASE_URL", db)
    .env("ENGINE_SECRET_KEY", SITE_KEY)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::null());
    let mut child = c.spawn().unwrap();
    use std::io::Write as _;
    writeln!(child.stdin.take().unwrap(), "{pw}").unwrap();
    assert!(child.wait().unwrap().success());
    let (_server, port) = serve(db, site, &site.join("serve.log"), &[]);
    let mut anon = Browser::new(port);
    let r = anon.get("/dashboard/");
    assert_eq!(r.status, 303, "signed-out visitors are sent to sign in");
    let mut b = Browser::new(port);
    assert_eq!(b.login("dash@example.org", pw).status, 303);
    let r = b.get("/dashboard/");
    assert_eq!(r.status, 200, "{}", r.body);
    for want in [
        "Orders per day",
        "Customers",
        "<dialog",
        "role=\"tablist\"",
        "<data",
    ] {
        assert!(r.body.contains(want), "{want} in {}", r.body);
    }
}
