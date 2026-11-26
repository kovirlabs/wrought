//! Stripe, through Checkout Sessions.
//!
//! * **Checkout:** `POST /v1/checkout/sessions` (form-encoded, `mode =
//!   payment`) with the engine's own prices as `price_data` lines, the
//!   order's public id as `client_reference_id` and in `metadata[order]`
//!   (also on the PaymentIntent, so refunds can be traced), and an
//!   `Idempotency-Key` header so a retried request makes no second
//!   session. The visitor is sent to the session's `url`.
//! * **Webhooks:** `Stripe-Signature` checked over the raw body (see
//!   [`crate::signature`]), then `checkout.session.completed` (when
//!   `payment_status` is `paid`), `checkout.session.async_payment_succeeded`,
//!   `...async_payment_failed`, `checkout.session.expired` and
//!   `charge.refunded` (full refunds only) become events; other types are
//!   acknowledged and ignored.
//! * **Keys** come from the environment only: `ENGINE_STRIPE_SECRET_KEY`
//!   and `ENGINE_STRIPE_WEBHOOK_SECRET`. They are never printed (`Debug`
//!   redacts them) and never read from `site.toml`.

use std::time::Duration;

use axum::http::HeaderMap;
use serde_json::Value as Json;

use crate::provider::{
    BoxFuture, CheckoutRequest, CheckoutSession, Event, EventKind, Provider, ProviderError,
    WebhookError,
};
use crate::signature;

pub const SECRET_KEY_ENV: &str = "ENGINE_STRIPE_SECRET_KEY";
pub const WEBHOOK_SECRET_ENV: &str = "ENGINE_STRIPE_WEBHOOK_SECRET";
/// The only host the secret key is ever sent to.
pub const API_HOST: &str = "api.stripe.com";
const API_BASE: &str = "https://api.stripe.com";

/// Stripe's keys, from the environment ([`StripeConfig::from_env`]). There is
/// no setting for the API address: the key goes to `https://api.stripe.com`
/// and nowhere else (the crate's own tests use a `#[cfg(test)]` constructor
/// that allows a loopback mock server).
#[derive(Clone)]
pub struct StripeConfig {
    pub secret_key: String,
    pub webhook_secret: String,
}

impl std::fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeConfig")
            .field("secret_key", &"[redacted]")
            .field("webhook_secret", &"[redacted]")
            .finish()
    }
}

impl StripeConfig {
    /// Read the keys from the environment; both must be set.
    pub fn from_env() -> anyhow::Result<Self> {
        let get = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let (Some(secret_key), Some(webhook_secret)) =
            (get(SECRET_KEY_ENV), get(WEBHOOK_SECRET_ENV))
        else {
            anyhow::bail!(
                "[payments] provider = \"stripe\" needs {SECRET_KEY_ENV} and {WEBHOOK_SECRET_ENV} \
                 in the environment (keys are never read from site.toml)"
            );
        };
        Ok(StripeConfig {
            secret_key,
            webhook_secret,
        })
    }
}

/// Whether a URL's host is a loopback address (`localhost`, 127.0.0.0/8,
/// `::1`).
pub fn is_loopback(u: &reqwest::Url) -> bool {
    use std::net::IpAddr;
    match u.host_str() {
        Some(h) if h.eq_ignore_ascii_case("localhost") => true,
        Some(h) => h
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|a| a.is_loopback()),
        None => false,
    }
}

/// Check the address the secret key would be sent to: `https` to exactly
/// `api.stripe.com` (default port, no user name or password, no path,
/// query or fragment). With `allow_loopback_http` (tests only), plain
/// `http` to a loopback address is accepted too.
pub fn check_api_base(base: &str, allow_loopback_http: bool) -> anyhow::Result<reqwest::Url> {
    let u = reqwest::Url::parse(base)
        .map_err(|e| anyhow::anyhow!("the Stripe API address is not a URL ({e})"))?;
    if !u.username().is_empty() || u.password().is_some() {
        anyhow::bail!("the Stripe API address must not carry a user name or password");
    }
    if u.query().is_some() || u.fragment().is_some() || !matches!(u.path(), "" | "/") {
        anyhow::bail!("the Stripe API address must be a bare origin");
    }
    let stripe = u.scheme() == "https" && u.host_str() == Some(API_HOST) && u.port().is_none();
    let mock = allow_loopback_http && u.scheme() == "http" && is_loopback(&u);
    if !(stripe || mock) {
        anyhow::bail!(
            "the Stripe secret key is only sent over https to {API_HOST} (refused: {}://{})",
            u.scheme(),
            u.host_str().unwrap_or("?")
        );
    }
    Ok(u)
}

/// Whether a checkout URL from Stripe may be shown to the buyer: https, or
/// http on loopback when tests allow it.
fn acceptable_redirect(url: &str, allow_loopback_http: bool) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.username().is_empty()
                && u.password().is_none()
                && u.host_str().is_some()
                && (u.scheme() == "https"
                    || (allow_loopback_http && u.scheme() == "http" && is_loopback(&u)))
        }
        Err(_) => false,
    }
}

/// The Stripe provider.
pub struct Stripe {
    cfg: StripeConfig,
    api_base: reqwest::Url,
    allow_loopback_http: bool,
    http: reqwest::Client,
}

impl std::fmt::Debug for Stripe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stripe")
            .field("cfg", &self.cfg)
            .field("api_base", &self.api_base.as_str())
            .finish()
    }
}

impl Stripe {
    /// The provider, talking to `https://api.stripe.com`.
    pub fn new(cfg: StripeConfig) -> anyhow::Result<Self> {
        Self::build(cfg, API_BASE, false)
    }

    /// A provider talking to a mock server on loopback `http` (this crate's
    /// tests only; neither `site.toml` nor the environment can do this).
    #[cfg(test)]
    pub(crate) fn with_mock(cfg: StripeConfig, base: &str) -> anyhow::Result<Self> {
        Self::build(cfg, base, true)
    }

    fn build(cfg: StripeConfig, base: &str, allow_loopback_http: bool) -> anyhow::Result<Self> {
        let api_base = check_api_base(base, allow_loopback_http)?;
        // The client carries the key: it never follows a redirect, so the
        // key cannot be sent on to another host.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("wrought/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Stripe {
            cfg,
            api_base,
            allow_loopback_http,
            http,
        })
    }

    /// The form body of a Checkout Session request.
    pub fn session_form(req: &CheckoutRequest) -> Vec<(String, String)> {
        let mut f: Vec<(String, String)> = vec![
            ("mode".into(), "payment".into()),
            ("success_url".into(), req.success_url.clone()),
            ("cancel_url".into(), req.cancel_url.clone()),
            ("client_reference_id".into(), req.order.clone()),
            ("metadata[order]".into(), req.order.clone()),
            (
                "payment_intent_data[metadata][order]".into(),
                req.order.clone(),
            ),
        ];
        if let Some(e) = &req.customer_email {
            f.push(("customer_email".into(), e.clone()));
        }
        let cur = req.currency.to_ascii_lowercase();
        for (i, l) in req.lines.iter().enumerate() {
            let p = format!("line_items[{i}]");
            f.push((format!("{p}[quantity]"), l.quantity.to_string()));
            f.push((format!("{p}[price_data][currency]"), cur.clone()));
            f.push((
                format!("{p}[price_data][unit_amount]"),
                l.unit_amount.to_string(),
            ));
            f.push((
                format!("{p}[price_data][product_data][name]"),
                l.name.clone(),
            ));
        }
        f
    }

    async fn create(&self, req: &CheckoutRequest) -> Result<CheckoutSession, ProviderError> {
        // Checked again right before the key is attached.
        let base = check_api_base(self.api_base.as_str(), self.allow_loopback_http)
            .map_err(|e| ProviderError::Unexpected(e.to_string()))?;
        let url = base
            .join("/v1/checkout/sessions")
            .map_err(|e| ProviderError::Unexpected(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.cfg.secret_key)
            .header("Idempotency-Key", &req.idempotency_key)
            .form(&Self::session_form(req))
            .send()
            .await
            // reqwest errors carry the URL, never the key.
            .map_err(|e| ProviderError::Unreachable(e.without_url().to_string()))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(ProviderError::Unexpected(format!(
                "Stripe answered with a redirect ({status}); not followed"
            )));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::Unreachable(e.without_url().to_string()))?;
        let v: Json = serde_json::from_slice(&body)
            .map_err(|e| ProviderError::Unexpected(format!("not JSON ({e})")))?;
        if !status.is_success() {
            let message = v["error"]["message"]
                .as_str()
                .unwrap_or("no message")
                .chars()
                .take(300)
                .collect();
            return Err(ProviderError::Refused {
                status: status.as_u16(),
                message,
            });
        }
        match (v["id"].as_str(), v["url"].as_str()) {
            (Some(id), Some(url)) if acceptable_redirect(url, self.allow_loopback_http) => {
                Ok(CheckoutSession {
                    id: id.to_string(),
                    url: url.to_string(),
                })
            }
            _ => Err(ProviderError::Unexpected(
                "the session has no id or no https url".into(),
            )),
        }
    }
}

fn s(v: &Json) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Turn a verified Stripe event into the engine's terms (`None`: a type
/// the engine does not act on).
pub fn parse_event(v: &Json) -> Result<Option<Event>, WebhookError> {
    let id = s(&v["id"]).ok_or_else(|| WebhookError::Payload("no event id".into()))?;
    let ty = s(&v["type"]).ok_or_else(|| WebhookError::Payload("no event type".into()))?;
    let o = &v["data"]["object"];
    let session_event = |kind| Event {
        id: id.clone(),
        kind,
        provider_type: ty.clone(),
        order: s(&o["client_reference_id"]).or_else(|| s(&o["metadata"]["order"])),
        session: s(&o["id"]),
        payment: s(&o["payment_intent"]),
        amount: o["amount_total"].as_i64(),
        currency: s(&o["currency"]).map(|c| c.to_ascii_uppercase()),
    };
    Ok(match ty.as_str() {
        "checkout.session.completed" => match o["payment_status"].as_str() {
            Some("paid") | Some("no_payment_required") => Some(session_event(EventKind::Paid)),
            // `unpaid`: a delayed method; an async_payment event follows.
            _ => None,
        },
        "checkout.session.async_payment_succeeded" => Some(session_event(EventKind::Paid)),
        "checkout.session.async_payment_failed" => Some(session_event(EventKind::Failed)),
        "checkout.session.expired" => Some(session_event(EventKind::Expired)),
        "charge.refunded" if o["refunded"].as_bool() == Some(true) => Some(Event {
            id,
            kind: EventKind::Refunded,
            provider_type: ty,
            order: s(&o["metadata"]["order"]),
            session: None,
            payment: s(&o["payment_intent"]),
            amount: o["amount_refunded"].as_i64(),
            currency: s(&o["currency"]).map(|c| c.to_ascii_uppercase()),
        }),
        _ => None,
    })
}

impl Provider for Stripe {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn create_checkout<'a>(
        &'a self,
        req: &'a CheckoutRequest,
    ) -> BoxFuture<'a, Result<CheckoutSession, ProviderError>> {
        Box::pin(self.create(req))
    }

    fn parse_webhook(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: i64,
        tolerance: i64,
    ) -> Result<Option<Event>, WebhookError> {
        let header = headers
            .get("stripe-signature")
            .and_then(|v| v.to_str().ok());
        signature::verify(header, body, &self.cfg.webhook_secret, now, tolerance)?;
        let v: Json = serde_json::from_slice(body)
            .map_err(|e| WebhookError::Payload(format!("not JSON ({e})")))?;
        parse_event(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> CheckoutRequest {
        CheckoutRequest {
            order: "ord_abc".into(),
            currency: "GBP".into(),
            lines: vec![crate::provider::CheckoutLine {
                name: "Mug".into(),
                unit_amount: 1250,
                quantity: 2,
            }],
            total: 2500,
            success_url: "https://shop.test/ok".into(),
            cancel_url: "https://shop.test/cart".into(),
            idempotency_key: "checkout-ord_abc".into(),
            customer_email: Some("ann@example.org".into()),
        }
    }

    #[test]
    fn form_carries_server_prices_and_the_order() {
        let f = Stripe::session_form(&req());
        let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        assert_eq!(get("mode"), Some("payment"));
        assert_eq!(get("client_reference_id"), Some("ord_abc"));
        assert_eq!(get("metadata[order]"), Some("ord_abc"));
        assert_eq!(get("line_items[0][price_data][currency]"), Some("gbp"));
        assert_eq!(get("line_items[0][price_data][unit_amount]"), Some("1250"));
        assert_eq!(get("line_items[0][quantity]"), Some("2"));
        assert_eq!(
            get("line_items[0][price_data][product_data][name]"),
            Some("Mug")
        );
    }

    #[test]
    fn debug_never_shows_keys() {
        let c = StripeConfig {
            secret_key: "sk_test_SECRET".into(),
            webhook_secret: "whsec_SECRET".into(),
        };
        let d = format!("{:?}", Stripe::new(c).unwrap());
        assert!(!d.contains("SECRET"), "{d}");
    }
}

#[cfg(test)]
#[path = "stripe_mock_tests.rs"]
mod mock_tests;
