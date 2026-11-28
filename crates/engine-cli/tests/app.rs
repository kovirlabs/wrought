//! Acceptance test for roadmap v0.4: the `small-app` example runs logins, a
//! form and an emailed receipt. Runs the real `engine` binary against a
//! throwaway database and a real SMTP sink (Mailpit,
//! deploy/compose.mailpit.yml), and drives it over HTTP like browsers.
//!
//! Also covers: registration and the welcome mail, the spam guard
//! (time trap, honeypot, rate limit), CSRF on form posts, validation,
//! multi-step forms, a password reset by email, the fragment cache (hits,
//! eviction by a content edit, per-user fragments never crossing accounts),
//! the App UI components, the admin's forms screens, the workflow (review,
//! preview, notifications), job payload scrubbing, and TOTP secrets stored
//! encrypted (and refused without a key).
//!
//! Needs the `app` features (`--all-features`), ENGINE_TEST_DATABASE_URL,
//! ENGINE_TEST_SMTP_URL and ENGINE_TEST_MAILPIT_URL (all set by
//! scripts/test-postgres.sh); skipped otherwise.

#![cfg(all(feature = "forms", feature = "mail", feature = "workflow"))]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use engine_db::testing::TestDb;
use engine_mail::testing::Mailpit;

const ADMIN_PW: &str = "admin password, long enough";
const ED_PW: &str = "editor password, long enough";
const ANN_PW: &str = "ann's password, long enough";
const BOB_PW: &str = "bob's password, long enough";

#[tokio::test(flavor = "multi_thread")]
async fn small_app_runs_logins_a_form_and_an_emailed_receipt() {
    let Some(mail) = Mailpit::from_env("small_app_runs_logins_a_form_and_an_emailed_receipt")
    else {
        return;
    };
    let Some(t) = TestDb::create_postgres("small_app").await else {
        return;
    };
    let url = t.url.clone();
    let admin_email = Mailpit::address("admin");
    let a2 = admin_email.clone();
    tokio::task::spawn_blocking(move || scenario(&url, &mail, &a2))
        .await
        .unwrap();
    // TOTP secrets are stored encrypted.
    let stored = engine_db::testing::query_text(
        &t.db,
        &format!("SELECT totp_secret FROM engine_users WHERE email = '{admin_email}'"),
    )
    .await
    .unwrap();
    assert!(stored.starts_with("v1:"), "encrypted at rest: {stored}");
    t.cleanup().await;
}

/// Count rows (through a short-lived runtime: the scenario is blocking).
fn count(db: &str, sql: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        let n = engine_db::testing::query_i64(&d, sql).await.unwrap();
        d.close().await;
        n
    })
}

fn exec(db: &str, sql: &str) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        engine_db::testing::exec_sql(&d, sql).await.unwrap();
        d.close().await;
    });
}

fn csrf(page: &Resp) -> String {
    hidden(&page.body, "_csrf")
}

fn submit_support(b: &mut Browser, stamp: &str, csrf: &str, email: &str, message: &str) -> Resp {
    b.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", csrf),
            ("_page", "/support/"),
            ("_t", stamp),
            ("_step", "0"),
            ("name", "Ann Example"),
            ("email", email),
            ("topic", "Bug"),
            ("priority", "High"),
            ("message", message),
            ("website", ""),
        ],
    )
}

fn scenario(db: &str, mail: &Mailpit, admin_email: &str) {
    let admin_email = admin_email.to_string();
    // Every address is new on each run: Mailpit keeps earlier runs' mail.
    let ed_email = Mailpit::address("ed");
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/small-app"),
        &site,
    );
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
    // A role that may write but not publish (the workflow's editors).
    exec(
        db,
        r#"INSERT INTO engine_roles (name, label, is_admin, grants) VALUES ('writer', 'Writer', false,
           '{"types": {"*": {"view": true, "create": true, "edit": true}}}')"#,
    );
    engine(db, &["content", "seed", "--site", s], None);
    engine(db, &["build", "--site", s], None);
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            &admin_email,
            "--name",
            "Ada Admin",
            "--password-stdin",
        ],
        Some(&format!("{ADMIN_PW}\n")),
    );
    engine(
        db,
        &[
            "user",
            "create",
            "--site",
            s,
            "--email",
            &ed_email,
            "--name",
            "Ed Itor",
            "--role",
            "writer",
            "--password-stdin",
        ],
        Some(&format!("{ED_PW}\n")),
    );
    let ann = Mailpit::address("ann");
    let bob = Mailpit::address("bob");
    let (server, port) = serve(
        db,
        &site,
        &dir.path().join("serve.log"),
        &[("ENGINE_SMTP_URL", &mail.smtp_url)],
    );
    let log = || server.log();

    // --- Registration with the site's own page and the Accounts component.
    let mut a = Browser::new(port);
    let page = a.get("/register/");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("action=\"/account/register/\""));
    assert!(
        page.body.contains("name=\"website\""),
        "the registration form has a honeypot"
    );
    let r = a.post(
        "/account/register/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("name", "Ann Example"),
            ("email", &ann),
            ("password", ANN_PW),
            ("website", ""),
        ],
    );
    // Email verification (on by default): no session until the address is
    // confirmed through the link in the welcome message.
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("Check your email"), "{}", r.body);
    assert!(
        r.header("set-cookie")
            .is_none_or(|c| !c.contains("wrought_session="))
    );
    let page = a.get("/login/");
    let r = a.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("email", &ann),
            ("password", ANN_PW),
        ],
    );
    assert_eq!(
        r.status, 403,
        "unverified accounts cannot sign in: {}",
        r.body
    );
    assert!(
        r.body.contains("Confirm your email address first"),
        "{}",
        r.body
    );
    // The welcome message, and a fresh link sent by the refused sign-in.
    let got = mail.wait_for(&ann, 2, Duration::from_secs(20)).unwrap();
    let welcome = got
        .iter()
        .find(|m| m.subject.contains("Welcome to Small App"))
        .unwrap_or_else(|| panic!("{got:?}"));
    assert!(
        got.iter()
            .any(|m| m.subject.contains("Confirm your email address")),
        "{got:?}"
    );
    let link = welcome
        .text
        .split_whitespace()
        .find(|w| w.contains("/account/verify/?token="))
        .expect("a verification link")
        .to_string();
    let path = &link[link.find("/account/").unwrap()..];
    let r = a.get(path);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/account/login/?done=verified");
    assert_eq!(a.get(path).status, 400, "links work once");
    let page = a.get("/login/");
    let r = a.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("email", &ann),
            ("password", ANN_PW),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/me/");
    let me = a.get("/me/");
    assert_eq!(me.status, 200);
    assert!(
        me.body
            .contains("Signed in as <strong>Ann Example</strong>"),
        "{}",
        me.body
    );
    assert!(me.body.contains("No requests yet"), "empty state");
    assert_eq!(me.header("cache-control"), Some("private, no-store"));

    // --- Sign out and in again through the site's login page.
    let r = a.post("/account/logout/", &[("_csrf", &csrf(&me))]);
    assert_eq!(r.status, 303);
    let page = a.get("/login/");
    assert!(page.body.contains("action=\"/account/login/\""));
    let r = a.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("email", &ann),
            ("password", ANN_PW),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location(),
        "/me/",
        "members land on the site, not the admin"
    );
    assert_eq!(a.get("/admin/").status, 403, "no admin rights");

    // --- Two-factor sign-in for a member (no admin rights), on the site's
    // own profile page.
    let prof = a.get("/account/profile/");
    assert_eq!(prof.status, 200, "{}", prof.body);
    let secret = hidden(&prof.body, "secret");
    assert!(prof.body.contains("<svg"), "QR code drawn on the server");
    let r = a.post(
        "/account/profile/",
        &[
            ("_csrf", &csrf(&prof)),
            ("action", "totp_on"),
            ("secret", &secret),
            ("code", "000000"),
        ],
    );
    assert_eq!(r.status, 422, "a wrong code does not turn it on");
    let code = engine_auth::totp::code_at(&secret, engine_auth::totp::current_step()).unwrap();
    let r = a.post(
        "/account/profile/",
        &[
            ("_csrf", &csrf(&prof)),
            ("action", "totp_on"),
            ("secret", &secret),
            ("code", &code),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let mut phone = Browser::new(port);
    let page = phone.get("/login/");
    let r = phone.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("email", &ann),
            ("password", ANN_PW),
        ],
    );
    assert_eq!(r.status, 303);
    assert!(
        r.location().starts_with("/account/totp/"),
        "{}",
        r.location()
    );
    let page = phone.get(r.location());
    let next = engine_auth::totp::code_at(&secret, engine_auth::totp::current_step() + 1).unwrap();
    let r = phone.post(
        "/account/totp/",
        &[
            ("_csrf", &hidden(&page.body, "_csrf")),
            ("code", &next),
            ("next", "/me/"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(phone.get("/me/").status, 200);
    // Turning it off needs the password.
    let prof = a.get("/account/profile/");
    let r = a.post(
        "/account/profile/",
        &[
            ("_csrf", &csrf(&prof)),
            ("action", "totp_off"),
            ("current", "wrong one"),
        ],
    );
    assert_eq!(r.status, 422);
    let r = a.post(
        "/account/profile/",
        &[
            ("_csrf", &csrf(&prof)),
            ("action", "totp_off"),
            ("current", ANN_PW),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    // --- The form: CSRF, spam guard, validation, then a real submission.
    let page = a.get("/support/");
    assert_eq!(page.status, 200, "{}", page.body);
    let stamp = hidden(&page.body, "_t");
    let token = csrf(&page);
    let stored = || {
        count(
            db,
            "SELECT count(*) FROM engine_form_submissions WHERE form = 'support'",
        )
    };
    // Time trap: sent at once.
    let r = submit_support(&mut a, &stamp, &token, &ann, "Too fast to be a person.");
    assert_eq!(r.status, 422);
    assert!(r.body.contains("That was very fast"), "{}", r.body);
    assert!(
        r.body.contains("Too fast to be a person."),
        "values are kept"
    );
    assert!(log().contains("refused (sent too fast)"));
    // A forged stamp.
    let r = submit_support(&mut a, "123.abc", &token, &ann, "Forged stamp here.");
    assert_eq!(r.status, 422);
    std::thread::sleep(Duration::from_millis(2100));
    // No or wrong CSRF token, or another origin.
    let mut no_token = vec![
        ("_page", "/support/"),
        ("_t", stamp.as_str()),
        ("email", ann.as_str()),
    ];
    assert_eq!(a.post("/_engine/forms/support/", &no_token).status, 403);
    no_token.push(("_csrf", "wrong"));
    assert_eq!(a.post("/_engine/forms/support/", &no_token).status, 403);
    let r = a.post_with(
        "/_engine/forms/support/",
        &[("_csrf", &token), ("_page", "/support/"), ("_t", &stamp)],
        &[("Origin", "https://evil.example")],
    );
    assert_eq!(r.status, 403);
    // Honeypot: looks fine, stores nothing.
    let r = a.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", &token),
            ("_page", "/support/"),
            ("_t", &stamp),
            ("name", "Bot"),
            ("email", &ann),
            ("topic", "Bug"),
            ("priority", "Low"),
            ("message", "Buy cheap things now!!!"),
            ("website", "http://spam.example"),
        ],
    );
    assert_eq!(r.status, 303);
    assert_eq!(stored(), 0, "honeypot hits are not stored");
    assert!(log().contains("refused (honeypot)"));
    // Validation.
    let r = a.post(
        "/_engine/forms/support/",
        &[
            ("_csrf", &token),
            ("_page", "/support/"),
            ("_t", &stamp),
            ("name", ""),
            ("email", "not an address"),
            ("topic", "Gossip"),
            ("message", "short"),
        ],
    );
    assert_eq!(r.status, 422);
    for msg in [
        "Your name is required",
        "is not a valid email address",
        "is not one of the choices",
        "must be at least 10 characters",
    ] {
        assert!(r.body.contains(msg), "{msg}: {}", r.body);
    }
    assert!(r.body.contains("aria-invalid=\"true\""));
    assert_eq!(stored(), 0);
    // A good one.
    let r = submit_support(
        &mut a,
        &stamp,
        &token,
        &ann,
        "The export button does nothing <b>at all</b>.",
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/support/?sent=support");
    let done = a.get("/support/?sent=support");
    assert!(done.body.contains("Your request is in"), "{}", done.body);
    assert_eq!(stored(), 1);

    // --- The emailed receipt, through the job queue and real SMTP.
    let got = mail
        .wait_for(&ann, 3, Duration::from_secs(30))
        .unwrap_or_else(|e| panic!("{e}\n{}", log()));
    let receipt = got
        .iter()
        .find(|m| m.subject.starts_with("We received your support request"))
        .expect("a receipt");
    assert!(receipt.subject.contains("(#"), "{}", receipt.subject);
    assert!(receipt.from.contains("hello@small-app.test"));
    assert!(
        receipt
            .text
            .contains("Message: The export button does nothing <b>at all</b>."),
        "{}",
        receipt.text
    );
    assert!(
        receipt
            .html
            .contains("does nothing &lt;b&gt;at all&lt;/b&gt;"),
        "HTML part escapes: {}",
        receipt.html
    );
    let team = mail
        .wait_for("support@small-app.test", 1, Duration::from_secs(20))
        .unwrap();
    assert!(
        team.iter()
            .any(|m| m.subject.starts_with("New support request"))
    );
    // Delivered jobs keep no message body (a reset link would be a secret).
    engine_wait(db, s, "mail jobs done", "done");
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM engine_jobs WHERE kind = 'mail.send' AND status = 'done' AND payload <> '{}'::jsonb"
        ),
        0
    );

    // --- The account page: App UI components over Ann's submissions.
    let me = a.get("/me/?requests_sort=id&requests_dir=desc");
    assert!(me.body.contains("<data value=\"1\">1</data>"), "stat card");
    assert!(
        me.body.contains("<svg") && me.body.contains("role=\"img\""),
        "chart"
    );
    assert!(me.body.contains("aria-sort=\"descending\""), "sorted table");
    assert!(me.body.contains("The export button does nothing"));
    assert!(me.body.contains("<dialog id=\"about-data\""), "modal");

    // --- Rate limit (spam guard), on the newsletter: 10 per hour.
    let page = a.get("/support/");
    let (nt, ns) = (
        csrf(&page),
        hidden(
            &page.body[page.body.find("form-newsletter").unwrap()..],
            "_t",
        ),
    );
    std::thread::sleep(Duration::from_millis(3100));
    let mut last = 0;
    for i in 0..11 {
        let r = a.post(
            "/_engine/forms/newsletter/",
            &[
                ("_csrf", &nt),
                ("_page", "/support/"),
                ("_t", &ns),
                ("email", &format!("n{i}-{ann}")),
                ("consent", "yes"),
            ],
        );
        last = r.status;
        if i < 10 {
            assert_eq!(r.status, 303, "{i}: {}", r.body);
        }
    }
    assert_eq!(last, 429, "the 11th signup in an hour is refused");
    assert!(log().contains("refused (rate limit)"));

    // --- Multi-step form.
    let page = a.get("/onboarding/");
    assert!(page.body.contains("Step 1 of 2"), "{}", page.body);
    let (ot, os) = (csrf(&page), hidden(&page.body, "_t"));
    let r = a.post(
        "/_engine/forms/onboarding/",
        &[
            ("_csrf", &ot),
            ("_page", "/onboarding/"),
            ("_t", &os),
            ("_step", "0"),
            ("_go", "next"),
            ("team", "Smiths"),
            ("size", "0"),
        ],
    );
    assert_eq!(r.status, 422, "step 1 is checked before moving on");
    let r = a.post(
        "/_engine/forms/onboarding/",
        &[
            ("_csrf", &ot),
            ("_page", "/onboarding/"),
            ("_t", &os),
            ("_step", "0"),
            ("_go", "next"),
            ("team", "Smiths"),
            ("size", "4"),
        ],
    );
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Step 2 of 2"), "{}", r.body);
    assert!(
        r.body.contains("name=\"team\" value=\"Smiths\""),
        "step 1 carried along"
    );
    std::thread::sleep(Duration::from_millis(2100));
    let r = a.post(
        "/_engine/forms/onboarding/",
        &[
            ("_csrf", &ot),
            ("_page", "/onboarding/"),
            ("_t", &os),
            ("_step", "1"),
            ("_go", "submit"),
            ("team", "Smiths"),
            ("size", "4"),
            ("email", &ann),
            ("terms", "yes"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM engine_form_submissions WHERE form = 'onboarding'"
        ),
        1
    );

    // --- Fragment cache: hits, eviction by a content edit, and personal
    // fragments that never cross accounts.
    let mut b = Browser::new(port);
    let page = b.get("/register/");
    let r = b.post(
        "/account/register/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/status/"),
            ("name", "Bob Other"),
            ("email", &bob),
            ("password", BOB_PW),
            ("website", ""),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let got = mail.wait_for(&bob, 1, Duration::from_secs(20)).unwrap();
    let link = got[0]
        .text
        .split_whitespace()
        .find(|w| w.contains("/account/verify/?token="))
        .expect("a verification link")
        .to_string();
    assert_eq!(b.get(&link[link.find("/account/").unwrap()..]).status, 303);
    let page = b.get("/login/");
    let r = b.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/status/"),
            ("email", &bob),
            ("password", BOB_PW),
        ],
    );
    assert_eq!(r.status, 303);
    let a1 = a.get("/status/");
    let a2 = a.get("/status/");
    assert_eq!(
        a2.header("x-wrought-fragments"),
        Some("hits=2 misses=0"),
        "{:?}",
        a1.header("x-wrought-fragments")
    );
    assert!(a2.body.contains("Requests of Ann Example") && a2.body.contains("Bug (High)"));
    let b1 = b.get("/status/");
    assert_eq!(
        b1.header("x-wrought-fragments"),
        Some("hits=1 misses=1"),
        "shared hit, own miss"
    );
    assert!(b1.body.contains("Requests of Bob Other"));
    assert!(
        !b1.body.contains("Ann"),
        "Bob never sees Ann's fragment: {}",
        b1.body
    );
    assert!(b1.body.contains("None yet."));
    let a3 = a.get("/status/");
    assert!(!a3.body.contains("Bob"), "Ann never sees Bob's");
    let anon = Browser::new(port).get("/status/");
    assert!(!anon.body.contains("Ann") && !anon.body.contains("Bob"));
    // A signed-out visitor gets a form-token cookie only from a page that
    // carries the token (a form), so other pages stay the same for all.
    let csrf_cookie = |r: &common::Resp| {
        r.header("set-cookie")
            .is_some_and(|c| c.contains("wrought_csrf="))
    };
    assert_eq!(csrf_cookie(&anon), anon.body.contains("name=\"_csrf\""));
    let reg = Browser::new(port).get("/register/");
    assert!(csrf_cookie(&reg), "the registration form's token cookie");
    assert!(anon.body.contains("Support requests are open"));
    // Edit an update: the shared fragment is evicted and re-rendered.
    engine(
        db,
        &[
            "content",
            "set",
            "--site",
            s,
            "update",
            "support-requests-are-open",
            "--set",
            "title=Support requests: now with receipts",
        ],
        None,
    );
    wait_for("the fragment to be evicted", || {
        Browser::new(port)
            .get("/status/")
            .body
            .contains("now with receipts")
    });
    let a4 = a.get("/status/");
    assert!(a4.body.contains("now with receipts"));
    // The anonymous poll above re-rendered the shared fragment; Ann's own
    // fragment was not touched by the edit.
    assert_eq!(a4.header("x-wrought-fragments"), Some("hits=2 misses=0"));

    // --- Password reset by email.
    let mut forgetful = Browser::new(port);
    let page = forgetful.get("/reset/");
    let r = forgetful.post(
        "/account/reset/",
        &[("_csrf", &csrf(&page)), ("email", &ann)],
    );
    assert_eq!(r.status, 200);
    let msgs = mail.wait_for(&ann, 4, Duration::from_secs(20)).unwrap();
    let reset = msgs
        .iter()
        .find(|m| m.subject.starts_with("Reset your password"))
        .expect("reset mail");
    let link = reset
        .text
        .split_whitespace()
        .find(|w| w.contains("/account/reset/confirm/?token="))
        .expect("link in the mail")
        .to_string();
    let path = &link[link.find("/account/").unwrap()..];
    let page = forgetful.get(path);
    assert_eq!(page.status, 200, "{}", page.body);
    let r = forgetful.post(
        "/account/reset/confirm/",
        &[
            ("_csrf", &csrf(&page)),
            ("token", &hidden(&page.body, "token")),
            ("password", "a brand new password"),
            ("password_again", "a brand new password"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let page = forgetful.get("/login/");
    let r = forgetful.post(
        "/account/login/",
        &[
            ("_csrf", &csrf(&page)),
            ("next", "/me/"),
            ("email", &ann),
            ("password", "a brand new password"),
        ],
    );
    assert_eq!(r.status, 303);
    assert!(
        !log().contains("/account/reset/confirm/?token="),
        "with mail, links never go to the log"
    );

    // --- The admin: forms and submissions.
    let mut admin = Browser::new(port);
    let r = admin.login(&admin_email, ADMIN_PW);
    assert_eq!(r.status, 303);
    let list = admin.get("/admin/forms/");
    assert_eq!(list.status, 200, "{}", list.body);
    assert!(list.body.contains("Support request") && list.body.contains("forms/support.toml"));
    let subs = admin.get("/admin/forms/support/");
    assert!(subs.body.contains("Ann Example"), "{}", subs.body);
    let csv = admin.get("/admin/forms/support/export.csv");
    assert_eq!(csv.header("content-type"), Some("text/csv; charset=utf-8"));
    assert!(
        csv.body
            .contains("The export button does nothing <b>at all</b>.")
    );
    // A form built in the admin.
    let page = admin.get("/admin/forms/new/");
    let r = admin.post(
        "/admin/forms/new/",
        &[
            ("_csrf", &csrf(&page)),
            ("_rows", "1"),
            ("name", "feedback"),
            ("title", "Feedback"),
            ("store", "true"),
            ("min_seconds", "3"),
            ("max_per_hour", "10"),
            ("f0.name", "comment"),
            ("f0.type", "textarea"),
            ("f0.required", "true"),
            ("_action", "save"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(admin.get("/admin/forms/").body.contains("Feedback"));
    // The jobs screen: a dead job is listed with its error, and retried.
    let dead = count(
        db,
        "WITH j AS (INSERT INTO engine_jobs (kind, payload, status, attempts, max_attempts, \
         last_error, finished_at) VALUES ('test.never', '{}', 'dead', 5, 5, \
         'smtp: connection refused <b>', now()) RETURNING id) SELECT id FROM j",
    );
    let jobs = admin.get("/admin/jobs/");
    assert_eq!(jobs.status, 200, "{}", jobs.body);
    assert!(jobs.body.contains("test.never"), "{}", jobs.body);
    assert!(jobs.body.contains("smtp: connection refused &lt;b&gt;"));
    let detail = admin.get(&format!("/admin/jobs/{dead}/"));
    assert_eq!(detail.status, 200, "{}", detail.body);
    let r = admin.post(
        &format!("/admin/jobs/{dead}/retry/"),
        &[("_csrf", &csrf(&detail))],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        count(
            db,
            &format!("SELECT count(*) FROM engine_jobs WHERE id = {dead} AND status = 'queued'")
        ),
        1
    );
    assert_eq!(
        forgetful.get("/admin/jobs/").status,
        403,
        "administrators only"
    );
    // Members cannot see submissions (Ann is signed in again in
    // `forgetful`; the reset signed her out everywhere else).
    assert_eq!(forgetful.get("/admin/forms/support/").status, 403);
    assert_eq!(
        a.get("/me/").status,
        303,
        "the password reset ended old sessions"
    );

    // --- Workflow: the editor sends a draft for review, the admin
    // previews and approves it; both are notified.
    engine(
        db,
        &[
            "content",
            "create",
            "--site",
            s,
            "update",
            "--set",
            "title=Dark mode is coming",
            "--set",
            "body=Soon.",
        ],
        None,
    );
    let id = count(
        db,
        "SELECT id FROM content_update WHERE slug = 'dark-mode-is-coming'",
    );
    let mut ed = Browser::new(port);
    assert_eq!(ed.login(&ed_email, ED_PW).status, 303);
    let edit = ed.get(&format!("/admin/content/update/{id}/"));
    assert!(edit.body.contains("Send for review"), "{}", edit.body);
    let r = ed.post(
        &format!("/admin/content/update/{id}/review/"),
        &[
            ("_csrf", &csrf(&edit)),
            ("action", "submit"),
            ("note", "Please check the wording."),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let review = mail
        .wait_for(&admin_email, 1, Duration::from_secs(20))
        .unwrap();
    assert!(
        review[0]
            .subject
            .contains("Review requested: Dark mode is coming")
    );
    assert!(review[0].text.contains("Please check the wording."));
    let preview = admin.get(&format!("/admin/content/update/{id}/preview/"));
    assert_eq!(preview.status, 200, "{}", preview.body);
    assert!(
        preview.body.contains("Dark mode is coming")
            && preview.body.contains("<strong>Preview</strong>")
    );
    assert_eq!(preview.header("x-robots-tag"), Some("noindex, nofollow"));
    assert_eq!(
        Browser::new(port)
            .get("/updates/dark-mode-is-coming/")
            .status,
        404,
        "drafts are not published"
    );
    assert_eq!(
        forgetful
            .get(&format!("/admin/content/update/{id}/preview/"))
            .status,
        403,
        "members cannot preview"
    );
    // Sent back once with a note, then sent again and approved.
    let edit = admin.get(&format!("/admin/content/update/{id}/"));
    assert!(edit.body.contains("Approve and publish"));
    let r = admin.post(
        &format!("/admin/content/update/{id}/review/"),
        &[
            ("_csrf", &csrf(&edit)),
            ("_version", &hidden(&edit.body, "_version")),
            ("action", "reject"),
            ("note", "Say when."),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let back = mail
        .wait_for(&ed_email, 1, Duration::from_secs(20))
        .unwrap();
    assert!(
        back[0]
            .subject
            .starts_with("Changes requested: Dark mode is coming"),
        "{back:?}"
    );
    assert!(back[0].text.contains("Say when."));
    let edit = ed.get(&format!("/admin/content/update/{id}/"));
    assert!(
        edit.body.contains("Send for review") && edit.body.contains("Say when."),
        "history shows the note"
    );
    let r = ed.post(
        &format!("/admin/content/update/{id}/review/"),
        &[
            ("_csrf", &csrf(&edit)),
            ("action", "submit"),
            ("note", "Now with a date."),
        ],
    );
    assert_eq!(r.status, 303);
    let queue = admin.get("/admin/review/");
    assert!(
        queue.body.contains("Dark mode is coming") && queue.body.contains("Now with a date."),
        "{}",
        queue.body
    );
    let edit = admin.get(&format!("/admin/content/update/{id}/"));
    let r = admin.post(
        &format!("/admin/content/update/{id}/review/"),
        &[
            ("_csrf", &csrf(&edit)),
            ("_version", &hidden(&edit.body, "_version")),
            ("action", "approve"),
            ("note", ""),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    wait_for("the approved update to be published", || {
        site.join("public/updates/dark-mode-is-coming/index.html")
            .is_file()
    });
    let decided = mail
        .wait_for(&ed_email, 2, Duration::from_secs(20))
        .unwrap();
    assert!(
        decided
            .iter()
            .any(|m| m.subject.starts_with("Published: Dark mode is coming")),
        "{decided:?}"
    );

    // --- TOTP for the admin: a QR code, then secrets stored encrypted
    // (checked after the scenario).
    let prof = admin.get("/admin/profile/");
    assert!(
        prof.body.contains("<svg role=\"img\" aria-label=\"QR code"),
        "{}",
        prof.body
    );
    let secret = hidden(&prof.body, "secret");
    let code = engine_auth::totp::code_at(&secret, engine_auth::totp::current_step()).unwrap();
    let r = admin.post(
        "/admin/profile/totp/",
        &[
            ("_csrf", &csrf(&prof)),
            ("secret", &secret),
            ("code", &code),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    drop(server);

    // Without the key, the server refuses to start rather than leave
    // two-factor secrets unreadable.
    let out = Command::new(ENGINE)
        .args(["serve", "--site", s, "--port", &free_port().to_string()])
        .env("ENGINE_DATABASE_URL", db)
        .env_remove("ENGINE_SECRET_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        text.contains("use two-factor sign-in, but no site key is set")
            && text.contains("ENGINE_SECRET_KEY"),
        "{text}"
    );
}

/// Wait until no `mail.send` job is queued or running.
fn engine_wait(db: &str, _site: &str, what: &str, _status: &str) {
    wait_for(what, || {
        count(
            db,
            "SELECT count(*) FROM engine_jobs WHERE kind = 'mail.send' AND status IN ('queued', 'running')",
        ) == 0
    });
}
