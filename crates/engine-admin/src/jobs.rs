//! The jobs screen (`jobs` feature, Postgres sites): `/admin/jobs/`.
//!
//! Counts per status, the queue filtered by status with paging, a job's
//! details (attempts, last error, times) and, for dead or failed-and-waiting
//! jobs, *retry* (a fresh set of attempts) and *delete*. Payloads are not
//! shown: a queued email holds a whole message, reset links included.
//! Administrators only.

use crate::Refusal;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router};
use engine_auth::Session;
use engine_auth::web::{FormData, see_other};
use minijinja::context;
use serde::Deserialize;

use crate::{Admin, PER_PAGE, Pager};

pub(crate) const STATUSES: [&str; 4] = ["queued", "running", "done", "dead"];

#[derive(Deserialize)]
pub(crate) struct ListQ {
    status: Option<String>,
    page: Option<i64>,
    notice: Option<String>,
}

fn allowed(a: &Admin, s: &Session) -> Result<(), Refusal> {
    if s.access.is_admin() {
        Ok(())
    } else {
        Err(Box::new(a.forbidden(s, "see the job queue")))
    }
}

async fn guard(a: &Admin, headers: &HeaderMap, uri: &Uri) -> Result<Session, Refusal> {
    let s = a.require(headers, uri).await?;
    allowed(a, &s)?;
    Ok(s)
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
    let Some(jobs) = a.jobs() else {
        return a.not_found(&s);
    };
    // Failed jobs first: that is what people come here for.
    let status = q
        .status
        .as_deref()
        .filter(|s| STATUSES.contains(s) || *s == "all")
        .unwrap_or("dead");
    let filter = (status != "all").then_some(status);
    let counts = match jobs.counts().await {
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
    let total = match filter {
        Some(f) => count(f),
        None => counts.iter().map(|(_, n)| n).sum(),
    };
    let pager = Pager::new(q.page.unwrap_or(1), total, |p| {
        format!("/admin/jobs/?status={status}&page={p}")
    });
    let list = match jobs.page(filter, PER_PAGE, pager.offset()).await {
        Ok(l) => l,
        Err(e) => return a.error(&s, e),
    };
    let list: Vec<_> = list
        .into_iter()
        .map(|j| {
            let short = j.last_error.as_deref().map(|e| {
                let s: String = e.chars().take(120).collect();
                if s.len() < e.len() {
                    format!("{s}…")
                } else {
                    s
                }
            });
            context! {
                id => j.id, kind => j.kind, status => j.status, attempts => j.attempts,
                max_attempts => j.max_attempts, last_error => short,
            }
        })
        .collect();
    let tabs: Vec<_> = STATUSES
        .iter()
        .map(|st| context! { status => st, n => count(st) })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/jobs.html",
        a.base(&s, "Jobs"),
        context! { jobs => list, tabs, status, pager, notice => q.notice },
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
    let Some(jobs) = a.jobs() else {
        return a.not_found(&s);
    };
    match jobs.job(id).await {
        Ok(Some(j)) => a.page(
            StatusCode::OK,
            "admin/job.html",
            a.base(&s, &format!("Job {id}")),
            context! { job => j },
        ),
        Ok(None) => a.not_found(&s),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn retry(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = allowed(&a, &s) {
        return *r;
    }
    let Some(jobs) = a.jobs() else {
        return a.not_found(&s);
    };
    match jobs.retry(id).await {
        Ok(true) => see_other("/admin/jobs/?status=queued&notice=job-retried"),
        Ok(false) => a.message(
            &s,
            StatusCode::CONFLICT,
            "Not retried",
            "Only dead jobs can be retried; this one is queued, running or done.",
        ),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn delete(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = allowed(&a, &s) {
        return *r;
    }
    let Some(jobs) = a.jobs() else {
        return a.not_found(&s);
    };
    match jobs.delete(id).await {
        Ok(true) => see_other("/admin/jobs/?notice=job-deleted"),
        Ok(false) => a.message(
            &s,
            StatusCode::CONFLICT,
            "Not deleted",
            "A running job cannot be deleted; wait until it finishes or fails.",
        ),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) fn routes(r: Router<Admin>) -> Router<Admin> {
    r.route("/admin/jobs/", get(list))
        .route("/admin/jobs/{id}/", get(item))
        .route("/admin/jobs/{id}/retry/", post(retry))
        .route("/admin/jobs/{id}/delete/", post(delete))
}
