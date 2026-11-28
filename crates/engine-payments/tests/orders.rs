//! Orders, webhook handling and the job on a real Postgres database
//! (ENGINE_TEST_DATABASE_URL; skipped without it).

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue};
use engine_db::migrate;
use engine_db::testing::{TestDb, exec_sql, query_i64, query_text};
use engine_payments::fake::{Fake, Outcome, SIGNATURE_HEADER};
use engine_payments::provider::Provider;
use engine_payments::{FileCatalogue, Payments, PaymentsConfig, ReceiveError, Received};

const PRODUCTS: &str = r#"
[[products]]
id = "mug"
name = "Enamel mug"
price = 1200

[[products]]
id = "towel"
name = "Tea towel"
price = 1000

[[products]]
id = "retired"
name = "Old poster"
price = 500
available = false
"#;

async fn setup(name: &str) -> Option<(TestDb, Payments, Arc<Fake>, i64)> {
    let t = TestDb::create(name).await?;
    let mut all = engine_auth::migrations();
    all.extend(engine_jobs::migrations());
    all.extend(engine_payments::migrations());
    migrate::apply(&t.db, &all).await.unwrap();
    exec_sql(
        &t.db,
        "INSERT INTO engine_users (email, password_hash) VALUES ('ann@example.org', 'x')",
    )
    .await
    .unwrap();
    let uid = query_i64(&t.db, "SELECT id FROM engine_users")
        .await
        .unwrap();
    let cfg: toml::Table = toml::from_str("provider = \"fake\"\ncurrency = \"GBP\"").unwrap();
    let cfg = PaymentsConfig::from_site(Some(&cfg)).unwrap().unwrap();
    let fake = Arc::new(Fake::new("a test secret, long enough".into()));
    let p = Payments::new(
        t.db.clone(),
        fake.clone() as Arc<dyn Provider>,
        Arc::new(FileCatalogue::parse(PRODUCTS, "GBP", "test").unwrap()),
        cfg,
        "http://shop.test",
    )
    .unwrap();
    Some((t, p, fake, uid))
}

fn headers(sig: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(SIGNATURE_HEADER, HeaderValue::from_str(sig).unwrap());
    h
}

/// Order, fake session and the event of `outcome`.
async fn order_with(
    p: &Payments,
    fake: &Fake,
    uid: i64,
    key: &str,
    outcome: Outcome,
) -> (engine_payments::Order, String, Vec<u8>, String) {
    let store = p.store();
    store.cart_add(uid, "mug", 2).await.unwrap();
    store.cart_add(uid, "towel", 1).await.unwrap();
    let cart = p.cart(uid).await.unwrap();
    let (o, _) = store.create_order(uid, key, &cart, "fake").await.unwrap();
    let req = engine_payments::provider::CheckoutRequest {
        order: o.public_id.clone(),
        currency: o.currency.clone(),
        lines: vec![],
        total: o.total,
        success_url: "/ok".into(),
        cancel_url: "/cart".into(),
        idempotency_key: format!("checkout-{}", o.public_id),
        customer_email: None,
    };
    let s = fake.create_checkout(&req).await.unwrap();
    store.set_session(o.id, &s.id, &s.url).await.unwrap();
    let (_, body, sig) = fake.event(&s.id, outcome).unwrap();
    (o, s.id, body, sig)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paid_event_pays_once_and_replays_change_nothing() {
    let Some((t, p, fake, uid)) = setup("payments_paid").await else {
        return;
    };
    let store = p.store();
    // Prices come from the catalogue; unavailable products are refused.
    store.cart_add(uid, "mug", 2).await.unwrap();
    store.cart_add(uid, "mug", 1).await.unwrap();
    store.cart_add(uid, "towel", 1).await.unwrap();
    let cart = p.cart(uid).await.unwrap();
    assert_eq!(cart.total, 3 * 1200 + 1000);
    assert_eq!(cart.total_display, "£46.00");
    assert_eq!(cart.count, 4);
    store.cart_set(uid, "mug", 2).await.unwrap();
    let cart = p.cart(uid).await.unwrap();
    assert_eq!(cart.total, 3400);

    // The same checkout key twice: one order.
    let key = "key-0123456789abcdef";
    let (a, new_a) = store.create_order(uid, key, &cart, "fake").await.unwrap();
    let (b, new_b) = store.create_order(uid, key, &cart, "fake").await.unwrap();
    assert!(new_a && !new_b);
    assert_eq!(a.public_id, b.public_id);
    assert_eq!(a.lines.len(), 2);
    assert_eq!(
        query_i64(&t.db, "SELECT count(*) FROM engine_orders")
            .await
            .unwrap(),
        1
    );
    store.cart_clear(uid).await.unwrap();

    let (o, _session, body, sig) =
        order_with(&p, &fake, uid, "key-second-abcdefgh", Outcome::Pay).await;
    assert_eq!(o.status, "pending");
    assert_eq!(
        p.receive(&headers(&sig), &body).await.unwrap(),
        Received::New
    );
    // A replay: stored once, queued once.
    assert_eq!(
        p.receive(&headers(&sig), &body).await.unwrap(),
        Received::Duplicate
    );
    assert_eq!(
        query_i64(&t.db, "SELECT count(*) FROM engine_payment_events")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        query_i64(
            &t.db,
            "SELECT count(*) FROM engine_jobs WHERE kind = 'payments.apply'"
        )
        .await
        .unwrap(),
        1
    );
    let ev = query_i64(&t.db, "SELECT id FROM engine_payment_events")
        .await
        .unwrap();
    let r = store.apply_event(ev).await.unwrap();
    assert_eq!(r.status, "applied");
    let paid = store.order_by_public(&o.public_id).await.unwrap().unwrap();
    assert_eq!(paid.status, "paid");
    assert!(paid.paid_at.is_some());
    assert_eq!(paid.version, o.version + 2, "session, then paid");
    // Paying clears the cart.
    assert!(store.cart_lines(uid).await.unwrap().is_empty());
    // The job running again (at least once delivery) changes nothing.
    assert_eq!(store.apply_event(ev).await.unwrap().status, "done before");
    let again = store.order_by_public(&o.public_id).await.unwrap().unwrap();
    assert_eq!(again.version, paid.version);
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_and_wrong_events_change_nothing() {
    let Some((t, p, fake, uid)) = setup("payments_forged").await else {
        return;
    };
    let store = p.store();
    let (o, _, body, sig) = order_with(&p, &fake, uid, "key-forged-abcdefgh", Outcome::Pay).await;

    // A changed body with the genuine signature, a made-up signature, none.
    let forged = String::from_utf8_lossy(&body).replace("\"amount\":3400", "\"amount\":1");
    assert_ne!(forged.as_bytes(), body.as_slice());
    for (h, b) in [
        (headers(&sig), forged.as_bytes()),
        (
            headers(&format!(
                "t={},v1={}",
                engine_payments::now(),
                "ab".repeat(32)
            )),
            &body[..],
        ),
        (HeaderMap::new(), &body[..]),
    ] {
        assert!(matches!(
            p.receive(&h, b).await,
            Err(ReceiveError::Refused(_))
        ));
    }
    // An old capture, correctly signed for its time.
    let old = fake.sign(engine_payments::now() - 3600, &body);
    assert!(matches!(
        p.receive(&headers(&old), &body).await,
        Err(ReceiveError::Refused(_))
    ));
    assert_eq!(
        query_i64(&t.db, "SELECT count(*) FROM engine_payment_events")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .order_by_public(&o.public_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_amounts_are_flagged_and_illegal_moves_refused() {
    let Some((t, p, fake, uid)) = setup("payments_flagged").await else {
        return;
    };
    let store = p.store();
    // Paying one penny too little: flagged, not paid.
    let (o, _, body, sig) =
        order_with(&p, &fake, uid, "key-underpay-abcdefgh", Outcome::Underpay).await;
    assert_eq!(
        p.receive(&headers(&sig), &body).await.unwrap(),
        Received::New
    );
    let ev = query_i64(&t.db, "SELECT max(id) FROM engine_payment_events")
        .await
        .unwrap();
    assert_eq!(store.apply_event(ev).await.unwrap().status, "flagged");
    let f = store.order_by_public(&o.public_id).await.unwrap().unwrap();
    assert_eq!(f.status, "flagged");
    assert!(f.note.unwrap().contains("£33.99"));

    // A paid order cannot be cancelled.
    store.cart_clear(uid).await.unwrap();
    let (o, session, body, sig) =
        order_with(&p, &fake, uid, "key-paid-abcdefghij", Outcome::Pay).await;
    p.receive(&headers(&sig), &body).await.unwrap();
    let ev = query_i64(&t.db, "SELECT max(id) FROM engine_payment_events")
        .await
        .unwrap();
    store.apply_event(ev).await.unwrap();
    // Hand-make a cancel event for the same session (the fake page would
    // not offer it any more).
    let cancel = serde_json::json!({
        "id": "evt_fake_handmade_cancel", "type": "checkout.cancelled",
        "session": session, "order": o.public_id, "amount": 3400, "currency": "GBP",
    })
    .to_string();
    let sig = fake.sign(engine_payments::now(), cancel.as_bytes());
    assert_eq!(
        p.receive(&headers(&sig), cancel.as_bytes()).await.unwrap(),
        Received::New
    );
    let ev = query_i64(&t.db, "SELECT max(id) FROM engine_payment_events")
        .await
        .unwrap();
    let r = store.apply_event(ev).await.unwrap();
    assert_eq!(r.status, "refused");
    let still = store.order_by_public(&o.public_id).await.unwrap().unwrap();
    assert_eq!(still.status, "paid");
    assert_eq!(
        query_text(
            &t.db,
            &format!("SELECT status FROM engine_payment_events WHERE id = {ev}")
        )
        .await
        .unwrap(),
        "refused"
    );
    // Refund: paid -> refunded.
    let (_, body, sig) = fake.event(&session, Outcome::Refund).unwrap();
    p.receive(&headers(&sig), &body).await.unwrap();
    let ev = query_i64(&t.db, "SELECT max(id) FROM engine_payment_events")
        .await
        .unwrap();
    assert_eq!(store.apply_event(ev).await.unwrap().status, "applied");
    assert_eq!(
        store
            .order_by_public(&o.public_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "refunded"
    );
    let events = store.events_of(o.id).await.unwrap();
    assert_eq!(events.len(), 3);
    t.cleanup().await;
}
