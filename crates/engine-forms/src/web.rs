//! `POST /_engine/forms/<name>/`: CSRF, spam guard, validation, storage.
//!
//! A refused or invalid post shows the page the form is on again
//! (`_page`, rendered by the engine's `request` mode with the visitor's
//! values and the errors) with status 422 (429 when rate limited). A good
//! one is stored and answered with a redirect: to the form's `redirect`,
//! or back to the page with `?sent=<name>`, where the form shows its
//! success message. Multi-step forms post each step here; `Next` checks
//! the step's fields and shows the next step.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use engine_core::web::{AnonCsrf, Client, FormData, safe_path, same_origin, secret};
use minijinja::{Value, context};

use crate::validate::{self, Errors};
use crate::{FormDef, Forms, Submission};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A signed-in visitor, as far as forms care.
#[derive(Debug, Clone)]
pub struct Visitor {
    pub user_id: i64,
    pub csrf: String,
    pub email: String,
    pub name: String,
}

/// What the forms routes need from the rest of the engine (implemented by
/// the CLI, which knows which features are compiled in).
pub trait Hooks: Send + Sync {
    /// The signed-in visitor, if any (`auth` feature).
    fn visitor<'a>(&'a self, headers: &'a HeaderMap) -> BoxFuture<'a, Option<Visitor>>;

    /// Render the site page at `path` per request, with `form_state` in
    /// its context. `None` if no page renders there.
    fn page<'a>(
        &'a self,
        path: &'a str,
        headers: &'a HeaderMap,
        form_state: Value,
    ) -> BoxFuture<'a, Option<String>>;

    /// After a submission was accepted (receipts, notifications). Errors
    /// are logged; the visitor still gets the success answer.
    fn submitted<'a>(
        &'a self,
        def: &'a FormDef,
        sub: &'a Submission,
        visitor: Option<&'a Visitor>,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}

#[derive(Clone)]
struct Ctx {
    forms: Forms,
    hooks: Arc<dyn Hooks>,
    csrf: AnonCsrf,
    behind_proxy: bool,
}

/// The routes. `csrf` and `behind_proxy` follow `[auth]` (cookie_secure,
/// behind_proxy) when accounts exist.
pub fn router(forms: Forms, hooks: Arc<dyn Hooks>, csrf: AnonCsrf, behind_proxy: bool) -> Router {
    Router::new()
        .route("/_engine/forms/{name}/", post(submit))
        .with_state(Ctx {
            forms,
            hooks,
            csrf,
            behind_proxy,
        })
}

fn html(status: StatusCode, body: String) -> Response {
    let mut r = (status, body).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}

fn plain(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, format!("{msg}\n")).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn see_other(to: &str) -> Response {
    let mut r = Redirect::to(to).into_response();
    *r.status_mut() = StatusCode::SEE_OTHER;
    r
}

struct Shown<'a> {
    form: &'a str,
    page: &'a str,
    values: std::collections::BTreeMap<String, String>,
    errors: Errors,
    error: Option<&'a str>,
    step: usize,
    stamp: String,
}

/// Show the form's page again with values and errors.
async fn again(ctx: &Ctx, headers: &HeaderMap, status: StatusCode, s: Shown<'_>) -> Response {
    let state = context! {
        form => s.form, values => s.values, errors => s.errors, error => s.error,
        step => s.step, stamp => s.stamp,
    };
    match ctx.hooks.page(s.page, headers, state).await {
        Some(body) => html(status, body),
        None => {
            // The page is gone or not a request page: say what is wrong.
            let mut lines = vec![s.error.unwrap_or("Please check the form.").to_string()];
            lines.extend(s.errors.iter().map(|(k, v)| format!("{k} {v}")));
            plain(status, &lines.join("\n"))
        }
    }
}

async fn submit(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let found = match ctx.forms.get(&name).await {
        Ok(Some(f)) => f,
        Ok(None) => return plain(StatusCode::NOT_FOUND, "No such form."),
        Err(e) => {
            eprintln!("forms: {e:#}");
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The form could not be read.",
            );
        }
    };
    let def = found.def;
    let Some(store) = ctx.forms.store().cloned() else {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "Forms need the site's database.",
        );
    };

    // CSRF: the session's token when signed in, else the cookie's.
    let visitor = ctx.hooks.visitor(&headers).await;
    let csrf_ok = match &visitor {
        Some(v) => {
            same_origin(&headers) && form.get("_csrf").is_some_and(|t| secret::same(t, &v.csrf))
        }
        None => ctx.csrf.check(&headers, &form),
    };
    if !csrf_ok {
        return plain(
            StatusCode::FORBIDDEN,
            "This form was not accepted: its security token is missing or out of date, or it \
             was sent from another site. Go back, reload the page and try again.",
        );
    }

    let page = safe_path(form.get("_page"))
        .map(|p| p.split('?').next().unwrap_or("/").to_string())
        .unwrap_or_else(|| "/".into());
    let ip = client.ip(ctx.behind_proxy);
    let key = ip.clone().unwrap_or_else(|| "unknown".into());
    let stamp_in = form.get("_t").unwrap_or("").to_string();
    let raw = validate::raw_values(&def, &form);
    let steps = def.step_count();
    let step = form
        .get("_step")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(steps - 1);
    let go = form.get("_go").unwrap_or("submit");
    let shown = |values, errors, error, step, stamp: String| Shown {
        form: &name,
        page: &page,
        values,
        errors,
        error,
        step,
        stamp,
    };

    // Multi-step navigation (no storing, no spam checks yet).
    if steps > 1 && go == "back" {
        let prev = step.saturating_sub(1);
        return again(
            &ctx,
            &headers,
            StatusCode::OK,
            shown(raw, Errors::new(), None, prev, stamp_in),
        )
        .await;
    }
    if steps > 1 && go == "next" && step + 1 < steps {
        let (_, errors) = validate::validate(&def.step_fields(step), &form);
        let (status, next) = if errors.is_empty() {
            (StatusCode::OK, step + 1)
        } else {
            (StatusCode::UNPROCESSABLE_ENTITY, step)
        };
        return again(
            &ctx,
            &headers,
            status,
            shown(raw, errors, None, next, stamp_in),
        )
        .await;
    }

    // Spam guard 1: the honeypot. Look successful, store nothing.
    if form
        .get(def.honeypot())
        .is_some_and(|v| !v.trim().is_empty())
    {
        let _ = store
            .record_event(&name, "spam", &key, Some("honeypot"))
            .await;
        eprintln!("forms: `{name}` post from {key} refused (honeypot)");
        return success_redirect(&def, &page);
    }
    // Spam guard 2: the time trap.
    let fresh_stamp = ctx.forms.stamp(&name);
    let age = ctx.forms.stamp_age(&name, &stamp_in);
    let too_fast = age.is_none_or(|a| a < def.spam.min_seconds as f64);
    let too_old = age.is_some_and(|a| a > (def.spam.max_age_hours * 3600) as f64);
    if too_fast || too_old {
        let reason = if age.is_none() {
            "stamp missing or forged"
        } else if too_old {
            "form too old"
        } else {
            "sent too fast"
        };
        let _ = store.record_event(&name, "spam", &key, Some(reason)).await;
        eprintln!("forms: `{name}` post from {key} refused ({reason})");
        let msg = if too_old {
            "This form was open for a long time. Please check it and send it again."
        } else {
            "That was very fast. Please check the form and send it again."
        };
        return again(
            &ctx,
            &headers,
            StatusCode::UNPROCESSABLE_ENTITY,
            shown(raw, Errors::new(), Some(msg), steps - 1, fresh_stamp),
        )
        .await;
    }
    // Spam guard 3: the rate limit.
    match store.recent_accepted(&name, &key, 60).await {
        Ok(n) if n >= def.spam.max_per_hour => {
            let _ = store
                .record_event(&name, "spam", &key, Some("rate limit"))
                .await;
            eprintln!("forms: `{name}` post from {key} refused (rate limit)");
            return again(
                &ctx,
                &headers,
                StatusCode::TOO_MANY_REQUESTS,
                shown(
                    raw,
                    Errors::new(),
                    Some("Too many submissions from your network in the last hour. Please try again later."),
                    steps - 1,
                    stamp_in,
                ),
            )
            .await;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("forms: {e:#}");
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The form could not be checked.",
            );
        }
    }

    // Validation of every field (steps included: hidden values can be
    // edited by anyone).
    let (values, errors) = validate::validate_all(&def, &form);
    if !errors.is_empty() {
        let first_bad = (0..steps)
            .find(|i| {
                def.step_fields(*i)
                    .iter()
                    .any(|f| errors.contains_key(&f.name))
            })
            .unwrap_or(0);
        return again(
            &ctx,
            &headers,
            StatusCode::UNPROCESSABLE_ENTITY,
            shown(raw, errors, None, first_bad, stamp_in),
        )
        .await;
    }

    let data = serde_json::Value::Object(values.into_iter().collect());
    let unique = def.unique.as_ref().and_then(|u| data.get(u)).map(|v| {
        v.as_str()
            .map(str::to_lowercase)
            .unwrap_or_else(|| v.to_string())
    });
    let id = if def.store {
        match store
            .insert(
                &name,
                &data,
                unique.as_deref(),
                visitor.as_ref().map(|v| v.user_id),
                ip.as_deref(),
                &client.user_agent,
            )
            .await
        {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                // Already stored (e.g. already subscribed): same answer.
                let _ = store
                    .record_event(&name, "accepted", &key, Some("duplicate"))
                    .await;
                return success_redirect(&def, &page);
            }
            Err(e) => {
                eprintln!("forms: storing `{name}`: {e:#}");
                return plain(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The form could not be saved.",
                );
            }
        }
    } else {
        None
    };
    let _ = store.record_event(&name, "accepted", &key, None).await;
    let sub = Submission {
        id: id.unwrap_or(0),
        form: name.clone(),
        data,
        user_id: visitor.as_ref().map(|v| v.user_id),
        ip,
        created_at: now_utc(),
    };
    if let Err(e) = ctx.hooks.submitted(&def, &sub, visitor.as_ref()).await {
        eprintln!("forms: after `{name}` submission {}: {e:#}", sub.id);
    }
    success_redirect(&def, &page)
}

fn success_redirect(def: &FormDef, page: &str) -> Response {
    match &def.redirect {
        Some(r) => see_other(r),
        None => see_other(&format!("{page}?sent={}", def.name)),
    }
}

/// `2026-11-26 10:00 UTC`.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_dates() {
        let s = super::now_utc();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.starts_with("20") && s.ends_with(" UTC"));
    }
}
