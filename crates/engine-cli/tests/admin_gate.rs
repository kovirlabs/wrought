//! Every admin route goes through the same gate. The routes are read from
//! the admin's router source (so a route added later is covered without
//! touching this test), filled with sample parameters, and each one must
//! refuse: (a) no session, (b) a signed-in member without admin access,
//! (c) an administrator who has not turned on TOTP while
//! `require_totp_for_admins` is set (profile pages excepted, that is where
//! they enrol), and (d) a state-changing request (POST/PUT) without the
//! CSRF token, from an administrator who is otherwise allowed.
//!
//! Needs the `cms` features (`--all-features`) and ENGINE_TEST_DATABASE_URL.

#![cfg(all(feature = "admin", feature = "roles"))]

mod common;

use std::path::Path;

use common::*;
use engine_db::testing::TestDb;

const ADMIN_PW: &str = "admin password, long enough";
const MEMBER_PW: &str = "member password, long enough";

/// (method, path) of every route registered in engine-admin, with
/// parameters filled in.
fn admin_routes() -> Vec<(String, String)> {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine-admin/src");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&src_dir).unwrap() {
        let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
        // `.route("/admin/...", get(x).post(y))`, possibly over several lines.
        let mut rest = text.as_str();
        while let Some(i) = rest.find(".route(") {
            rest = &rest[i + 7..];
            let Some(q1) = rest.find('"') else { break };
            let after = &rest[q1 + 1..];
            let Some(q2) = after.find('"') else { break };
            let path = &after[..q2];
            // The handlers up to the next route, or the end of the chain
            // (`;`, or the closing brace of a function returning it).
            let end = [".route(", ";", "\n}"]
                .iter()
                .filter_map(|m| after.find(m))
                .min()
                .unwrap_or(after.len());
            let call = &after[q2..end];
            if !path.starts_with("/admin/") {
                continue;
            }
            let filled = path
                .replace("{ty}", "article")
                .replace("{vocab}", "topics")
                .replace("{field}", "sections")
                .replace("{name}", "editor")
                .split('/')
                .map(|seg| {
                    if seg.starts_with('{') {
                        "1".to_string()
                    } else {
                        seg.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            for (needle, method) in [("get(", "GET"), ("post(", "POST"), ("put(", "PUT")] {
                if call.contains(needle) {
                    out.push((method.to_string(), filled.clone()));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn send(b: &mut Browser, method: &str, path: &str, csrf: Option<&str>) -> Resp {
    let mut h: Vec<(&str, &str)> = Vec::new();
    if let Some(c) = csrf {
        h.push(("X-CSRF-Token", c));
    }
    match method {
        "GET" => b.request("GET", path, None, &h),
        "PUT" => {
            h.push(("Content-Type", "application/json"));
            b.request(
                "PUT",
                path,
                Some(r#"{"version": 1, "blocks": []}"#.into()),
                &h,
            )
        }
        _ => {
            let r = b.request("POST", path, Some(String::new()), &h);
            if r.status == 400 || r.status == 415 {
                // A multipart route (uploads) rejects a urlencoded body
                // before its handler runs; send an empty multipart body.
                return b.request_bytes(
                    "POST",
                    path,
                    Some((
                        b"--x--\r\n".to_vec(),
                        "multipart/form-data; boundary=x".into(),
                    )),
                    &h,
                );
            }
            r
        }
    }
}

/// A refusal: a redirect to sign in / enrol, 401 or 403; or 404 for a
/// route this build or database does not mount (the jobs screen on
/// SQLite; 405 when only the static server answers). Never a success.
fn refused(r: &Resp) -> bool {
    matches!(r.status, 303 | 401 | 403 | 404 | 405)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_admin_route_is_gated() {
    let Some(t) = TestDb::create("every_admin_route_is_gated").await else {
        return;
    };
    let url = t.url.clone();
    tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    t.cleanup().await;
}

fn scenario(db: &str) {
    let routes = admin_routes();
    assert!(
        routes.len() > 30 && routes.iter().any(|(_, p)| p.starts_with("/admin/api/")),
        "the router source was read: {routes:?}"
    );
    if cfg!(feature = "visual-editor") {
        for want in [
            ("GET", "/admin/editor/article/1/"),
            ("GET", "/admin/editor/article/1/frame/"),
            ("POST", "/admin/api/items/article/1/preview/"),
            ("PUT", "/admin/api/items/article/1/blocks/"),
        ] {
            assert!(
                routes.iter().any(|(m, p)| m == want.0 && p == want.1),
                "the visual editor's {want:?} is enumerated: {routes:?}"
            );
        }
        assert!(
            !routes
                .iter()
                .any(|(m, p)| m == "GET" && p.ends_with("/blocks/") && p.contains("/api/")),
            "no handler is misread: {routes:?}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site"),
        &site,
    );
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["content", "seed", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    for (email, pw) in [
        ("admin@example.org", ADMIN_PW),
        ("member@example.org", MEMBER_PW),
    ] {
        engine(
            db,
            &[
                "user",
                "create",
                "--site",
                s,
                "--email",
                email,
                "--password-stdin",
            ],
            Some(&format!("{pw}\n")),
        );
    }

    // (a), (b), (d) with the default settings.
    {
        let (_server, port) = serve(db, &site, &dir.path().join("serve1.log"), &[]);
        let mut anon = Browser::new(port);
        let mut member = Browser::new(port);
        assert_eq!(member.login("member@example.org", MEMBER_PW).status, 303);
        let mut admin = Browser::new(port);
        assert_eq!(admin.login("admin@example.org", ADMIN_PW).status, 303);
        for (m, p) in &routes {
            let r = send(&mut anon, m, p, None);
            assert!(refused(&r), "{m} {p} without a session: {}", r.status);
            if r.status == 303 {
                assert!(
                    r.location().starts_with("/account/login/"),
                    "{m} {p}: {}",
                    r.location()
                );
            }
            let r = send(&mut member, m, p, None);
            assert!(
                matches!(r.status, 401 | 403 | 404 | 405),
                "{m} {p} as a member without admin rights: {}",
                r.status
            );
            if m != "GET" {
                let r = send(&mut admin, m, p, None);
                assert!(
                    matches!(r.status, 403..=405),
                    "{m} {p} without the CSRF token: {} {}",
                    r.status,
                    r.body
                );
            }
        }
    }

    // (c) administrators must enrol in TOTP first.
    let cfg = site.join("site.toml");
    let text = std::fs::read_to_string(&cfg).unwrap().replace(
        "require_totp_for_admins = false",
        "require_totp_for_admins = true",
    );
    assert!(text.contains("require_totp_for_admins = true"));
    std::fs::write(&cfg, text).unwrap();
    let (_server, port) = serve(db, &site, &dir.path().join("serve2.log"), &[]);
    let mut admin = Browser::new(port);
    assert_eq!(admin.login("admin@example.org", ADMIN_PW).status, 303);
    for (m, p) in routes
        .iter()
        .filter(|(_, p)| !p.starts_with("/admin/profile/"))
    {
        let r = send(&mut admin, m, p, None);
        assert!(
            refused(&r),
            "{m} {p} by an admin without TOTP: {}",
            r.status
        );
        if r.status == 303 {
            assert!(
                r.location().contains("totp-required") || m != "GET",
                "{m} {p}: {}",
                r.location()
            );
        }
        if p.starts_with("/admin/api/") {
            assert!(r.body.contains("totp_required"), "{m} {p}: {}", r.body);
        }
    }
}
