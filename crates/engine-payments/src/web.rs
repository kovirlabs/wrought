//! The payments routes.
//!
//! * `POST /_engine/shop/cart/`: `action` = `add` (default), `set` or
//!   `remove`, with `product`, `quantity`, `_csrf` and `_page` (where to go
//!   back to, with `?shop=added|updated|removed|unavailable|cart-full`).
//!   Signed-in visitors only; others are sent to sign in.
//! * `POST /_engine/shop/checkout/`: `_csrf`, `_key` (the idempotency key
//!   the checkout form carries) and `_page`. Makes the order and sends the
//!   visitor to the provider's checkout.
//! * `POST /_engine/payments/webhook/<provider>/`: provider deliveries.
//!   200 for accepted, repeated and ignored events; 400 for forged, stale
//!   or unreadable ones (nothing stored); 500 when the database fails (the
//!   provider retries).
//! * With the fake provider only: `GET`/`POST
//!   /_engine/payments/fake/checkout/<session>/` (the test checkout page)
//!   and `GET /_engine/payments/fake/deliveries/<session>/` (JSON list of
//!   the signed deliveries, for replaying by hand).

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use engine_auth::Auth;
use engine_auth::web::{Ui, csrf_ok, secure_headers, see_other};
use engine_core::web::{AnonCsrf, FormData, safe_path, url_encode};
use include_dir::{Dir, include_dir};
use minijinja::context;

use crate::catalogue::valid_id;
use crate::fake::{Delivery, Outcome};
use crate::provider::CheckoutRequest;
use crate::{Payments, ReceiveError, Received};

/// The fake checkout page's template.
pub static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Largest webhook body taken.
pub const MAX_WEBHOOK_BYTES: usize = 256 * 1024;

#[derive(Clone)]
struct Ctx {
    payments: Payments,
    auth: Auth,
    anon: AnonCsrf,
    ui: Arc<Ui>,
}

/// The routes (fake pages only with the fake provider).
pub fn router(payments: Payments, auth: Auth, anon: AnonCsrf, site_name: &str) -> Router {
    let fake = payments.fake().is_some();
    let ctx = Ctx {
        payments,
        auth,
        anon,
        ui: Arc::new(Ui::new(site_name, &[&TEMPLATES], &[])),
    };
    let mut r = Router::new()
        .route("/_engine/shop/cart/", post(cart))
        .route("/_engine/shop/checkout/", post(checkout))
        .route(
            "/_engine/payments/webhook/{provider}/",
            post(webhook).layer(axum::extract::DefaultBodyLimit::max(MAX_WEBHOOK_BYTES)),
        );
    if fake {
        r = r
            .route(
                "/_engine/payments/fake/checkout/{session}/",
                get(fake_page).post(fake_post),
            )
            .route(
                "/_engine/payments/fake/deliveries/{session}/",
                get(fake_deliveries),
            );
    }
    r.with_state(ctx)
}

fn plain(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, format!("{msg}\n")).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    secure_headers(r.headers_mut());
    r
}

/// `_page` without its query, or `default`.
fn back_to(form: &FormData, default: &str) -> String {
    safe_path(form.get("_page"))
        .map(|p| p.split('?').next().unwrap_or("/").to_string())
        .unwrap_or_else(|| default.to_string())
}

fn with_notice(page: &str, notice: &str) -> String {
    format!("{page}?shop={notice}")
}

const BAD_TOKEN: &str = "This form was not accepted: its security token is missing or out of \
                         date, or it was sent from another site. Go back, reload the page and \
                         try again.";

/// The signed-in visitor, or the response that sends them to sign in
/// (boxed: a `Response` is large, and the happy path should not carry it).
async fn visitor(
    ctx: &Ctx,
    headers: &HeaderMap,
    form: &FormData,
    page: &str,
) -> Result<engine_auth::Session, Box<Response>> {
    let Some(s) = ctx.auth.signed_in(headers).await else {
        return Err(Box::new(see_other(&format!(
            "/account/login/?next={}",
            url_encode(page)
        ))));
    };
    if !csrf_ok(&s, headers, form) {
        return Err(Box::new(plain(StatusCode::FORBIDDEN, BAD_TOKEN)));
    }
    Ok(s)
}

fn failed(e: anyhow::Error) -> Response {
    eprintln!("payments: {e:#}");
    if engine_db::health::is_unavailable_any(&e) {
        return engine_core::serve::unavailable(
            "The shop is not available right now; nothing was charged. Please try again in a moment.\n",
        );
    }
    plain(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong on our side; nothing was charged. Please try again.",
    )
}

async fn cart(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let cfg = ctx.payments.config();
    let page = back_to(&form, &cfg.cancel_path);
    let s = match visitor(&ctx, &headers, &form, &page).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let uid = s.user.id;
    let Some(product) = form.get("product").filter(|p| valid_id(p)) else {
        return plain(StatusCode::BAD_REQUEST, "No product given.");
    };
    let quantity = match form.get("quantity").map(str::trim) {
        None | Some("") => 1,
        Some(q) => match q.parse::<i64>() {
            Ok(q) if (0..=crate::orders::MAX_QUANTITY).contains(&q) => q,
            _ => return plain(StatusCode::BAD_REQUEST, "The quantity must be 0 to 99."),
        },
    };
    let store = ctx.payments.store();
    let notice = match form.get("action").unwrap_or("add") {
        "add" => {
            let p = match ctx.payments.catalogue().product(product).await {
                Ok(p) => p,
                Err(e) => return failed(e),
            };
            if !p.is_some_and(|p| p.available && p.currency == cfg.currency) {
                "unavailable"
            } else {
                match store.cart_add(uid, product, quantity.max(1)).await {
                    Ok(true) => "added",
                    Ok(false) => "cart-full",
                    Err(e) => return failed(e),
                }
            }
        }
        "set" => match store.cart_set(uid, product, quantity).await {
            Ok(()) if quantity == 0 => "removed",
            Ok(()) => "updated",
            Err(e) => return failed(e),
        },
        "remove" => match store.cart_remove(uid, product).await {
            Ok(()) => "removed",
            Err(e) => return failed(e),
        },
        _ => return plain(StatusCode::BAD_REQUEST, "Unknown cart action."),
    };
    see_other(&with_notice(&page, notice))
}

fn valid_key(k: &str) -> bool {
    (16..=100).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn checkout(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let p = &ctx.payments;
    let cfg = p.config();
    let page = back_to(&form, &cfg.cancel_path);
    let s = match visitor(&ctx, &headers, &form, &page).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let uid = s.user.id;
    let Some(key) = form.get("_key").filter(|k| valid_key(k)) else {
        return plain(
            StatusCode::BAD_REQUEST,
            "This checkout form is incomplete. Go back, reload the page and try again.",
        );
    };
    let store = p.store();
    let done = |o: &crate::Order| format!("{}?order={}", cfg.success_path, o.public_id);
    // The same form posted again: the same order.
    let existing = match store.order_by_key(uid, key).await {
        Ok(o) => o,
        Err(e) => return failed(e),
    };
    let order = match existing {
        Some(o) => o,
        None => {
            let cart = match p.cart(uid).await {
                Ok(c) => c,
                Err(e) => return failed(e),
            };
            if cart.is_empty() {
                return see_other(&with_notice(&page, "empty"));
            }
            if !cart.unavailable.is_empty() {
                return see_other(&with_notice(&page, "unavailable"));
            }
            match store
                .create_order(uid, key, &cart, p.provider().name())
                .await
            {
                Ok((o, _)) => o,
                Err(e) => return failed(e),
            }
        }
    };
    if order.status != "pending" {
        return see_other(&done(&order));
    }
    if let Some(url) = &order.checkout_url {
        return see_other(url);
    }
    let req = CheckoutRequest {
        order: order.public_id.clone(),
        currency: order.currency.clone(),
        lines: order
            .lines
            .iter()
            .map(|l| crate::provider::CheckoutLine {
                name: l.name.clone(),
                unit_amount: l.unit_amount,
                quantity: l.quantity as i64,
            })
            .collect(),
        total: order.total,
        success_url: format!("{}{}", p.public_url(), done(&order)),
        cancel_url: format!(
            "{}{}?order={}",
            p.public_url(),
            cfg.cancel_path,
            order.public_id
        ),
        idempotency_key: format!("checkout-{}", order.public_id),
        customer_email: Some(s.user.email.clone()),
    };
    match p.provider().create_checkout(&req).await {
        Ok(session) => {
            if let Err(e) = store.set_session(order.id, &session.id, &session.url).await {
                return failed(e);
            }
            see_other(&session.url)
        }
        Err(e) => {
            eprintln!(
                "payments: checkout for order {} failed: {e}",
                order.public_id
            );
            if let Err(e2) = store.fail_unstarted(&order, &e.to_string()).await {
                eprintln!("payments: {e2:#}");
            }
            see_other(&with_notice(&page, "provider-error"))
        }
    }
}

async fn webhook(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let p = &ctx.payments;
    if name != p.provider().name() {
        return plain(StatusCode::NOT_FOUND, "No such payment provider.");
    }
    let (status, _) = deliver(p, &headers, &body).await;
    let msg = match status {
        StatusCode::OK => "ok",
        StatusCode::BAD_REQUEST => "refused",
        _ => "error",
    };
    plain(status, msg)
}

/// The shared webhook handling: what the endpoint and the fake
/// provider's deliveries both go through.
async fn deliver(p: &Payments, headers: &HeaderMap, body: &[u8]) -> (StatusCode, &'static str) {
    match p.receive(headers, body).await {
        Ok(Received::New) => (StatusCode::OK, "new"),
        Ok(Received::Duplicate) => {
            eprintln!(
                "payments: {} webhook repeated an event already received; nothing done",
                p.provider().name()
            );
            (StatusCode::OK, "duplicate")
        }
        Ok(Received::Ignored) => (StatusCode::OK, "ignored"),
        Err(ReceiveError::Refused(e)) => {
            eprintln!("payments: {} webhook refused: {e}", p.provider().name());
            (StatusCode::BAD_REQUEST, "refused")
        }
        // The provider sends it again later either way.
        Err(ReceiveError::Internal(e)) => {
            eprintln!("payments: storing a webhook event failed: {e:#}");
            if engine_db::health::is_unavailable_any(&e) {
                (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, "error")
            }
        }
    }
}

fn not_found() -> Response {
    plain(StatusCode::NOT_FOUND, "No such test checkout.")
}

async fn fake_page(
    State(ctx): State<Ctx>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(fake) = ctx.payments.fake() else {
        return not_found();
    };
    let Some(s) = fake.session(&session) else {
        return not_found();
    };
    let (csrf, set) = ctx.anon.token(&headers);
    let deliveries = fake.deliveries(&session);
    let mut r = ctx.ui.page(
        StatusCode::OK,
        "payments/fake_checkout.html",
        context! {
            title => "Test checkout", session => s, csrf => csrf,
            deliveries => deliveries.len(),
            deliveries_url => format!("/_engine/payments/fake/deliveries/{session}/"),
        },
    );
    if let Some(c) = set {
        r.headers_mut().append(header::SET_COOKIE, c);
    }
    r
}

async fn fake_post(
    State(ctx): State<Ctx>,
    Path(session): Path<String>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let Some(fake) = ctx.payments.fake() else {
        return not_found();
    };
    if !ctx.anon.check(&headers, &form) {
        return plain(StatusCode::FORBIDDEN, BAD_TOKEN);
    }
    let Some(s) = fake.session(&session) else {
        return not_found();
    };
    let Some(outcome) = form.get("outcome").and_then(Outcome::parse) else {
        return plain(StatusCode::BAD_REQUEST, "Choose what happens.");
    };
    let Some((event, body, sig)) = fake.event(&session, outcome) else {
        return plain(
            StatusCode::CONFLICT,
            &format!("This test checkout is {} already.", s.state),
        );
    };
    let mut h = HeaderMap::new();
    h.insert(
        HeaderName::from_static(crate::fake::SIGNATURE_HEADER),
        HeaderValue::from_str(&sig).expect("hex and digits"),
    );
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let (status, _) = deliver(&ctx.payments, &h, &body).await;
    println!(
        "payments: fake provider delivered {event} for {session} ({})",
        status.as_u16()
    );
    fake.record(
        &session,
        Delivery {
            event,
            signature: sig,
            body: String::from_utf8_lossy(&body).into_owned(),
            status: status.as_u16(),
        },
    );
    match outcome {
        Outcome::Pay | Outcome::Underpay => see_other(&s.success_url),
        Outcome::Refund => see_other(&format!("/_engine/payments/fake/checkout/{session}/")),
        _ => see_other(&s.cancel_url),
    }
}

async fn fake_deliveries(State(ctx): State<Ctx>, Path(session): Path<String>) -> Response {
    let Some(fake) = ctx.payments.fake() else {
        return not_found();
    };
    if fake.session(&session).is_none() {
        return not_found();
    }
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "signature_header": "Wrought-Signature",
        "webhook": "/_engine/payments/webhook/fake/",
        "deliveries": fake.deliveries(&session),
    }))
    .unwrap_or_default();
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    secure_headers(r.headers_mut());
    r
}
