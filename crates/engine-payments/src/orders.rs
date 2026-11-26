//! Carts, orders and the order state machine.
//!
//! ```text
//!            paid (amount and currency match) ──> paid ──refunded──> refunded
//! pending ──<paid with the wrong amount/currency> ──> flagged
//!            failed / expired / cancelled ──> failed / expired / cancelled
//! ```
//!
//! An event that would move an order any other way (paying a cancelled
//! order, refunding an unpaid one, anything on a flagged order) is
//! refused: the order stays as it is, the event is kept with the reason,
//! and the refusal is logged. The same event kind again (a second "paid"
//! for a paid order) is a no-op. Every change is one transaction with an
//! optimistic version check on the order; the event row is locked while it
//! is applied, so a job run twice (delivery is at least once) applies it
//! once.

use anyhow::{Context, Result, bail};
use engine_db::{Db, Timestamp};
use serde::Serialize;

use crate::catalogue::Catalogue;
use crate::money;
use crate::provider::{Event, EventKind};
use crate::sql;

/// Most distinct products in one cart.
pub const MAX_CART_LINES: i64 = 50;
/// Most of one product in a cart.
pub const MAX_QUANTITY: i64 = 99;

/// Order states.
pub const STATUSES: [&str; 9] = [
    "pending",
    "processing",
    "paid",
    "failed",
    "expired",
    "cancelled",
    "refunded",
    "returned",
    "flagged",
];

/// What an event does to an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Move to this status.
    To(&'static str),
    /// Already there (a repeated event): nothing changes.
    Same,
    /// Money and order disagree: move to `flagged` for a person to check.
    Flag(String),
    /// Not a legal move: the order stays as it is.
    Refuse(String),
}

/// The state machine. `status` is the order's, `total`/`currency` its
/// amount; the event's amount and currency are checked against them.
pub fn decide(
    status: &str,
    total: i64,
    currency: &str,
    kind: EventKind,
    amount: Option<i64>,
    ev_currency: Option<&str>,
) -> Decision {
    let target = kind.as_str();
    if status == target {
        return Decision::Same;
    }
    let illegal = || Decision::Refuse(format!("a `{target}` event cannot move a {status} order"));
    match kind {
        EventKind::Processing => {
            if status == "pending" {
                Decision::To("processing")
            } else {
                illegal()
            }
        }
        EventKind::Paid => {
            if status != "pending" && status != "processing" {
                return illegal();
            }
            match (amount, ev_currency) {
                (Some(a), Some(c)) if a == total && c == currency => Decision::To("paid"),
                (Some(a), Some(c)) => Decision::Flag(format!(
                    "paid {} but the order is {}",
                    money::format(a, c),
                    money::format(total, currency)
                )),
                _ => Decision::Flag("the payment event has no amount or currency".into()),
            }
        }
        EventKind::Failed | EventKind::Expired | EventKind::Cancelled => {
            if status == "pending" || (status == "processing" && kind == EventKind::Failed) {
                Decision::To(target)
            } else {
                illegal()
            }
        }
        EventKind::Returned => {
            if status == "processing" || status == "paid" {
                Decision::To("returned")
            } else {
                illegal()
            }
        }
        EventKind::Refunded => {
            if status != "paid" {
                return illegal();
            }
            match (amount, ev_currency) {
                (Some(a), Some(c)) if a == total && c == currency => Decision::To("refunded"),
                _ => Decision::Refuse(
                    "only full refunds in the order's currency change an order".into(),
                ),
            }
        }
    }
}

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: i64,
    public_id: String,
    user_id: Option<i64>,
    status: String,
    currency: String,
    total: i64,
    provider: String,
    provider_session: Option<String>,
    provider_payment: Option<String>,
    checkout_url: Option<String>,
    note: Option<String>,
    version: i32,
    created_at: Timestamp,
    updated_at: Timestamp,
    paid_at: Option<Timestamp>,
}

/// One line of an order.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderLine {
    pub product: String,
    pub name: String,
    pub unit_amount: i64,
    pub unit_display: String,
    pub quantity: i32,
    pub amount: i64,
    pub amount_display: String,
}

/// An order with its lines.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Order {
    pub id: i64,
    pub public_id: String,
    pub user_id: Option<i64>,
    pub status: String,
    pub currency: String,
    pub total: i64,
    pub total_display: String,
    pub provider: String,
    pub provider_session: Option<String>,
    pub provider_payment: Option<String>,
    pub checkout_url: Option<String>,
    pub note: Option<String>,
    pub version: i32,
    pub created_at: String,
    pub updated_at: String,
    pub paid_at: Option<String>,
    pub lines: Vec<OrderLine>,
}

impl From<OrderRow> for Order {
    fn from(r: OrderRow) -> Order {
        Order {
            total_display: money::format(r.total, &r.currency),
            id: r.id,
            public_id: r.public_id,
            user_id: r.user_id,
            status: r.status,
            currency: r.currency,
            total: r.total,
            provider: r.provider,
            provider_session: r.provider_session,
            provider_payment: r.provider_payment,
            checkout_url: r.checkout_url,
            note: r.note,
            version: r.version,
            created_at: r.created_at.iso(),
            updated_at: r.updated_at.iso(),
            paid_at: r.paid_at.map(|t| t.iso()),
            lines: Vec::new(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct LineRow {
    order_id: i64,
    #[allow(dead_code)]
    position: i32,
    product: String,
    name: String,
    unit_amount: i64,
    quantity: i32,
    amount: i64,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    provider: String,
    event_id: String,
    kind: String,
    provider_type: String,
    order_ref: Option<String>,
    session: Option<String>,
    payment: Option<String>,
    amount: Option<i64>,
    currency: Option<String>,
    status: String,
    note: Option<String>,
    #[allow(dead_code)]
    order_id: Option<i64>,
    received_at: Timestamp,
}

/// A stored provider event (for the admin).
#[derive(Debug, Clone, Serialize)]
pub struct StoredEvent {
    pub id: i64,
    pub provider: String,
    pub event_id: String,
    pub kind: String,
    pub provider_type: String,
    pub amount_display: Option<String>,
    pub status: String,
    pub note: Option<String>,
    pub received_at: String,
}

/// A cart line priced from the catalogue.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CartLine {
    pub product: String,
    pub name: String,
    pub url: Option<String>,
    pub quantity: i64,
    pub unit_amount: i64,
    pub unit_display: String,
    pub amount: i64,
    pub amount_display: String,
    /// `false`: gone from the catalogue or not for sale any more.
    pub available: bool,
}

/// A priced cart.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Cart {
    pub lines: Vec<CartLine>,
    /// Number of items (sum of quantities).
    pub count: i64,
    pub total: i64,
    pub total_display: String,
    pub currency: String,
    /// Products that cannot be bought now (names).
    pub unavailable: Vec<String>,
}

impl Cart {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Price cart lines from the catalogue (the only place prices come from).
pub async fn price_cart(
    lines: &[(String, i64)],
    catalogue: &dyn Catalogue,
    currency: &str,
) -> Result<Cart> {
    let mut out = Vec::new();
    let mut unavailable = Vec::new();
    for (id, qty) in lines {
        let p = catalogue.product(id).await?;
        let (name, url, unit, available) = match &p {
            Some(p) => (
                p.name.clone(),
                p.url.clone(),
                p.price,
                p.available && p.currency == currency,
            ),
            None => (id.clone(), None, 0, false),
        };
        if !available {
            unavailable.push(name.clone());
        }
        let amount = money::line_amount(unit, *qty).context("a cart line is too large")?;
        out.push(CartLine {
            product: id.clone(),
            name,
            url,
            quantity: *qty,
            unit_amount: unit,
            unit_display: money::format(unit, currency),
            amount,
            amount_display: money::format(amount, currency),
            available,
        });
    }
    let total = money::total(out.iter().filter(|l| l.available).map(|l| l.amount))
        .context("the cart total is too large")?;
    Ok(Cart {
        count: out.iter().map(|l| l.quantity).sum(),
        lines: out,
        total,
        total_display: money::format(total, currency),
        currency: currency.to_string(),
        unavailable,
    })
}

/// Whether an event was new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Stored (row id) and queued for the job.
    New(i64),
    /// This provider event id was seen before: nothing done.
    Duplicate,
}

/// The outcome of applying one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub status: &'static str,
    pub note: Option<String>,
    pub order: Option<String>,
}

/// Orders, carts and events in the database. Cheap to clone.
#[derive(Clone, Debug)]
pub struct OrderStore {
    db: Db,
}

/// A public order id: `ord_` + 24 random URL-safe characters.
pub fn new_public_id() -> String {
    let t = engine_core::web::secret::token();
    format!("ord_{}", &t[..24])
}

impl OrderStore {
    pub fn new(db: Db) -> Self {
        OrderStore { db }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    // --- carts

    pub async fn cart_lines(&self, user: i64) -> Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i32)> = sql::CART_LINES.bind(user).fetch_all(&self.db).await?;
        Ok(rows.into_iter().map(|(p, q)| (p, q as i64)).collect())
    }

    /// Add `quantity` (1..=99) of a product; the line is capped at 99.
    /// `false` if the cart is full.
    pub async fn cart_add(&self, user: i64, product: &str, quantity: i64) -> Result<bool> {
        let q = quantity.clamp(1, MAX_QUANTITY) as i32;
        let lines = self.cart_lines(user).await?;
        if !lines.iter().any(|(p, _)| p == product) && lines.len() as i64 >= MAX_CART_LINES {
            return Ok(false);
        }
        sql::CART_ADD
            .bind(user)
            .bind(product)
            .bind(q)
            .execute(&self.db)
            .await?;
        Ok(true)
    }

    /// Set a line's quantity; 0 removes it.
    pub async fn cart_set(&self, user: i64, product: &str, quantity: i64) -> Result<()> {
        if quantity <= 0 {
            return self.cart_remove(user, product).await;
        }
        let lines = self.cart_lines(user).await?;
        if !lines.iter().any(|(p, _)| p == product) {
            // Only existing lines are changed this way.
            return Ok(());
        }
        sql::CART_SET
            .bind(user)
            .bind(product)
            .bind(quantity.min(MAX_QUANTITY) as i32)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    pub async fn cart_remove(&self, user: i64, product: &str) -> Result<()> {
        sql::CART_REMOVE
            .bind(user)
            .bind(product)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    pub async fn cart_clear(&self, user: i64) -> Result<()> {
        sql::CART_CLEAR.bind(user).execute(&self.db).await?;
        Ok(())
    }

    // --- orders

    async fn with_lines(&self, mut orders: Vec<Order>) -> Result<Vec<Order>> {
        if orders.is_empty() {
            return Ok(orders);
        }
        let ids: Vec<i64> = orders.iter().map(|o| o.id).collect();
        let rows: Vec<LineRow> = sql::LINES_OF.bind(ids).fetch_all(&self.db).await?;
        for r in rows {
            if let Some(o) = orders.iter_mut().find(|o| o.id == r.order_id) {
                let cur = o.currency.clone();
                o.lines.push(OrderLine {
                    unit_display: money::format(r.unit_amount, &cur),
                    amount_display: money::format(r.amount, &cur),
                    product: r.product,
                    name: r.name,
                    unit_amount: r.unit_amount,
                    quantity: r.quantity,
                    amount: r.amount,
                });
            }
        }
        Ok(orders)
    }

    async fn one(&self, row: Option<OrderRow>) -> Result<Option<Order>> {
        match row {
            Some(r) => Ok(self.with_lines(vec![r.into()]).await?.pop()),
            None => Ok(None),
        }
    }

    pub async fn order(&self, id: i64) -> Result<Option<Order>> {
        let r = sql::ORDER_BY_ID.bind(id).fetch_optional(&self.db).await?;
        self.one(r).await
    }

    pub async fn order_by_public(&self, public_id: &str) -> Result<Option<Order>> {
        let r = sql::ORDER_BY_PUBLIC
            .bind(public_id)
            .fetch_optional(&self.db)
            .await?;
        self.one(r).await
    }

    pub async fn order_by_key(&self, user: i64, key: &str) -> Result<Option<Order>> {
        let r = sql::ORDER_BY_KEY
            .bind(user)
            .bind(key)
            .fetch_optional(&self.db)
            .await?;
        self.one(r).await
    }

    /// A user's orders, newest first.
    pub async fn orders_of(&self, user: i64, limit: i64) -> Result<Vec<Order>> {
        let rows: Vec<OrderRow> = sql::ORDERS_OF_USER
            .bind(user)
            .bind(limit)
            .fetch_all(&self.db)
            .await?;
        self.with_lines(rows.into_iter().map(Into::into).collect())
            .await
    }

    /// Every order (admin), newest first; `status` filters.
    pub async fn page(&self, status: Option<&str>, limit: i64, offset: i64) -> Result<Vec<Order>> {
        let rows: Vec<OrderRow> = sql::ORDERS_PAGE
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await?;
        self.with_lines(rows.into_iter().map(Into::into).collect())
            .await
    }

    pub async fn counts(&self) -> Result<Vec<(String, i64)>> {
        Ok(sql::ORDER_COUNTS.q().fetch_all(&self.db).await?)
    }

    pub async fn user_email(&self, user: i64) -> Result<Option<String>> {
        Ok(sql::USER_EMAIL.bind(user).scalar_opt(&self.db).await?)
    }

    /// The events recorded for an order.
    pub async fn events_of(&self, order: i64) -> Result<Vec<StoredEvent>> {
        let rows: Vec<EventRow> = sql::EVENTS_OF_ORDER.bind(order).fetch_all(&self.db).await?;
        Ok(rows
            .into_iter()
            .map(|r| StoredEvent {
                amount_display: match (r.amount, &r.currency) {
                    (Some(a), Some(c)) => Some(money::format(a, c)),
                    _ => None,
                },
                id: r.id,
                provider: r.provider,
                event_id: r.event_id,
                kind: r.kind,
                provider_type: r.provider_type,
                status: r.status,
                note: r.note,
                received_at: r.received_at.iso(),
            })
            .collect())
    }

    /// Make a pending order from a priced cart. A second call with the
    /// same `key` returns the first order (`false`: not new).
    pub async fn create_order(
        &self,
        user: i64,
        key: &str,
        cart: &Cart,
        provider: &str,
    ) -> Result<(Order, bool)> {
        if let Some(o) = self.order_by_key(user, key).await? {
            return Ok((o, false));
        }
        if cart.is_empty() || !cart.unavailable.is_empty() {
            bail!("the cart cannot be bought as it is");
        }
        let mut tx = self.db.begin().await?;
        let id: Option<i64> = sql::ORDER_INSERT
            .bind(new_public_id())
            .bind(user)
            .bind(&cart.currency)
            .bind(cart.total)
            .bind(provider)
            .bind(key)
            .scalar_opt(&mut tx)
            .await?;
        let Some(id) = id else {
            // A concurrent post with the same key won.
            drop(tx);
            let o = self
                .order_by_key(user, key)
                .await?
                .context("order vanished")?;
            return Ok((o, false));
        };
        for (i, l) in cart.lines.iter().enumerate() {
            sql::LINE_INSERT
                .bind(id)
                .bind(i as i32)
                .bind(&l.product)
                .bind(&l.name)
                .bind(l.unit_amount)
                .bind(l.quantity as i32)
                .bind(l.amount)
                .execute(&mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok((self.order(id).await?.context("order vanished")?, true))
    }

    /// Record the provider's checkout session on a pending order.
    pub async fn set_session(&self, order: i64, session: &str, url: &str) -> Result<()> {
        sql::ORDER_SET_SESSION
            .bind(order)
            .bind(session)
            .bind(url)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// A pending order whose checkout could not be made: failed, with why.
    pub async fn fail_unstarted(&self, order: &Order, why: &str) -> Result<()> {
        sql::ORDER_TRANSITION
            .bind(order.id)
            .bind(order.version)
            .bind("failed")
            .bind(why)
            .bind(None::<String>)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    // --- events

    /// Store a verified event and queue the job that applies it, in one
    /// transaction. A provider event id seen before is a no-op.
    pub async fn record_event(
        &self,
        provider: &str,
        ev: &Event,
        payload: &serde_json::Value,
    ) -> Result<Recorded> {
        let mut tx = self.db.begin().await?;
        let id: Option<i64> = sql::EVENT_INSERT
            .bind(provider)
            .bind(&ev.id)
            .bind(ev.kind.as_str())
            .bind(&ev.provider_type)
            .bind(ev.order.as_deref())
            .bind(ev.session.as_deref())
            .bind(ev.payment.as_deref())
            .bind(ev.amount)
            .bind(ev.currency.as_deref())
            .bind(payload)
            .scalar_opt(&mut tx)
            .await?;
        let Some(id) = id else {
            return Ok(Recorded::Duplicate);
        };
        engine_jobs::Jobs::enqueue_in(
            &mut tx,
            crate::JOB_KIND,
            serde_json::json!({ "event": id }),
            &engine_jobs::Enqueue::default(),
        )
        .await?;
        tx.commit().await?;
        Ok(Recorded::New(id))
    }

    /// Apply a stored event to its order (the `payments.apply` job). Safe
    /// to run more than once: an event that is no longer `received` is
    /// left alone.
    pub async fn apply_event(&self, event: i64) -> Result<Applied> {
        let mut tx = self.db.begin().await?;
        let ev: Option<EventRow> = sql::EVENT_LOCK.bind(event).fetch_optional(&mut tx).await?;
        let Some(ev) = ev else {
            bail!("payment event {event} does not exist");
        };
        if ev.status != "received" {
            return Ok(Applied {
                status: "done before",
                note: ev.note,
                order: None,
            });
        }
        let kind = EventKind::parse(&ev.kind).context("unknown event kind")?;
        // Find the order: by its public id, else the session, else the
        // payment (refunds); always within the event's provider.
        let mut row: Option<OrderRow> = None;
        if let Some(r) = &ev.order_ref {
            row = sql::ORDER_BY_PUBLIC
                .bind(r)
                .fetch_optional(&mut tx)
                .await?
                .filter(|o: &OrderRow| o.provider == ev.provider);
        }
        if row.is_none()
            && let Some(s) = &ev.session
        {
            row = sql::ORDER_BY_SESSION
                .bind(&ev.provider)
                .bind(s)
                .fetch_optional(&mut tx)
                .await?;
        }
        if row.is_none()
            && let Some(p) = &ev.payment
        {
            row = sql::ORDER_BY_PAYMENT
                .bind(&ev.provider)
                .bind(p)
                .fetch_optional(&mut tx)
                .await?;
        }
        let Some(o) = row else {
            // Kept, not refused: a later record type (donations arrive by
            // webhook with no order made first) can claim it.
            let note = "no order matches this event".to_string();
            sql::EVENT_DONE
                .bind(event)
                .bind("unmatched")
                .bind(&note)
                .bind(None::<i64>)
                .execute(&mut tx)
                .await?;
            tx.commit().await?;
            return Ok(Applied {
                status: "unmatched",
                note: Some(note),
                order: None,
            });
        };
        // A session the order did not start is not this order's.
        if let (Some(s), Some(mine)) = (&ev.session, &o.provider_session)
            && s != mine
        {
            let note = "the event's checkout session is not the order's".to_string();
            sql::EVENT_DONE
                .bind(event)
                .bind("refused")
                .bind(&note)
                .bind(o.id)
                .execute(&mut tx)
                .await?;
            tx.commit().await?;
            return Ok(Applied {
                status: "refused",
                note: Some(note),
                order: Some(o.public_id),
            });
        }
        let d = decide(
            &o.status,
            o.total,
            &o.currency,
            kind,
            ev.amount,
            ev.currency.as_deref(),
        );
        let (ev_status, to, note): (&'static str, Option<&'static str>, Option<String>) = match d {
            Decision::To(s) => ("applied", Some(s), None),
            Decision::Same => (
                "ignored",
                None,
                Some(format!("the order is already {}", o.status)),
            ),
            Decision::Flag(why) => ("flagged", Some("flagged"), Some(why)),
            Decision::Refuse(why) => ("refused", None, Some(why)),
        };
        if let Some(to) = to {
            let n = sql::ORDER_TRANSITION
                .bind(o.id)
                .bind(o.version)
                .bind(to)
                .bind(note.as_deref())
                .bind(ev.payment.as_deref())
                .execute(&mut tx)
                .await?;
            if n != 1 {
                // Changed since we read it: roll back, the job retries.
                bail!(
                    "order {} changed while event {event} was applied; trying again",
                    o.public_id
                );
            }
            if to == "paid"
                && let Some(u) = o.user_id
            {
                sql::CART_CLEAR.bind(u).execute(&mut tx).await?;
            }
        } else if ev_status == "refused" {
            sql::ORDER_NOTE
                .bind(o.id)
                .bind(o.version)
                .bind(note.as_deref())
                .execute(&mut tx)
                .await?;
        }
        sql::EVENT_DONE
            .bind(event)
            .bind(ev_status)
            .bind(note.as_deref())
            .bind(o.id)
            .execute(&mut tx)
            .await?;
        tx.commit().await?;
        if ev_status == "refused" || ev_status == "flagged" {
            eprintln!(
                "payments: {} event {} ({}) for order {}: {ev_status}: {}",
                ev.provider,
                ev.event_id,
                ev.kind,
                o.public_id,
                note.as_deref().unwrap_or("")
            );
        }
        Ok(Applied {
            status: ev_status,
            note,
            order: Some(o.public_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EventKind::*;

    fn d(status: &str, kind: EventKind, amount: Option<i64>, cur: Option<&str>) -> Decision {
        decide(status, 2500, "GBP", kind, amount, cur)
    }

    #[test]
    fn the_happy_paths() {
        assert_eq!(
            d("pending", Paid, Some(2500), Some("GBP")),
            Decision::To("paid")
        );
        assert_eq!(
            d("paid", Refunded, Some(2500), Some("GBP")),
            Decision::To("refunded")
        );
        for (k, to) in [
            (Failed, "failed"),
            (Expired, "expired"),
            (Cancelled, "cancelled"),
        ] {
            assert_eq!(d("pending", k, None, None), Decision::To(to));
        }
    }

    #[test]
    fn bank_debits_settle_or_come_back() {
        assert_eq!(
            d("pending", Processing, None, None),
            Decision::To("processing")
        );
        assert_eq!(
            d("processing", Paid, Some(2500), Some("GBP")),
            Decision::To("paid")
        );
        assert_eq!(d("processing", Failed, None, None), Decision::To("failed"));
        assert_eq!(
            d("processing", Returned, None, None),
            Decision::To("returned")
        );
        assert_eq!(d("paid", Returned, None, None), Decision::To("returned"));
        for st in ["pending", "failed", "refunded", "expired"] {
            assert!(
                matches!(d(st, Returned, None, None), Decision::Refuse(_)),
                "{st}"
            );
        }
        for k in [Expired, Cancelled] {
            assert!(matches!(
                d("processing", k, None, None),
                Decision::Refuse(_)
            ));
        }
        assert!(matches!(
            d("paid", Processing, None, None),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn money_must_match() {
        assert!(matches!(
            d("pending", Paid, Some(2499), Some("GBP")),
            Decision::Flag(m) if m.contains("£24.99") && m.contains("£25.00")
        ));
        assert!(matches!(
            d("pending", Paid, Some(2500), Some("USD")),
            Decision::Flag(_)
        ));
        assert!(matches!(d("pending", Paid, None, None), Decision::Flag(_)));
        assert!(matches!(
            d("paid", Refunded, Some(100), Some("GBP")),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn repeats_are_no_ops() {
        assert_eq!(d("paid", Paid, Some(2500), Some("GBP")), Decision::Same);
        assert_eq!(d("failed", Failed, None, None), Decision::Same);
        assert_eq!(
            d("refunded", Refunded, Some(2500), Some("GBP")),
            Decision::Same
        );
    }

    #[test]
    fn illegal_moves_are_refused() {
        for (status, kind) in [
            ("cancelled", Paid),
            ("expired", Paid),
            ("failed", Paid),
            ("refunded", Paid),
            ("pending", Refunded),
            ("failed", Refunded),
            ("paid", Failed),
            ("paid", Expired),
            ("paid", Cancelled),
            ("refunded", Cancelled),
            ("flagged", Paid),
            ("flagged", Refunded),
            ("flagged", Failed),
        ] {
            assert!(
                matches!(
                    d(status, kind, Some(2500), Some("GBP")),
                    Decision::Refuse(_)
                ),
                "{status} + {kind:?}"
            );
        }
    }

    #[test]
    fn public_ids_fit_the_schema() {
        let id = new_public_id();
        assert!(id.starts_with("ord_") && id.len() == 28, "{id}");
        assert_ne!(id, new_public_id());
    }
}
