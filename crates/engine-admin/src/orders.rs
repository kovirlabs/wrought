//! The orders screen (`payments` feature): `/admin/orders/`. Read-only:
//! counts per status, the orders filtered by status with paging (flagged
//! ones first: they need a person), and one order with its lines, buyer
//! and the provider events that moved it (with the reason when an event
//! was refused or flagged the order). Administrators only.

use crate::Refusal;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use engine_auth::Session;
use minijinja::context;
use serde::Deserialize;

use crate::{Admin, PER_PAGE, Pager};

const STATUSES: [&str; 9] = engine_payments::orders::STATUSES;

#[derive(Deserialize)]
pub(crate) struct ListQ {
    status: Option<String>,
    page: Option<i64>,
}

async fn guard(a: &Admin, headers: &HeaderMap, uri: &Uri) -> Result<Session, Refusal> {
    let s = a.require(headers, uri).await?;
    if s.access.is_admin() {
        Ok(s)
    } else {
        Err(Box::new(a.forbidden(&s, "see orders")))
    }
}

pub(crate) async fn list(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Query(q): Query<ListQ>,
) -> Response {
    let s = match guard(&a, &headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(orders) = a.orders() else {
        return a.not_found(&s);
    };
    let counts = match orders.counts().await {
        Ok(c) => c,
        Err(e) => return a.error(&s, e),
    };
    let count = |st: &str| {
        counts
            .iter()
            .find(|(k, _)| k == st)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    let default = if count("flagged") > 0 {
        "flagged"
    } else {
        "all"
    };
    let status = q
        .status
        .as_deref()
        .filter(|s| STATUSES.contains(s) || *s == "all")
        .unwrap_or(default);
    let filter = (status != "all").then_some(status);
    let total = match filter {
        Some(f) => count(f),
        None => counts.iter().map(|(_, n)| n).sum(),
    };
    let pager = Pager::new(q.page.unwrap_or(1), total, |p| {
        format!("/admin/orders/?status={status}&page={p}")
    });
    let list = match orders.page(filter, PER_PAGE, pager.offset()).await {
        Ok(l) => l,
        Err(e) => return a.error(&s, e),
    };
    let tabs: Vec<_> = STATUSES
        .iter()
        .map(|st| context! { status => st, n => count(st) })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/orders.html",
        a.base(&s, "Orders"),
        context! { orders => list, tabs, status, pager },
    )
}

pub(crate) async fn item(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
) -> Response {
    let s = match guard(&a, &headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(orders) = a.orders() else {
        return a.not_found(&s);
    };
    let o = match orders.order(id).await {
        Ok(Some(o)) => o,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let buyer = match o.user_id {
        Some(u) => orders.user_email(u).await.ok().flatten(),
        None => None,
    };
    let events = match orders.events_of(o.id).await {
        Ok(e) => e,
        Err(e) => return a.error(&s, e),
    };
    a.page(
        StatusCode::OK,
        "admin/order.html",
        a.base(&s, &format!("Order {}", o.public_id)),
        context! { order => o, buyer, events },
    )
}

pub(crate) fn routes(r: Router<Admin>) -> Router<Admin> {
    r.route("/admin/orders/", get(list))
        .route("/admin/orders/{id}/", get(item))
}
