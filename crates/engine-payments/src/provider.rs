//! The provider interface: create a hosted checkout, and turn a webhook
//! delivery into a normalised [`Event`] after checking its signature.
//!
//! Implementations: [`crate::stripe::Stripe`] (Checkout Sessions over
//! HTTPS) and [`crate::fake::Fake`] (an in-process stand-in for tests and
//! examples). Adding a provider means implementing [`Provider`]; the
//! webhook endpoint, replay protection, the job and the order state
//! machine are shared.

use std::future::Future;
use std::pin::Pin;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::signature::SignatureError;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One line of a checkout, priced by the engine (never by the client).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckoutLine {
    pub name: String,
    /// Price of one, in minor units.
    pub unit_amount: i64,
    pub quantity: i64,
}

/// What the engine asks a provider for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckoutRequest {
    /// The order's public id (`ord_...`): the provider sends it back in its
    /// events (Stripe: `client_reference_id`).
    pub order: String,
    pub currency: String,
    pub lines: Vec<CheckoutLine>,
    /// The order total in minor units (the sum of the lines).
    pub total: i64,
    pub success_url: String,
    pub cancel_url: String,
    /// Sent to the provider so a retried request makes no second session.
    pub idempotency_key: String,
    pub customer_email: Option<String>,
}

/// A hosted checkout the visitor is sent to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckoutSession {
    pub id: String,
    pub url: String,
}

/// What happened, in the engine's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The money was taken.
    Paid,
    /// The payment was declined or failed.
    Failed,
    /// The checkout timed out unpaid.
    Expired,
    /// The buyer gave up.
    Cancelled,
    /// The whole payment was given back.
    Refunded,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Paid => "paid",
            EventKind::Failed => "failed",
            EventKind::Expired => "expired",
            EventKind::Cancelled => "cancelled",
            EventKind::Refunded => "refunded",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        Some(match s {
            "paid" => EventKind::Paid,
            "failed" => EventKind::Failed,
            "expired" => EventKind::Expired,
            "cancelled" => EventKind::Cancelled,
            "refunded" => EventKind::Refunded,
            _ => return None,
        })
    }
}

/// A verified provider event, normalised.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    /// The provider's own event id (unique per provider): replays are
    /// recognised by it.
    pub id: String,
    pub kind: EventKind,
    /// The provider's raw event type (`checkout.session.completed`).
    pub provider_type: String,
    /// The order's public id, when the event carries it.
    pub order: Option<String>,
    /// The checkout session id.
    pub session: Option<String>,
    /// The provider's payment id (Stripe: the PaymentIntent), which refund
    /// events refer to.
    pub payment: Option<String>,
    /// Amount the event is about, in minor units.
    pub amount: Option<i64>,
    /// Upper-case ISO code.
    pub currency: Option<String>,
}

/// Why a webhook delivery was refused (answered with 400; nothing stored).
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("bad signature: {0}")]
    Signature(#[from] SignatureError),
    #[error("unreadable event: {0}")]
    Payload(String),
}

/// Why creating a checkout failed.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("the payment provider could not be reached: {0}")]
    Unreachable(String),
    #[error("the payment provider refused the checkout ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("the payment provider's answer was not understood: {0}")]
    Unexpected(String),
}

/// A payment provider.
pub trait Provider: Send + Sync {
    /// `stripe`, `fake`: the last part of the webhook URL
    /// (`/_engine/payments/webhook/<name>/`).
    fn name(&self) -> &'static str;

    /// For reaching a concrete provider (the fake one's test pages).
    fn as_any(&self) -> &dyn std::any::Any;

    /// Create a hosted checkout for an order.
    fn create_checkout<'a>(
        &'a self,
        req: &'a CheckoutRequest,
    ) -> BoxFuture<'a, Result<CheckoutSession, ProviderError>>;

    /// Verify a webhook delivery (signature over the raw body, timestamp
    /// within `tolerance` seconds of `now`) and parse it. `Ok(None)`: a
    /// genuine event of a type the engine does not act on.
    fn parse_webhook(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: i64,
        tolerance: i64,
    ) -> Result<Option<Event>, WebhookError>;
}
