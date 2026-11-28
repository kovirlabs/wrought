//! The fake provider: a hosted checkout served by the engine itself, for
//! tests and examples. No money moves and no network is used.
//!
//! Checkout sends the buyer to `/_engine/payments/fake/checkout/<session>/`,
//! a page that says it is a test and offers to pay, decline, cancel, let
//! the checkout expire, pay too little (to see the amount check), and,
//! once paid, refund. Each choice makes an event, **signs** it in the same
//! `t=...,v1=...` scheme as Stripe (header `Hammerwright-Signature`) and hands
//! it to the same webhook handling as a real delivery
//! ([`crate::Payments::receive`]): verification, replay guard, job.
//!
//! Every delivery (raw body and signature header) is kept in memory and
//! listed at `/_engine/payments/fake/deliveries/<session>/` as JSON, so a
//! delivery can be replayed by hand with curl.
//!
//! The fake provider only runs when `ENGINE_PAYMENTS_FAKE=1` is set
//! (see [`crate::provider`]). Its signing secret is random per process,
//! or `ENGINE_PAYMENTS_FAKE_SECRET` when set (to sign events by hand).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::provider::{
    BoxFuture, CheckoutRequest, CheckoutSession, Event, EventKind, Provider, ProviderError,
    WebhookError,
};
use crate::signature;

/// The fake provider's signature header.
pub const SIGNATURE_HEADER: &str = "hammerwright-signature";
pub const SECRET_ENV: &str = "ENGINE_PAYMENTS_FAKE_SECRET";

/// A fake checkout session.
#[derive(Debug, Clone, Serialize)]
pub struct FakeSession {
    pub id: String,
    pub order: String,
    pub currency: String,
    pub total: i64,
    pub total_display: String,
    pub lines: Vec<crate::provider::CheckoutLine>,
    pub success_url: String,
    pub cancel_url: String,
    /// `open`, `paid`, `failed`, `cancelled`, `expired`, `refunded`.
    pub state: String,
    pub payment: String,
}

/// One webhook delivery, kept for replays.
#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub event: String,
    pub signature: String,
    pub body: String,
    /// What the webhook handling answered (200, 400, 500).
    pub status: u16,
}

/// What the buyer chose on the fake page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pay,
    Underpay,
    Decline,
    Cancel,
    Expire,
    Refund,
}

impl Outcome {
    pub fn parse(s: &str) -> Option<Outcome> {
        Some(match s {
            "pay" => Outcome::Pay,
            "underpay" => Outcome::Underpay,
            "decline" => Outcome::Decline,
            "cancel" => Outcome::Cancel,
            "expire" => Outcome::Expire,
            "refund" => Outcome::Refund,
            _ => return None,
        })
    }
}

pub struct Fake {
    secret: String,
    sessions: Mutex<HashMap<String, FakeSession>>,
    by_key: Mutex<HashMap<String, String>>,
    deliveries: Mutex<HashMap<String, Vec<Delivery>>>,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fake { .. }")
    }
}

fn rand_id(prefix: &str) -> String {
    let t = engine_core::web::secret::token().replace(['-', '_'], "x");
    format!("{prefix}{}", &t[..24])
}

impl Fake {
    pub fn new(secret: String) -> Self {
        Fake {
            secret,
            sessions: Mutex::default(),
            by_key: Mutex::default(),
            deliveries: Mutex::default(),
        }
    }

    pub fn from_env() -> Self {
        let secret = std::env::var(SECRET_ENV)
            .ok()
            .filter(|s| s.len() >= 16)
            .unwrap_or_else(engine_core::web::secret::token);
        Fake::new(secret)
    }

    pub fn session(&self, id: &str) -> Option<FakeSession> {
        self.sessions.lock().ok()?.get(id).cloned()
    }

    pub fn deliveries(&self, session: &str) -> Vec<Delivery> {
        self.deliveries
            .lock()
            .ok()
            .and_then(|d| d.get(session).cloned())
            .unwrap_or_default()
    }

    pub fn record(&self, session: &str, d: Delivery) {
        if let Ok(mut m) = self.deliveries.lock() {
            m.entry(session.to_string()).or_default().push(d);
        }
    }

    /// The event for `outcome` on a session, as (event id, raw body,
    /// signature header), and the session's new state. `None` if the
    /// session is unknown or the outcome does not fit its state.
    pub fn event(&self, session: &str, outcome: Outcome) -> Option<(String, Vec<u8>, String)> {
        let mut sessions = self.sessions.lock().ok()?;
        let s = sessions.get_mut(session)?;
        let open = s.state == "open";
        let (ty, state, amount) = match outcome {
            Outcome::Pay if open => ("checkout.paid", "paid", s.total),
            Outcome::Underpay if open => ("checkout.paid", "paid", s.total - 1),
            Outcome::Decline if open => ("checkout.failed", "failed", s.total),
            Outcome::Cancel if open => ("checkout.cancelled", "cancelled", s.total),
            Outcome::Expire if open => ("checkout.expired", "expired", s.total),
            Outcome::Refund if s.state == "paid" => ("payment.refunded", "refunded", s.total),
            _ => return None,
        };
        s.state = state.into();
        let id = rand_id("evt_fake_");
        let body = json!({
            "id": id,
            "type": ty,
            "created": crate::now(),
            "session": s.id,
            "order": s.order,
            "payment": s.payment,
            "amount": amount,
            "currency": s.currency,
        })
        .to_string()
        .into_bytes();
        let header = signature::header(&self.secret, crate::now(), &body);
        Some((id, body, header))
    }

    /// Sign a body as the fake provider would (tests).
    pub fn sign(&self, timestamp: i64, body: &[u8]) -> String {
        signature::header(&self.secret, timestamp, body)
    }
}

/// Map a fake event's JSON to the engine's terms.
pub fn parse_event(v: &Json) -> Result<Option<Event>, WebhookError> {
    let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let id = s("id").ok_or_else(|| WebhookError::Payload("no event id".into()))?;
    let ty = s("type").ok_or_else(|| WebhookError::Payload("no event type".into()))?;
    let kind = match ty.as_str() {
        "checkout.paid" => EventKind::Paid,
        "checkout.failed" => EventKind::Failed,
        "checkout.cancelled" => EventKind::Cancelled,
        "checkout.expired" => EventKind::Expired,
        "payment.refunded" => EventKind::Refunded,
        _ => return Ok(None),
    };
    Ok(Some(Event {
        id,
        kind,
        provider_type: ty,
        order: s("order"),
        session: s("session"),
        payment: s("payment"),
        amount: v["amount"].as_i64(),
        currency: s("currency"),
    }))
}

impl Provider for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn create_checkout<'a>(
        &'a self,
        req: &'a CheckoutRequest,
    ) -> BoxFuture<'a, Result<CheckoutSession, ProviderError>> {
        Box::pin(async move {
            let mut keys = self
                .by_key
                .lock()
                .map_err(|_| ProviderError::Unexpected("lock".into()))?;
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| ProviderError::Unexpected("lock".into()))?;
            let id = match keys.get(&req.idempotency_key) {
                Some(id) => id.clone(),
                None => {
                    let id = rand_id("cs_fake_");
                    sessions.insert(
                        id.clone(),
                        FakeSession {
                            id: id.clone(),
                            order: req.order.clone(),
                            currency: req.currency.clone(),
                            total: req.total,
                            total_display: crate::money::format(req.total, &req.currency),
                            lines: req.lines.clone(),
                            success_url: req.success_url.clone(),
                            cancel_url: req.cancel_url.clone(),
                            state: "open".into(),
                            payment: rand_id("pi_fake_"),
                        },
                    );
                    keys.insert(req.idempotency_key.clone(), id.clone());
                    id
                }
            };
            Ok(CheckoutSession {
                url: format!("/_engine/payments/fake/checkout/{id}/"),
                id,
            })
        })
    }

    fn parse_webhook(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: i64,
        tolerance: i64,
    ) -> Result<Option<Event>, WebhookError> {
        let header = headers.get(SIGNATURE_HEADER).and_then(|v| v.to_str().ok());
        signature::verify(header, body, &self.secret, now, tolerance)?;
        let v: Json = serde_json::from_slice(body)
            .map_err(|e| WebhookError::Payload(format!("not JSON ({e})")))?;
        parse_event(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn req(key: &str) -> CheckoutRequest {
        CheckoutRequest {
            order: "ord_abcdefgh".into(),
            currency: "GBP".into(),
            lines: vec![],
            total: 1000,
            success_url: "/ok".into(),
            cancel_url: "/cart".into(),
            idempotency_key: key.into(),
            customer_email: None,
        }
    }

    #[tokio::test]
    async fn sessions_are_idempotent_and_events_verify() {
        let f = Fake::new("a test secret, long enough".into());
        let a = f.create_checkout(&req("k1")).await.unwrap();
        let b = f.create_checkout(&req("k1")).await.unwrap();
        let c = f.create_checkout(&req("k2")).await.unwrap();
        assert_eq!(a, b);
        assert_ne!(a.id, c.id);
        assert!(f.event(&a.id, Outcome::Refund).is_none(), "not paid yet");
        let (id, body, sig) = f.event(&a.id, Outcome::Pay).unwrap();
        assert!(f.event(&a.id, Outcome::Pay).is_none(), "paid once");
        let mut h = HeaderMap::new();
        h.insert(SIGNATURE_HEADER, HeaderValue::from_str(&sig).unwrap());
        let ev = f
            .parse_webhook(&h, &body, crate::now(), 300)
            .unwrap()
            .unwrap();
        assert_eq!(ev.id, id);
        assert_eq!(ev.kind, EventKind::Paid);
        assert_eq!(ev.amount, Some(1000));
        assert_eq!(ev.order.as_deref(), Some("ord_abcdefgh"));
        // Another provider's secret does not verify.
        let other = Fake::new("another secret, long enough".into());
        assert!(other.parse_webhook(&h, &body, crate::now(), 300).is_err());
        assert!(f.event(&a.id, Outcome::Refund).is_some());
    }
}
