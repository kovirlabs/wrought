//! The Stripe adapter against an in-process mock of Stripe's API (bound to
//! 127.0.0.1 on an ephemeral port) and recorded webhook payloads. No real
//! network calls, no real keys.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use engine_payments::provider::{CheckoutLine, CheckoutRequest, EventKind, Provider};
use engine_payments::signature;
use engine_payments::stripe::{Stripe, StripeConfig};

const SECRET_KEY: &str = "sk_test_mock_not_a_real_key";
const WEBHOOK_SECRET: &str = "whsec_mock_not_a_real_secret";

#[derive(Default)]
struct Seen {
    headers: Option<HeaderMap>,
    body: String,
}

async fn sessions(
    State(seen): State<Arc<Mutex<Seen>>>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    let refuse = body.contains("refuse-me");
    {
        let mut s = seen.lock().unwrap();
        s.headers = Some(headers);
        s.body = body;
    }
    if refuse {
        return (
            StatusCode::BAD_REQUEST,
            r#"{"error":{"type":"invalid_request_error","message":"Invalid currency: xxx"}}"#,
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"cs_test_mock123","object":"checkout.session","url":"https://checkout.stripe.test/c/pay/cs_test_mock123"}"#,
    )
        .into_response()
}

async fn mock() -> (String, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = Router::new()
        .route("/v1/checkout/sessions", post(sessions))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn stripe(base: &str) -> Stripe {
    Stripe::new(StripeConfig {
        secret_key: SECRET_KEY.into(),
        webhook_secret: WEBHOOK_SECRET.into(),
        api_base: base.into(),
    })
    .unwrap()
}

fn req(order: &str) -> CheckoutRequest {
    CheckoutRequest {
        order: order.into(),
        currency: "GBP".into(),
        lines: vec![
            CheckoutLine {
                name: "Enamel mug".into(),
                unit_amount: 1200,
                quantity: 2,
            },
            CheckoutLine {
                name: "Tea towel & cloth".into(),
                unit_amount: 1000,
                quantity: 1,
            },
        ],
        total: 3400,
        success_url: "https://shop.test/shop/orders/?order=ord_x".into(),
        cancel_url: "https://shop.test/shop/cart/".into(),
        idempotency_key: format!("checkout-{order}"),
        customer_email: Some("ann@example.org".into()),
    }
}

#[tokio::test]
async fn creates_a_checkout_session() {
    let (base, seen) = mock().await;
    let s = stripe(&base);
    let session = s.create_checkout(&req("ord_abc")).await.unwrap();
    assert_eq!(session.id, "cs_test_mock123");
    assert!(session.url.starts_with("https://checkout.stripe.test/"));
    let seen = seen.lock().unwrap();
    let h = seen.headers.as_ref().unwrap();
    assert_eq!(
        h.get("authorization").unwrap(),
        &HeaderValue::from_str(&format!("Bearer {SECRET_KEY}")).unwrap()
    );
    assert_eq!(h.get("idempotency-key").unwrap(), "checkout-ord_abc");
    assert!(
        h.get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/x-www-form-urlencoded")
    );
    let pairs: Vec<(String, String)> = url_pairs(&seen.body);
    let get = |k: &str| pairs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    assert_eq!(get("mode"), Some("payment"));
    assert_eq!(get("client_reference_id"), Some("ord_abc"));
    assert_eq!(get("line_items[0][price_data][unit_amount]"), Some("1200"));
    assert_eq!(get("line_items[0][quantity]"), Some("2"));
    assert_eq!(
        get("line_items[1][price_data][product_data][name]"),
        Some("Tea towel & cloth")
    );
    assert_eq!(get("line_items[1][price_data][currency]"), Some("gbp"));
    assert_eq!(get("customer_email"), Some("ann@example.org"));
}

fn url_pairs(body: &str) -> Vec<(String, String)> {
    engine_core::web::parse_query(body)
}

#[tokio::test]
async fn a_refusal_is_reported_without_the_key() {
    let (base, _) = mock().await;
    let e = stripe(&base)
        .create_checkout(&req("ord_refuse-me"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Invalid currency"), "{e}");
    assert!(!e.contains(SECRET_KEY), "{e}");
    // Nothing listening: unreachable, and still no key in the message.
    let e = stripe("http://127.0.0.1:9")
        .create_checkout(&req("ord_abc"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not be reached"), "{e}");
    assert!(!e.contains(SECRET_KEY), "{e}");
}

fn signed(body: &[u8], t: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "stripe-signature",
        HeaderValue::from_str(&signature::header(WEBHOOK_SECRET, t, body)).unwrap(),
    );
    h
}

#[test]
fn webhook_fixtures_parse() {
    let s = stripe("http://127.0.0.1:9");
    let now = engine_payments::now();
    let body = include_bytes!("fixtures/checkout_session_completed.json");
    let ev = s
        .parse_webhook(&signed(body, now), body, now, 300)
        .unwrap()
        .unwrap();
    assert_eq!(ev.id, "evt_1QfixtureCompleted");
    assert_eq!(ev.kind, EventKind::Paid);
    assert_eq!(ev.order.as_deref(), Some("ord_fixture0000000000000000"));
    assert_eq!(ev.session.as_deref(), Some("cs_test_a1b2c3"));
    assert_eq!(ev.payment.as_deref(), Some("pi_3QfixturePayment"));
    assert_eq!(ev.amount, Some(3400));
    assert_eq!(ev.currency.as_deref(), Some("GBP"));

    let body = include_bytes!("fixtures/charge_refunded.json");
    let ev = s
        .parse_webhook(&signed(body, now), body, now, 300)
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, EventKind::Refunded);
    assert_eq!(ev.payment.as_deref(), Some("pi_3QfixturePayment"));
    assert_eq!(ev.amount, Some(3400));

    // A delayed payment method: nothing to do until the async event.
    let body = include_bytes!("fixtures/checkout_session_unpaid.json");
    assert!(
        s.parse_webhook(&signed(body, now), body, now, 300)
            .unwrap()
            .is_none()
    );
}

#[test]
fn forged_and_stale_webhooks_are_refused() {
    let s = stripe("http://127.0.0.1:9");
    let now = engine_payments::now();
    let body = include_bytes!("fixtures/checkout_session_completed.json");
    // Changed body, same signature.
    let forged = String::from_utf8_lossy(body).replace("3400", "1");
    assert!(
        s.parse_webhook(&signed(body, now), forged.as_bytes(), now, 300)
            .is_err()
    );
    // Signed with another secret.
    let mut h = HeaderMap::new();
    h.insert(
        "stripe-signature",
        HeaderValue::from_str(&signature::header("whsec_other", now, body)).unwrap(),
    );
    assert!(s.parse_webhook(&h, body, now, 300).is_err());
    // No header at all.
    assert!(s.parse_webhook(&HeaderMap::new(), body, now, 300).is_err());
    // A genuine delivery replayed ten minutes later.
    assert!(
        s.parse_webhook(&signed(body, now - 600), body, now, 300)
            .is_err()
    );
}
