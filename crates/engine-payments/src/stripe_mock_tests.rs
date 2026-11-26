//! The Stripe adapter against an in-process mock of Stripe's API (bound to
//! 127.0.0.1 on an ephemeral port) and recorded webhook payloads. No real
//! network calls, no real keys. A unit-test module (not an integration
//! test) because pointing the client at a mock needs the `#[cfg(test)]`
//! constructor; nothing outside this crate's tests can change where the
//! secret key goes.

use std::sync::{Arc, Mutex};

use crate::provider::{CheckoutLine, CheckoutRequest, EventKind, Provider};
use crate::signature;
use crate::stripe::{Stripe, StripeConfig, check_api_base};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;

const SECRET_KEY: &str = "sk_test_mock_not_a_real_key";
const WEBHOOK_SECRET: &str = "whsec_mock_not_a_real_secret";

#[derive(Default)]
struct Seen {
    headers: Option<HeaderMap>,
    body: String,
    /// Requests that reached the redirect target.
    followed: usize,
}

async fn sessions(
    State(seen): State<Arc<Mutex<Seen>>>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    let refuse = body.contains("refuse-me");
    let redirect = body.contains("redirect-me");
    let http_url = body.contains("http-url-me");
    {
        let mut s = seen.lock().unwrap();
        s.headers = Some(headers);
        s.body = body;
    }
    if redirect {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/v1/elsewhere")],
            "",
        )
            .into_response();
    }
    if http_url {
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            r#"{"id":"cs_test_plain","url":"http://checkout.stripe.test/c/pay/cs_test_plain"}"#,
        )
            .into_response();
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

async fn elsewhere(State(seen): State<Arc<Mutex<Seen>>>) -> &'static str {
    seen.lock().unwrap().followed += 1;
    r#"{"id":"cs_followed","url":"https://checkout.stripe.test/x"}"#
}

async fn mock() -> (String, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = Router::new()
        .route("/v1/checkout/sessions", post(sessions))
        .route("/v1/elsewhere", post(elsewhere))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn stripe(base: &str) -> Stripe {
    Stripe::with_mock(cfg(), base).unwrap()
}

fn cfg() -> StripeConfig {
    StripeConfig {
        secret_key: SECRET_KEY.into(),
        webhook_secret: WEBHOOK_SECRET.into(),
    }
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
    let now = crate::now();
    let body = include_bytes!("../tests/fixtures/checkout_session_completed.json");
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

    let body = include_bytes!("../tests/fixtures/charge_refunded.json");
    let ev = s
        .parse_webhook(&signed(body, now), body, now, 300)
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, EventKind::Refunded);
    assert_eq!(ev.payment.as_deref(), Some("pi_3QfixturePayment"));
    assert_eq!(ev.amount, Some(3400));

    // A delayed payment method (a bank debit): the order is processing
    // until the async event says it settled or failed.
    let body = include_bytes!("../tests/fixtures/checkout_session_unpaid.json");
    let ev = s
        .parse_webhook(&signed(body, now), body, now, 300)
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, EventKind::Processing);
}

#[test]
fn forged_and_stale_webhooks_are_refused() {
    let s = stripe("http://127.0.0.1:9");
    let now = crate::now();
    let body = include_bytes!("../tests/fixtures/checkout_session_completed.json");
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

#[test]
fn the_key_only_goes_to_api_stripe_com_over_https() {
    // Production: exactly https://api.stripe.com.
    assert!(check_api_base("https://api.stripe.com", false).is_ok());
    assert!(check_api_base("https://api.stripe.com/", false).is_ok());
    assert!(Stripe::new(cfg()).is_ok());
    for bad in [
        // Another host, or one that only looks like Stripe's.
        "https://api.stripe.com.evil.test",
        "https://evil.test",
        "https://api.stripe.co",
        "https://stripe.com",
        "https://API.stripe.com.evil.test",
        // Plain http, even to Stripe.
        "http://api.stripe.com",
        // User info that would hide the real host.
        "https://api.stripe.com@evil.test",
        "https://user:pass@api.stripe.com",
        "https://user@api.stripe.com",
        // Other ports, paths, queries.
        "https://api.stripe.com:8443",
        "https://api.stripe.com/v1/../x",
        "https://api.stripe.com/?x=1",
        "not a url",
    ] {
        assert!(check_api_base(bad, false).is_err(), "{bad} accepted");
    }
    // Tests may use loopback http, and nothing else over http.
    for ok in ["http://127.0.0.1:9", "http://localhost:9", "http://[::1]:9"] {
        assert!(check_api_base(ok, true).is_ok(), "{ok}");
        assert!(
            check_api_base(ok, false).is_err(),
            "{ok} without the test opt-in"
        );
    }
    for bad in [
        "http://10.0.0.1:9",
        "http://192.168.1.2",
        "http://evil.test",
        "http://user:pw@127.0.0.1:9",
        "http://127.0.0.1.evil.test",
    ] {
        assert!(check_api_base(bad, true).is_err(), "{bad} accepted");
        assert!(Stripe::with_mock(cfg(), bad).is_err(), "{bad} accepted");
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let (base, seen) = mock().await;
    let e = stripe(&base)
        .create_checkout(&req("ord_redirect-me"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("redirect"), "{e}");
    assert_eq!(
        seen.lock().unwrap().followed,
        0,
        "the redirect was followed"
    );
}

#[tokio::test]
async fn a_plain_http_checkout_url_is_refused() {
    let (base, _) = mock().await;
    let e = stripe(&base)
        .create_checkout(&req("ord_http-url-me"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("https"), "{e}");
}
