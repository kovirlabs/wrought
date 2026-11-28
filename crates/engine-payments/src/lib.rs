//! Hammerwright's `payments` feature: checkout through a pluggable provider,
//! signed webhooks, orders with a state machine, a server-side cart, and
//! the Commerce components.
//!
//! **PostgreSQL only.** The spec gives `payments` the dependencies `auth`
//! and `jobs`; `jobs` needs Postgres, so payments does too.
//!
//! How a purchase goes:
//!
//! 1. A signed-in visitor adds products to their cart
//!    (`POST /_engine/shop/cart/`). The cart stores product ids and
//!    quantities only.
//! 2. Checkout (`POST /_engine/shop/checkout/`) prices the cart again from
//!    the catalogue ([`catalogue`]), stores a `pending` order with its
//!    lines, asks the provider for a hosted checkout and sends the visitor
//!    there. The form carries an idempotency key, so posting it twice
//!    makes one order; the provider gets a key too.
//! 3. The provider calls `POST /_engine/payments/webhook/<provider>/`. The
//!    signature is checked over the raw body with a timestamp tolerance;
//!    forged or stale deliveries get 400 and change nothing. The event is
//!    stored under its provider event id (a replay is a 200 no-op) and a
//!    `payments.apply` job is queued in the same transaction.
//! 4. The job applies the event to the order ([`orders::decide`]): amount
//!    and currency must match the order, illegal moves are refused and
//!    logged, every change is version-checked.
//!
//! Providers: [`stripe::Stripe`] and [`fake::Fake`] (in-process, for tests
//! and examples; refused unless `ENGINE_PAYMENTS_FAKE=1`).

pub mod catalogue;
pub mod fake;
pub mod money;
pub mod orders;
pub mod provider;
pub mod signature;
pub mod site;
pub mod sql;
pub mod stripe;
pub mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use axum::http::HeaderMap;
use engine_db::{Db, Migration};
use serde::Deserialize;

pub use catalogue::{Catalogue, FileCatalogue, Product};
pub use orders::{Cart, Order, OrderStore};
pub use provider::{Event, EventKind, Provider, WebhookError};

/// The job kind that applies a stored provider event.
pub const JOB_KIND: &str = "payments.apply";

/// Environment variable that allows the fake provider.
pub const FAKE_ENV: &str = "ENGINE_PAYMENTS_FAKE";

/// The `payments` feature's schema.
pub fn migrations() -> Vec<Migration> {
    vec![
        Migration::new(
            "payments/0001_payments",
            include_str!("../migrations/postgres/0001_payments.sql"),
        ),
        Migration::new(
            "payments/0002_settlement",
            include_str!("../migrations/postgres/0002_settlement.sql"),
        ),
    ]
}

/// Which provider takes the money.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Stripe,
    Fake,
}

fn d_tolerance() -> i64 {
    300
}
fn d_success() -> String {
    "/shop/orders/".into()
}
fn d_cancel() -> String {
    "/shop/cart/".into()
}

/// `[payments]` in `site.toml`. No keys here: Stripe's come from the
/// environment.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentsConfig {
    pub provider: ProviderKind,
    /// The shop's currency (ISO 4217, e.g. `GBP`). One per shop.
    pub currency: String,
    /// Products from this content type (needs the `content` feature).
    #[serde(default)]
    pub product_type: Option<String>,
    /// Products from this TOML file when there is no `product_type`
    /// (default `products.toml`).
    #[serde(default)]
    pub products_file: Option<PathBuf>,
    /// Where the provider sends buyers after paying (`?order=<id>` is
    /// added).
    #[serde(default = "d_success")]
    pub success_path: String,
    /// Where buyers who give up land.
    #[serde(default = "d_cancel")]
    pub cancel_path: String,
    /// How far a webhook's signed timestamp may be from now, in seconds.
    #[serde(default = "d_tolerance")]
    pub webhook_tolerance_seconds: i64,
}

impl PaymentsConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Option<Self>> {
        let Some(t) = section else {
            return Ok(None);
        };
        let cfg: PaymentsConfig = t
            .clone()
            .try_into()
            .map_err(|e| anyhow::anyhow!("[payments]: {e}"))?;
        if !money::valid_currency(&cfg.currency) {
            bail!(
                "[payments] currency `{}` must be an ISO 4217 code such as GBP",
                cfg.currency
            );
        }
        for (k, p) in [
            ("success_path", &cfg.success_path),
            ("cancel_path", &cfg.cancel_path),
        ] {
            if engine_core::web::safe_path(Some(p)).is_none() || p.contains('?') {
                bail!("[payments] {k} must be a path on this site, like /shop/");
            }
        }
        if !(30..=3600).contains(&cfg.webhook_tolerance_seconds) {
            bail!("[payments] webhook_tolerance_seconds must be between 30 and 3600");
        }
        if cfg.product_type.is_some() && cfg.products_file.is_some() {
            bail!("[payments] has both product_type and products_file; choose one");
        }
        if let Some(f) = &cfg.products_file
            && (f.is_absolute() || f.components().any(|c| c.as_os_str() == ".."))
        {
            bail!("[payments] products_file must be a file inside the site");
        }
        Ok(Some(cfg))
    }

    /// The products file, when products come from a file.
    pub fn products_file(&self) -> Option<PathBuf> {
        match &self.product_type {
            Some(_) => None,
            None => Some(
                self.products_file
                    .clone()
                    .unwrap_or_else(|| "products.toml".into()),
            ),
        }
    }
}

/// Build the configured provider. The fake one only with
/// `ENGINE_PAYMENTS_FAKE=1`, so a `site.toml` that names it cannot go live
/// by accident; Stripe only with its keys in the environment.
pub fn provider(cfg: &PaymentsConfig) -> Result<Arc<dyn Provider>> {
    match cfg.provider {
        ProviderKind::Stripe => Ok(Arc::new(stripe::Stripe::new(
            stripe::StripeConfig::from_env()?,
        )?)),
        ProviderKind::Fake => {
            if std::env::var(FAKE_ENV).ok().as_deref() != Some("1") {
                bail!(
                    "[payments] provider = \"fake\" takes no money and accepts every payment; it \
                     only runs when {FAKE_ENV}=1 is set in the environment (for tests and local \
                     development). Use provider = \"stripe\" for a live shop."
                );
            }
            Ok(Arc::new(fake::Fake::from_env()))
        }
    }
}

/// The shop of one site. Cheap to clone.
#[derive(Clone)]
pub struct Payments {
    inner: Arc<Inner>,
}

struct Inner {
    store: OrderStore,
    provider: Arc<dyn Provider>,
    catalogue: Arc<dyn Catalogue>,
    config: PaymentsConfig,
    public_url: String,
}

/// What became of a webhook delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// Stored and queued.
    New,
    /// Seen before: nothing done.
    Duplicate,
    /// Genuine, but of a type the engine does not act on.
    Ignored,
}

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Payments {
    pub fn new(
        db: Db,
        provider: Arc<dyn Provider>,
        catalogue: Arc<dyn Catalogue>,
        config: PaymentsConfig,
        public_url: &str,
    ) -> Result<Self> {
        Ok(Payments {
            inner: Arc::new(Inner {
                store: OrderStore::new(db),
                provider,
                catalogue,
                config,
                public_url: public_url.trim_end_matches('/').to_string(),
            }),
        })
    }

    pub fn store(&self) -> &OrderStore {
        &self.inner.store
    }

    pub fn provider(&self) -> &dyn Provider {
        self.inner.provider.as_ref()
    }

    pub fn catalogue(&self) -> &dyn Catalogue {
        self.inner.catalogue.as_ref()
    }

    pub fn config(&self) -> &PaymentsConfig {
        &self.inner.config
    }

    pub fn public_url(&self) -> &str {
        &self.inner.public_url
    }

    /// The fake provider, when it is the one in use.
    pub fn fake(&self) -> Option<&fake::Fake> {
        self.provider().as_any().downcast_ref::<fake::Fake>()
    }

    /// The visitor's priced cart.
    pub async fn cart(&self, user: i64) -> Result<Cart> {
        let lines = self.store().cart_lines(user).await?;
        orders::price_cart(&lines, self.catalogue(), &self.config().currency).await
    }

    /// Handle one webhook delivery: verify, store, queue. Used by the HTTP
    /// endpoint and by the fake provider's deliveries alike.
    pub async fn receive(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<Received, ReceiveError> {
        let ev = self
            .provider()
            .parse_webhook(
                headers,
                body,
                now(),
                self.config().webhook_tolerance_seconds,
            )
            .map_err(ReceiveError::Refused)?;
        let Some(ev) = ev else {
            return Ok(Received::Ignored);
        };
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| ReceiveError::Refused(WebhookError::Payload(e.to_string())))?;
        match self
            .store()
            .record_event(self.provider().name(), &ev, &payload)
            .await
            .map_err(ReceiveError::Internal)?
        {
            orders::Recorded::New(_) => Ok(Received::New),
            orders::Recorded::Duplicate => Ok(Received::Duplicate),
        }
    }
}

/// Why a delivery was not taken.
#[derive(Debug, thiserror::Error)]
pub enum ReceiveError {
    /// Forged, stale or unreadable: 400.
    #[error("{0}")]
    Refused(WebhookError),
    /// Our side failed (database): 500, the provider retries.
    #[error("{0:#}")]
    Internal(anyhow::Error),
}

/// Register the job that applies provider events.
pub fn register_jobs(handlers: &mut engine_jobs::Handlers, db: Db) {
    let store = OrderStore::new(db);
    handlers.register(JOB_KIND, move |ctx| {
        let store = store.clone();
        async move {
            let id = ctx.payload["event"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("payments.apply without an event id"))?;
            let r = store.apply_event(id).await?;
            if r.status == "applied" {
                println!(
                    "payments: event {id} applied to order {}",
                    r.order.as_deref().unwrap_or("?")
                );
            }
            Ok(())
        }
    });
}

/// The products file of a site, resolved (for messages).
pub fn products_path(site: &Path, cfg: &PaymentsConfig) -> Option<PathBuf> {
    cfg.products_file().map(|f| site.join(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Option<PaymentsConfig>> {
        let t: toml::Table = toml::from_str(s).unwrap();
        PaymentsConfig::from_site(Some(&t))
    }

    #[test]
    fn config() {
        let c = parse("provider = \"fake\"\ncurrency = \"GBP\"")
            .unwrap()
            .unwrap();
        assert_eq!(c.products_file(), Some(PathBuf::from("products.toml")));
        assert_eq!(c.success_path, "/shop/orders/");
        let c = parse("provider = \"stripe\"\ncurrency = \"JPY\"\nproduct_type = \"product\"")
            .unwrap()
            .unwrap();
        assert_eq!(c.products_file(), None);
        for bad in [
            "provider = \"paypal\"\ncurrency = \"GBP\"",
            "provider = \"fake\"\ncurrency = \"gbp\"",
            "provider = \"fake\"\ncurrency = \"GBP\"\nsecret_key = \"sk\"",
            "provider = \"fake\"\ncurrency = \"GBP\"\nsuccess_path = \"https://evil.test/\"",
            "provider = \"fake\"\ncurrency = \"GBP\"\nproducts_file = \"../x.toml\"",
            "provider = \"fake\"\ncurrency = \"GBP\"\nwebhook_tolerance_seconds = 99999",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(PaymentsConfig::from_site(None).unwrap().is_none());
    }

    #[test]
    fn the_fake_provider_needs_its_variable() {
        let c = parse("provider = \"fake\"\ncurrency = \"GBP\"")
            .unwrap()
            .unwrap();
        // The test environment does not set ENGINE_PAYMENTS_FAKE.
        if std::env::var(FAKE_ENV).is_err() {
            let e = provider(&c).err().unwrap().to_string();
            assert!(e.contains(FAKE_ENV), "{e}");
        }
    }
}
