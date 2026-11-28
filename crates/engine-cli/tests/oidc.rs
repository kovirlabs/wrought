//! Single sign-on end to end: the real `engine` binary against a throwaway
//! database and a real OpenID Connect provider (Dex,
//! deploy/compose.oidc.yml), driven over HTTP through the provider's own
//! login form. Covers a new account made through the provider, the refused
//! takeover of an existing account with the same email address, linking
//! from the profile page and signing in again, the state bound to the
//! browser (replay, another browser, a changed state), unlinking rules,
//! blocked accounts and a public (PKCE-only) client.
//!
//! Needs `--all-features`, ENGINE_TEST_DATABASE_URL and
//! ENGINE_TEST_OIDC_ISSUER (`http://127.0.0.1:59110/dex`); skipped
//! otherwise. The engine listens on one of the redirect URIs' ports
//! registered at the provider (31111-31119, so several runs can overlap):
//! the first one free. They lie
//! below the kernel's ephemeral port range on purpose: a fixed port inside
//! it (59111 once) can be taken at any moment as the local port of some
//! outgoing connection, which made this test fail now and then with
//! "Address already in use" (see DECISIONS.md, v0.7).

#![cfg(all(feature = "oidc", feature = "roles"))]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use engine_db::testing::TestDb;

const BOB_PW: &str = "bob's local password, long";
const DEX_PW: &str = "password";
const SECRET: &str = "wrought-test-secret";

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_link_and_takeover_through_a_real_provider() {
    let Ok(issuer) = std::env::var("ENGINE_TEST_OIDC_ISSUER") else {
        eprintln!("skipping: set ENGINE_TEST_OIDC_ISSUER (deploy/compose.oidc.yml) to run");
        return;
    };
    let Some(t) = TestDb::create("oidc_e2e").await else {
        return;
    };
    let port = fixed_port(&[
        31111, 31112, 31113, 31114, 31115, 31116, 31117, 31118, 31119,
    ]);
    let url = t.url.clone();
    tokio::task::spawn_blocking(move || scenario(&url, &issuer, port))
        .await
        .unwrap();
    t.cleanup().await;
}

fn site(dir: &Path, port: u16, providers: &str) -> PathBuf {
    let site = dir.join("site");
    std::fs::create_dir_all(site.join("templates")).unwrap();
    std::fs::write(
        site.join("site.toml"),
        format!(
            "[site]\nname = \"SSO test\"\nbase_url = \"http://localhost:{port}\"\n\n\
             [[routes]]\npath = \"/\"\ntemplate = \"index.html\"\n\n\
             [[routes]]\npath = \"/signin/\"\ntemplate = \"signin.html\"\nmode = \"request\"\n\n\
             [auth]\ndefault_roles = [\"editor\"]\n\n{providers}"
        ),
    )
    .unwrap();
    std::fs::write(
        site.join("templates/index.html"),
        "<!doctype html><title>Home</title><p>Home page</p>",
    )
    .unwrap();
    std::fs::write(
        site.join("templates/signin.html"),
        "<!doctype html><title>Sign in</title>{{ component(\"login_form\") }}",
    )
    .unwrap();
    site
}

fn providers(issuer: &str) -> String {
    format!(
        "[[auth.oidc]]\nname = \"dex\"\nlabel = \"Dex\"\nissuer = \"{issuer}\"\n\
         client_id = \"wrought-test\"\ncreate_accounts = true\n"
    )
}

/// `/path?query` of an absolute URL.
fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.find('/') {
        Some(i) => rest[i..].to_string(),
        None => "/".into(),
    }
}

/// Follow the engine's redirect through Dex's login form; returns the URL
/// Dex sends the browser back to (the engine's callback with code and
/// state), without calling it.
fn dex_login(dex_port: u16, to_provider: &Resp, email: &str) -> String {
    assert_eq!(to_provider.status, 303, "{}", to_provider.body);
    let loc = to_provider.location().to_string();
    assert!(loc.contains("/dex/auth?"), "{loc}");
    for want in [
        "code_challenge_method=S256",
        "code_challenge=",
        "nonce=",
        "state=",
        "response_type=code",
        "scope=openid+email+profile",
    ] {
        assert!(loc.contains(want), "{want} in {loc}");
    }
    let mut dex = Browser::new(dex_port);
    let r = dex.get(&path_of(&loc));
    assert_eq!(r.status, 302, "{}", r.body);
    let r = dex.get(&path_of(r.location()));
    assert_eq!(r.status, 302, "{}", r.body);
    let form = path_of(r.location());
    let page = dex.get(&form);
    assert!(page.body.contains("name=\"password\""), "{}", page.body);
    let r = dex.post(&form, &[("login", email), ("password", DEX_PW)]);
    assert_eq!(r.status, 303, "dex login for {email}: {}", r.body);
    let back = r.location().to_string();
    assert!(back.contains("/callback/?code="), "{back}");
    back
}

/// Start a sign-in from the engine's login page.
fn start(b: &mut Browser, provider: &str) -> Resp {
    let page = b.get("/account/login/");
    let csrf = hidden(&page.body, "_csrf");
    b.post(
        &format!("/account/oidc/{provider}/start/"),
        &[("_csrf", &csrf), ("next", "/")],
    )
}

fn signed_in_as(b: &mut Browser, email: &str) -> String {
    let p = b.get("/account/profile/");
    assert_eq!(p.status, 200, "not signed in: {}", p.location());
    assert!(p.body.contains(email), "{}", p.body);
    p.body
}

fn no_session(b: &Browser) -> bool {
    !b.jar.contains_key("__Host-wrought_session")
}

fn scenario(db: &str, issuer: &str, port: u16) {
    let dex_port: u16 = issuer
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .and_then(|h| h.rsplit(':').next())
        .and_then(|p| p.parse().ok())
        .expect("issuer with a port");
    let dir = tempfile::tempdir().unwrap();
    let site = site(dir.path(), port, &providers(issuer));
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            "admin@example.org",
            "--password-stdin",
        ],
        Some("admin password, long enough\n"),
    );
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            "bob@example.org",
            "--name",
            "Bob",
            "--role",
            "editor",
            "--password-stdin",
        ],
        Some(&format!("{BOB_PW}\n")),
    );
    let log = dir.path().join("serve.log");
    // Without its client secret the server refuses to start.
    let out = std::process::Command::new(ENGINE)
        .args(["serve", "--site", s, "--port", &port.to_string()])
        .env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .env_remove("ENGINE_OIDC_DEX_CLIENT_SECRET")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ENGINE_OIDC_DEX_CLIENT_SECRET"), "{err}");

    let server = serve_on(
        port,
        db,
        &site,
        &log,
        &[("ENGINE_OIDC_DEX_CLIENT_SECRET", SECRET)],
    );
    let (server, _) = server;

    // The login page and the login_form component offer the provider; the
    // engine page lets its forms lead to the provider's origin.
    let mut a = Browser::new(port);
    let page = a.get("/account/login/");
    assert!(page.body.contains("Sign in with Dex"), "{}", page.body);
    assert!(page.body.contains("action=\"/account/oidc/dex/start/\""));
    let csp = page.header("content-security-policy").unwrap().to_string();
    let dex_origin = format!("http://127.0.0.1:{dex_port}");
    assert!(
        csp.contains(&format!("form-action 'self' {dex_origin}")),
        "{csp}"
    );
    let comp = a.get("/signin/");
    assert!(comp.body.contains("Sign in with Dex"), "{}", comp.body);

    // Login CSRF: starting needs the signed-out token; GET is not a route.
    let r = a.post("/account/oidc/dex/start/", &[("next", "/")]);
    assert_eq!(r.status, 403);
    let r = a.post("/account/oidc/dex/start/", &[("_csrf", "x"), ("next", "/")]);
    assert_eq!(r.status, 403);
    assert_ne!(a.get("/account/oidc/dex/start/").status, 303);
    assert_eq!(a.post("/account/oidc/nope/start/", &[]).status, 404);

    // 1. Alice has no account: the provider makes one (editor, not admin).
    let r = start(&mut a, "dex");
    let attempt = a
        .set_cookies
        .iter()
        .rfind(|c| c.starts_with("__Host-wrought_oidc="))
        .unwrap()
        .clone();
    for part in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=600",
    ] {
        assert!(attempt.contains(part), "{attempt}");
    }
    let back = dex_login(dex_port, &r, "alice@example.org");
    let done = a.get(&path_of(&back));
    assert_eq!(done.status, 303, "{}", done.body);
    assert_eq!(done.location(), "/");
    assert!(!no_session(&a));
    let profile = signed_in_as(&mut a, "alice@example.org");
    assert!(profile.contains("Unlink Dex"), "{profile}");
    // Replaying the provider's answer does nothing: the attempt is used.
    let mut replay = Browser::new(port);
    let r = replay.get(&path_of(&back));
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(no_session(&replay));
    let r = a.get(&path_of(&back));
    assert_eq!(r.status, 400, "{}", r.body);

    // 2. Bob has a local account. His provider identity has the same email
    // address, but it is not linked: no session, no link.
    let mut b = Browser::new(port);
    let r = start(&mut b, "dex");
    let back = dex_login(dex_port, &r, "bob@example.org");
    let r = b.get(&path_of(&back));
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("Link your account first"), "{}", r.body);
    assert!(no_session(&b));
    assert_eq!(b.get("/account/profile/").status, 303);

    // 3. Bob signs in with his password and links Dex on his profile.
    let r = b.login("bob@example.org", BOB_PW);
    assert_eq!(r.status, 303, "{}", r.body);
    let profile = signed_in_as(&mut b, "bob@example.org");
    assert!(profile.contains("Link Dex"), "{profile}");
    let csrf = hidden(&profile, "_csrf");
    // Linking needs the session's token.
    assert_eq!(
        b.post("/account/oidc/dex/link/", &[("_csrf", "wrong")])
            .status,
        403
    );
    let r = b.post("/account/oidc/dex/link/", &[("_csrf", &csrf)]);
    let back = dex_login(dex_port, &r, "bob@example.org");
    let r = b.get(&path_of(&back));
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/account/profile/?done=oidc-linked");
    let profile = signed_in_as(&mut b, "bob@example.org");
    assert!(profile.contains("Unlink Dex"), "{profile}");
    // Alice's identity cannot be linked to Bob's account.
    let r = b.post("/account/oidc/dex/link/", &[("_csrf", &csrf)]);
    let back = dex_login(dex_port, &r, "alice@example.org");
    let r = b.get(&path_of(&back));
    assert_eq!(r.status, 409, "{}", r.body);

    // 4. Signed out, Bob signs in with Dex.
    let r = b.post("/account/logout/", &[("_csrf", &csrf)]);
    assert_eq!(r.status, 303);
    let mut c = Browser::new(port);
    let r = start(&mut c, "dex");
    let back = dex_login(dex_port, &r, "bob@example.org");
    let r = c.get(&path_of(&back));
    assert_eq!(r.status, 303, "{}", r.body);
    signed_in_as(&mut c, "bob@example.org");

    // 5. The attempt is bound to the browser that started it, and to its
    // state value.
    let mut d = Browser::new(port);
    let r = start(&mut d, "dex");
    let back = dex_login(dex_port, &r, "carol@example.org");
    let mut e = Browser::new(port);
    assert_eq!(e.get(&path_of(&back)).status, 400);
    assert!(no_session(&e));
    let (before, state) = back.split_once("&state=").unwrap();
    let tampered = format!("{before}&state={}x", state);
    let mut d2 = Browser::new(port);
    d2.jar = d.jar.clone();
    assert_eq!(d2.get(&path_of(&tampered)).status, 400);
    // The real browser cannot use it either now: one try per attempt.
    assert_eq!(d.get(&path_of(&back)).status, 400);
    let r = start(&mut d, "dex");
    let back = dex_login(dex_port, &r, "carol@example.org");
    assert_eq!(d.get(&path_of(&back)).status, 303);
    signed_in_as(&mut d, "carol@example.org");

    // 6. Unlinking: Alice has no password, so Dex stays; Bob can unlink.
    let profile = a.get("/account/profile/").body;
    let csrf_a = hidden(&profile, "_csrf");
    let r = a.post(
        "/account/oidc/unlink/",
        &[("_csrf", &csrf_a), ("issuer", issuer)],
    );
    assert_eq!(r.location(), "/account/profile/?done=oidc-last");
    assert!(signed_in_as(&mut a, "alice@example.org").contains("Unlink Dex"));
    let profile = c.get("/account/profile/").body;
    let csrf_c = hidden(&profile, "_csrf");
    let r = c.post(
        "/account/oidc/unlink/",
        &[("_csrf", &csrf_c), ("issuer", issuer)],
    );
    assert_eq!(r.location(), "/account/profile/?done=oidc-unlinked");
    assert!(signed_in_as(&mut c, "bob@example.org").contains("Link Dex"));

    // 7. A blocked account cannot sign in through the provider.
    engine(
        db,
        &[
            "user",
            "set",
            "--site",
            s,
            "--email",
            "carol@example.org",
            "--active",
            "false",
        ],
        None,
    );
    let mut f = Browser::new(port);
    let r = start(&mut f, "dex");
    let back = dex_login(dex_port, &r, "carol@example.org");
    let r = f.get(&path_of(&back));
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(no_session(&f));
    let log_text = server.log();
    assert!(!log_text.contains(SECRET), "the client secret is logged");
    drop(server);

    // 8. A public client (PKCE only, no secret) of the same provider: the
    // identity is the same (issuer, subject), so Alice is recognised.
    std::fs::write(
        site.join("site.toml"),
        std::fs::read_to_string(site.join("site.toml")).unwrap().replace(
            &providers(issuer),
            &format!(
                "[[auth.oidc]]\nname = \"dexpub\"\nlabel = \"Dex public\"\nissuer = \"{issuer}\"\n\
                 client_id = \"wrought-public\"\npublic = true\n"
            ),
        ),
    )
    .unwrap();
    let (server, _) = serve_on(port, db, &site, &dir.path().join("serve2.log"), &[]);
    let mut g = Browser::new(port);
    let r = start(&mut g, "dexpub");
    let back = dex_login(dex_port, &r, "alice@example.org");
    let r = g.get(&path_of(&back));
    assert_eq!(r.status, 303, "{}", r.body);
    signed_in_as(&mut g, "alice@example.org");
    drop(server);
}
