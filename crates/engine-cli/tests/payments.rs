//! End-to-end test of the `payments` feature: the `shop` example on the
//! real `engine` binary and PostgreSQL, with the fake payment provider.
//! Browse, add to the cart, check out, pay on the fake hosted checkout;
//! the signed webhook goes through verification, is stored, a job applies
//! it and the order shows as paid in the order history. A replayed webhook
//! changes nothing; forged ones are refused with 400 and change nothing.
//!
//! Needs `--features app,payments` (or `--all-features`) and
//! ENGINE_TEST_DATABASE_URL (Postgres; skipped otherwise).

#![cfg(all(feature = "payments", feature = "app"))]

mod common;

use std::path::Path;

use common::*;
use engine_db::testing::TestDb;

const PW: &str = "ann's password, long enough";

#[tokio::test(flavor = "multi_thread")]
async fn shop_checkout_with_the_fake_provider() {
    let Some(t) = TestDb::create_postgres("payments_e2e").await else {
        return;
    };
    let url = t.url.clone();
    tokio::task::spawn_blocking(move || scenario(&url))
        .await
        .unwrap();
    t.cleanup().await;
}

fn sql_i64(db: &str, sql: &str) -> i64 {
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

fn sql_text(db: &str, sql: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = engine_db::DbConfig::new(db.to_string());
        c.max_connections = 1;
        let d = engine_db::Db::connect(&c).await.unwrap();
        let s = engine_db::testing::query_text(&d, sql).await.unwrap();
        d.close().await;
        s
    })
}

fn path_of(location: &str) -> String {
    match location.split_once("://") {
        Some((_, rest)) => rest
            .find('/')
            .map(|i| rest[i..].to_string())
            .unwrap_or_default(),
        None => location.to_string(),
    }
}

fn webhook(b: &mut Browser, body: &str, sig: Option<&str>) -> Resp {
    let mut h: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = sig {
        h.push(("Wrought-Signature", s));
    }
    b.request_bytes(
        "POST",
        "/_engine/payments/webhook/fake/",
        Some((body.as_bytes().to_vec(), "application/json".into())),
        &h,
    )
}

fn scenario(db: &str) {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/shop"),
        &site,
    );
    let s = site.to_str().unwrap();
    engine(db, &["migrate", "--site", s], None);
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
            "ann@example.org",
            "--name",
            "Ann",
            "--password-stdin",
        ],
        Some(&format!("{PW}\n")),
    );

    // Without ENGINE_PAYMENTS_FAKE the fake provider refuses to start.
    let out = std::process::Command::new(ENGINE)
        .args(["serve", "--site", s, "--port", &free_port().to_string()])
        .env("ENGINE_DATABASE_URL", db)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "2")
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .env_remove("ENGINE_PAYMENTS_FAKE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ENGINE_PAYMENTS_FAKE=1"), "{err}");

    let log = dir.path().join("serve.log");
    let (server, port) = serve(db, &site, &log, &[("ENGINE_PAYMENTS_FAKE", "1")]);
    assert!(server.log().contains("FAKE payment provider"));

    // Browse anonymously: published pages with prices and sign-in links.
    let mut anon = Browser::new(port);
    let shop = anon.get("/shop/");
    assert_eq!(shop.status, 200);
    assert!(shop.body.contains("Enamel mug") && shop.body.contains("£12.00"));
    assert!(shop.body.contains("Sign in to buy"));
    assert!(!shop.body.contains("Add to cart"));
    // The cart is not for visitors who are not signed in.
    let r = anon.post(
        "/_engine/shop/cart/",
        &[("product", "enamel-mug"), ("_page", "/shop/")],
    );
    assert_eq!(r.status, 303);
    assert!(
        r.location().starts_with("/account/login/"),
        "{}",
        r.location()
    );

    let mut b = Browser::new(port);
    assert_eq!(b.login("ann@example.org", PW).status, 303);
    let shop = b.get("/shop/");
    assert!(shop.body.contains("Add to cart"), "{}", shop.body);
    let csrf = hidden(&shop.body, "_csrf");

    // Adding needs the form token.
    let r = b.post(
        "/_engine/shop/cart/",
        &[("product", "enamel-mug"), ("_page", "/shop/")],
    );
    assert_eq!(r.status, 403);
    // A price sent by the browser is ignored: the catalogue's is used.
    for (p, q) in [("enamel-mug", "2"), ("forged-trivet", "1")] {
        let r = b.post(
            "/_engine/shop/cart/",
            &[
                ("_csrf", &csrf),
                ("_page", "/shop/"),
                ("action", "add"),
                ("product", p),
                ("quantity", q),
                ("price", "1"),
            ],
        );
        assert_eq!(r.status, 303);
        assert_eq!(r.location(), "/shop/?shop=added");
    }
    // Unavailable products cannot be added.
    let r = b.post(
        "/_engine/shop/cart/",
        &[
            ("_csrf", &csrf),
            ("_page", "/shop/"),
            ("product", "copper-ladle"),
        ],
    );
    assert_eq!(r.location(), "/shop/?shop=unavailable");
    let added = b.get("/shop/?shop=added");
    assert!(added.body.contains("role=\"status\""));
    assert!(added.body.contains("Added to your cart."));

    // The cart page: lines, total, checkout form with its key.
    let cart = b.get("/shop/cart/");
    assert_eq!(cart.status, 200);
    assert!(cart.body.contains("£24.00"), "{}", cart.body);
    assert!(cart.body.contains("Pay £48.50"));
    let key = hidden(&cart.body, "_key");

    // Checkout: to the fake hosted page; the same form twice is one order.
    let mut seen = Vec::new();
    for _ in 0..2 {
        let r = b.post(
            "/_engine/shop/checkout/",
            &[("_csrf", &csrf), ("_page", "/shop/cart/"), ("_key", &key)],
        );
        assert_eq!(r.status, 303);
        seen.push(r.location().to_string());
    }
    assert_eq!(seen[0], seen[1]);
    assert!(seen[0].starts_with("/_engine/payments/fake/checkout/cs_fake_"));
    assert_eq!(sql_i64(db, "SELECT count(*) FROM engine_orders"), 1);
    assert_eq!(sql_i64(db, "SELECT total FROM engine_orders"), 4850);

    // Pay on the fake page (its own form token).
    let page = b.get(&seen[0]);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Test payments"));
    let fcsrf = hidden(&page.body, "_csrf");
    let r = b.post(&seen[0], &[("_csrf", &fcsrf), ("outcome", "pay")]);
    assert_eq!(r.status, 303);
    let back = path_of(r.location());
    assert!(back.starts_with("/shop/orders/?order=ord_"), "{back}");
    let order = back.rsplit('=').next().unwrap().to_string();

    // Webhook -> job -> paid; the order history says so.
    wait_for("the order to be paid", || {
        sql_text(
            db,
            &format!("SELECT status FROM engine_orders WHERE public_id = '{order}'"),
        ) == "paid"
    });
    let history = b.get(&back);
    assert!(history.body.contains("is confirmed"), "{}", history.body);
    assert!(history.body.contains("Paid"));
    assert!(history.body.contains("2 × Enamel mug"));
    // Paying emptied the cart.
    assert!(b.get("/shop/cart/").body.contains("Your cart is empty."));
    let version = sql_i64(
        db,
        &format!("SELECT version::bigint FROM engine_orders WHERE public_id = '{order}'"),
    );

    // The delivery, as the fake provider sent it.
    let d = anon.get(&seen[0].replace("/checkout/", "/deliveries/"));
    let v: serde_json::Value = serde_json::from_str(&d.body).unwrap();
    let body = v["deliveries"][0]["body"].as_str().unwrap().to_string();
    let sig = v["deliveries"][0]["signature"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(v["deliveries"][0]["status"], 200);

    // Replayed byte for byte: accepted as a no-op.
    let r = webhook(&mut anon, &body, Some(&sig));
    assert_eq!(r.status, 200);
    // Forged: changed amount under the genuine signature, a made-up
    // signature, a stale one, none, and an unknown provider path.
    let forged = body.replace("\"amount\":4850", "\"amount\":1");
    assert_ne!(forged, body);
    let fresh_event = body.replace("evt_fake_", "evt_fake_new");
    let bad_sig = format!("t={},v1={}", engine_payments::now(), "0".repeat(64));
    let stale = engine_payments::signature::header("whatever", 1_000_000, body.as_bytes());
    for (b2, s2) in [
        (forged.as_str(), Some(sig.as_str())),
        (fresh_event.as_str(), Some(bad_sig.as_str())),
        (body.as_str(), Some(stale.as_str())),
        (body.as_str(), None),
    ] {
        let r = webhook(&mut anon, b2, s2);
        assert_eq!(r.status, 400, "{b2} / {s2:?}");
    }
    let r = anon.request_bytes(
        "POST",
        "/_engine/payments/webhook/stripe/",
        Some((body.as_bytes().to_vec(), "application/json".into())),
        &[],
    );
    assert_eq!(r.status, 404);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(sql_i64(db, "SELECT count(*) FROM engine_payment_events"), 1);
    assert_eq!(
        sql_i64(
            db,
            "SELECT count(*) FROM engine_jobs WHERE kind = 'payments.apply'"
        ),
        1
    );
    assert_eq!(
        sql_i64(
            db,
            &format!("SELECT version::bigint FROM engine_orders WHERE public_id = '{order}'"),
        ),
        version
    );
    let log = server.log();
    assert!(log.contains("repeated an event already received"), "{log}");
    assert!(log.contains("webhook refused"), "{log}");

    // A second order paid one penny short is flagged, not paid.
    let shop = b.get("/shop/");
    let csrf = hidden(&shop.body, "_csrf");
    b.post(
        "/_engine/shop/cart/",
        &[
            ("_csrf", &csrf),
            ("_page", "/shop/"),
            ("product", "linen-tea-towel"),
        ],
    );
    let key = hidden(&b.get("/shop/cart/").body, "_key");
    let r = b.post(
        "/_engine/shop/checkout/",
        &[("_csrf", &csrf), ("_page", "/shop/cart/"), ("_key", &key)],
    );
    let fake = r.location().to_string();
    let fcsrf = hidden(&b.get(&fake).body, "_csrf");
    let r = b.post(&fake, &[("_csrf", &fcsrf), ("outcome", "underpay")]);
    let second = path_of(r.location())
        .rsplit('=')
        .next()
        .unwrap()
        .to_string();
    wait_for("the short payment to be flagged", || {
        sql_text(
            db,
            &format!("SELECT status FROM engine_orders WHERE public_id = '{second}'"),
        ) == "flagged"
    });
    assert!(b.get("/shop/orders/").body.contains("Under review"));

    // The administrator's orders screen lists both.
    let admin = b.get("/admin/orders/?status=all");
    assert_eq!(admin.status, 200);
    assert!(admin.body.contains(&order) && admin.body.contains(&second));
    drop(server);
}
