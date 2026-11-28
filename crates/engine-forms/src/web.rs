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
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use engine_core::web::{AnonCsrf, Client, FormData, safe_path, same_origin, secret};
use minijinja::{Value, context};

use crate::spool::{Spool, SpoolError, Spooled};
use crate::validate::{self, Errors};
use crate::{FormDef, FormStore, Forms, Submission};

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
    /// are logged; the visitor still gets the success answer. `tx`: a
    /// spooled post being replayed, whose effects must commit with it (see
    /// [`replay`]).
    fn submitted<'a>(
        &'a self,
        def: &'a FormDef,
        sub: &'a Submission,
        visitor: Option<&'a Visitor>,
        tx: Option<&'a mut engine_db::Tx>,
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
    #[cfg(feature = "media")]
    let limit = match forms.media() {
        Some(m) => engine_media::upload::body_limit(m.config().max_bytes(), MAX_FILES),
        None => 2 * 1024 * 1024,
    };
    #[cfg(not(feature = "media"))]
    let limit = 2 * 1024 * 1024;
    Router::new()
        .route(
            "/_engine/forms/{name}/",
            post(submit).layer(axum::extract::DefaultBodyLimit::max(limit)),
        )
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

/// Most file fields in one form.
pub const MAX_FILES: usize = 5;

/// An uploaded file waiting for the submission to be accepted.
#[allow(dead_code)]
struct Pending {
    field: String,
    filename: String,
    bytes: Vec<u8>,
}

/// Read the post: url-encoded, or `multipart/form-data` for forms with file
/// fields. Text posted under a file field's name is dropped, so the
/// validation markers only ever come from real uploads.
async fn read_post(
    ctx: &Ctx,
    req: Request,
    def: &FormDef,
) -> Result<(FormData, Vec<Pending>), Box<Response>> {
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"));
    let file_fields: Vec<&str> = def
        .fields
        .iter()
        .filter(|f| f.kind == crate::def::Kind::File)
        .map(|f| f.name.as_str())
        .collect();
    if !is_multipart {
        let axum::Form(mut form) = axum::Form::<FormData>::from_request(req, &())
            .await
            .map_err(|e| Box::new(e.into_response()))?;
        form.0.retain(|(k, _)| !file_fields.contains(&k.as_str()));
        return Ok((form, Vec::new()));
    }
    #[cfg(feature = "media")]
    {
        let Some(media) = ctx.forms.media() else {
            return Err(Box::new(plain(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "This site does not take file uploads.",
            )));
        };
        let mp = axum::extract::Multipart::from_request(req, &())
            .await
            .map_err(|e| Box::new(e.into_response()))?;
        let parts =
            match engine_media::upload::read_multipart(mp, media.config().max_bytes(), MAX_FILES)
                .await
            {
                Ok(p) => p,
                Err(e @ engine_media::MediaError::TooLarge { .. }) => {
                    return Err(Box::new(plain(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        &format!("Not sent: {e}."),
                    )));
                }
                Err(engine_media::MediaError::Other(e)) => {
                    eprintln!("forms: reading a `{}` upload: {e:#}", def.name);
                    return Err(Box::new(plain(
                        StatusCode::BAD_REQUEST,
                        "Not sent: the upload could not be read.",
                    )));
                }
                Err(e) => {
                    return Err(Box::new(plain(
                        StatusCode::BAD_REQUEST,
                        &format!("Not sent: {e}."),
                    )));
                }
            };
        let mut form = FormData(parts.fields);
        form.0.retain(|(k, _)| !file_fields.contains(&k.as_str()));
        let mut pending = Vec::new();
        for f in parts.files {
            if !file_fields.contains(&f.field.as_str())
                || pending.iter().any(|p: &Pending| p.field == f.field)
            {
                continue;
            }
            let family = engine_media::sniff::sniff(&f.bytes).map(|k| k.family());
            let why = family.as_ref().err().map(|e| match e {
                engine_media::sniff::Refused::Markup(w) => w.to_string(),
                engine_media::sniff::Refused::Empty => "empty".into(),
                engine_media::sniff::Refused::Unknown => "unknown type".into(),
            });
            let marker = validate::file_marker(
                family
                    .as_ref()
                    .map(|f| *f)
                    .map_err(|_| why.as_deref().unwrap_or("")),
                f.bytes.len(),
            );
            form.0.push((f.field.clone(), marker));
            pending.push(Pending {
                field: f.field,
                filename: f.filename,
                bytes: f.bytes,
            });
        }
        Ok((form, pending))
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = ctx;
        Err(Box::new(plain(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "This site does not take file uploads.",
        )))
    }
}

async fn submit(
    State(ctx): State<Ctx>,
    Path(name): Path<String>,
    client: Client,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let found = match ctx.forms.get(&name).await {
        Ok(Some(f)) => f,
        // Before the database is reached, a name the files do not have may
        // be an admin-made form (defined in the database).
        Ok(None) if ctx.forms.is_offline() => return unavailable(30),
        Ok(None) => return plain(StatusCode::NOT_FOUND, "No such form."),
        Err(e) if engine_db::health::is_unavailable_any(&e) => return unavailable(30),
        Err(e) => return failed(&e, "forms", "The form could not be read."),
    };
    let def = found.def;
    let store = ctx.forms.store().cloned();
    if store.is_none() && !ctx.forms.is_offline() {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "Forms need the site's database.",
        );
    }
    // Unavailable (asleep, down, or not reached yet during the start): no
    // query is tried; the post goes to the spool.
    let db_down = store.as_ref().is_none_or(|s| s.db().health().is_down());
    // Files are stored as media in the database: an upload cannot wait in
    // the spool.
    if ctx.forms.is_offline()
        && req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/"))
    {
        return unavailable(30);
    }
    let (form, pending) = match read_post(&ctx, req, &def).await {
        Ok(x) => x,
        Err(r) => return *r,
    };

    // CSRF: the session's token when signed in, else the cookie's.
    let visitor = ctx.hooks.visitor(&headers).await;
    let csrf_ok = match &visitor {
        Some(v) => {
            same_origin(&headers) && form.get("_csrf").is_some_and(|t| secret::same(t, &v.csrf))
        }
        None => ctx.csrf.check(&headers, &form),
    };
    // Before the database is reached a session cannot be read: a signed-in
    // visitor's token cannot be checked yet.
    if !csrf_ok && ctx.forms.is_offline() {
        return unavailable(30);
    }
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
        if let Some(store) = store.as_ref().filter(|_| !db_down) {
            let _ = store
                .record_event(&name, "spam", &key, Some("honeypot"))
                .await;
        }
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
        if let Some(store) = store.as_ref().filter(|_| !db_down) {
            let _ = store.record_event(&name, "spam", &key, Some(reason)).await;
        }
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
    // Spam guard 3: the rate limit (while the database is unavailable,
    // counted in memory by `to_spool`, and again at replay).
    let counted = match store.as_ref().filter(|_| !db_down) {
        Some(store) => Some((store, store.recent_accepted(&name, &key, 60).await)),
        None => None,
    };
    match counted {
        None => {}
        Some((store, Ok(n))) if n >= def.spam.max_per_hour => {
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
        Some((_, Ok(_))) => {}
        // The database is unavailable: the post goes to the spool, where
        // the per-hour limit is counted in memory instead (`to_spool`) and
        // again in the database at replay.
        Some((_, Err(e)))
            if ctx.forms.spool().is_some() && engine_db::health::is_unavailable_any(&e) => {}
        Some((_, Err(e))) => return failed(&e, "forms", "The form could not be checked."),
    }
    // Files are stored as media in the database: none can be taken while
    // it is unavailable.
    #[cfg(feature = "media")]
    if !pending.is_empty() && db_down {
        return unavailable(30);
    }

    // Validation of every field (steps included: hidden values can be
    // edited by anyone).
    #[cfg(feature = "media")]
    let max_mb = ctx
        .forms
        .media()
        .map(|m| m.config().max_mb)
        .unwrap_or(validate::DEFAULT_MAX_MB);
    #[cfg(not(feature = "media"))]
    let max_mb = validate::DEFAULT_MAX_MB;
    #[allow(unused_mut)]
    let (mut values, mut errors) =
        validate::with_max_mb(max_mb, || validate::validate_all(&def, &form));
    // Store the files (private) only once everything else is valid.
    #[cfg(feature = "media")]
    if errors.is_empty() && !pending.is_empty() {
        let Some(media) = ctx.forms.media() else {
            return plain(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "This site does not take file uploads.",
            );
        };
        let mut stored: Vec<i64> = Vec::new();
        for p in &pending {
            match media
                .upload(engine_media::Upload {
                    bytes: &p.bytes,
                    filename: &p.filename,
                    alt: "",
                    private: true,
                    uploaded_by: visitor.as_ref().map(|v| v.user_id),
                })
                .await
            {
                Ok(m) => {
                    stored.push(m.id);
                    values.insert(
                        p.field.clone(),
                        serde_json::json!({
                            "media_id": m.id, "name": m.original_name, "type": m.mime,
                            "bytes": m.bytes,
                        }),
                    );
                }
                Err(engine_media::MediaError::Other(e)) => {
                    return failed(
                        &e,
                        &format!("forms: storing an upload of `{name}`"),
                        "The file could not be saved.",
                    );
                }
                Err(e) => {
                    errors.insert(p.field.clone(), format!("was not accepted: {e}"));
                }
            }
        }
        // Do not keep half a submission's files.
        if !errors.is_empty() {
            for id in stored {
                let _ = media.delete(id, 1).await;
            }
        }
    }
    #[cfg(not(feature = "media"))]
    let _ = &pending;
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
    // The post's idempotency key (not `key`, the client's address).
    let idem = crate::spool::new_key();
    let spooled = || Spooled {
        key: idem.clone(),
        form: name.clone(),
        data: data.clone(),
        unique: unique.clone(),
        user_id: visitor.as_ref().map(|v| v.user_id),
        visitor_name: visitor.as_ref().map(|v| v.name.clone()),
        visitor_email: visitor.as_ref().map(|v| v.email.clone()),
        ip: ip.clone(),
        user_agent: client.user_agent.clone(),
        created_at: now_utc(),
        store: def.store,
    };
    // While the database is unavailable, and while older posts still wait
    // in the spool (to keep the order), the post goes to the spool.
    if let Some(spool) = ctx.forms.spool()
        && (!spool.is_empty() || db_down)
    {
        return to_spool(&ctx.forms, spool, spooled(), &def, &page, &key).await;
    }
    let Some(store) = store else {
        return unavailable(30);
    };
    let id = if def.store {
        match FormStore::insert_keyed(
            store.db(),
            &idem,
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
            Err(e) if engine_db::health::is_unavailable_any(&e) && ctx.forms.spool().is_some() => {
                let spool = ctx.forms.spool().expect("checked");
                return to_spool(&ctx.forms, spool, spooled(), &def, &page, &key).await;
            }
            Err(e) => {
                return failed(
                    &e,
                    &format!("forms: storing `{name}`"),
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
    if let Err(e) = ctx
        .hooks
        .submitted(&def, &sub, visitor.as_ref(), None)
        .await
    {
        eprintln!("forms: after `{name}` submission {}: {e:#}", sub.id);
    }
    success_redirect(&def, &page)
}

/// A failure: `503` if its cause is that the database is unavailable (by
/// the error's chain), else `500` with `message`. Logged either way.
fn failed(e: &anyhow::Error, what: &str, message: &'static str) -> Response {
    eprintln!("{what}: {e:#}");
    if engine_db::health::is_unavailable_any(e) {
        return unavailable(30);
    }
    plain(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// `503` with `Retry-After` (seconds).
fn unavailable(retry_after: u32) -> Response {
    let mut r = plain(
        StatusCode::SERVICE_UNAVAILABLE,
        "This form cannot be taken right now. Please try again in a few minutes.",
    );
    if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// Keep a post in the spool (on disk, `fsync`ed) and answer as if it had
/// been stored; the replay writes it to the database when it is back.
///
/// The spool is bounded three ways: the form's `max_per_hour` per client,
/// counted in memory while the database cannot count it
/// ([`crate::outage`]); a client's share of the spool's bytes; and the
/// spool's total.
async fn to_spool(
    forms: &Forms,
    spool: &Arc<Spool>,
    post: Spooled,
    def: &FormDef,
    page: &str,
    client: &str,
) -> Response {
    let form = post.form.clone();
    if !forms
        .outage_limiter()
        .allow(&form, client, def.spam.max_per_hour)
    {
        eprintln!("forms: `{form}` post from {client} refused (rate limit, database unavailable)");
        let mut r = plain(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many submissions from your network in the last hour. Please try again later.",
        );
        r.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("3600"));
        return r;
    }
    let s = spool.clone();
    let c = client.to_string();
    match tokio::task::spawn_blocking(move || s.push(&post, &c)).await {
        Ok(Ok(())) => {
            eprintln!(
                "forms: `{form}` post kept in the spool ({} waiting) until the database takes it",
                spool.len()
            );
            spool.kick();
            success_redirect(def, page)
        }
        Ok(Err(e @ (SpoolError::Full { .. } | SpoolError::ClientFull { .. }))) => {
            eprintln!(
                "forms: `{form}` post from {client} refused: the database is unavailable and {e} \
                 ([limits] form_spool_mb)"
            );
            let mut r = plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "This form cannot be taken right now: the site is not accepting more submissions \
                 until its database is back. Please try again later.",
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("300"));
            r
        }
        Ok(Err(SpoolError::Io(e))) => {
            eprintln!("forms: spooling a `{form}` post: {e:#}");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The form could not be saved.",
            )
        }
        Err(e) => {
            eprintln!("forms: spooling a `{form}` post: {e}");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The form could not be saved.",
            )
        }
    }
}

/// The client a spooled post is counted against (as in the rate limit).
fn client_of(post: &Spooled) -> &str {
    post.ip.as_deref().unwrap_or("unknown")
}

/// Write the spooled posts to the database, oldest first, each exactly
/// once: the post's key is claimed, the submission stored, counted in the
/// rate limit and its effects (queued mail) run in one transaction, and
/// the file is removed only after it commits.
///
/// When the database is unavailable the replay stops and the post stays
/// for the next try. When the database refuses a post (a constraint, bad
/// data), it is tried again on the next rounds and set aside in
/// `quarantine/` after [`crate::spool::MAX_TRIES`], so the posts after it
/// go on. Returns how many were written.
pub async fn replay(forms: &Forms, hooks: &dyn Hooks) -> anyhow::Result<usize> {
    let (Some(spool), Some(store)) = (forms.spool(), forms.store()) else {
        return Ok(0);
    };
    let _one = spool.replaying.lock().await;
    let mut n = 0;
    for e in spool.entries()? {
        let post = match spool.read(&e) {
            Ok(p) => p,
            Err(err) => {
                let to = spool.quarantine(&e, None)?;
                eprintln!("forms: {err:#}; set aside in {}", to.display());
                continue;
            }
        };
        match replay_one(forms, store, hooks, &post).await {
            Ok(()) => {
                spool.remove(&e, client_of(&post))?;
                n += 1;
            }
            Err(err)
                if engine_db::health::is_unavailable_any(&err) || store.db().health().is_down() =>
            {
                return Err(err);
            }
            Err(err) => {
                let tries = spool.failed(&e);
                if tries < crate::spool::MAX_TRIES {
                    eprintln!(
                        "forms: the database refused spooled `{}` post {} (try {tries} of {}): \
                         {err:#}",
                        post.form,
                        post.key,
                        crate::spool::MAX_TRIES
                    );
                    return Err(err);
                }
                let to = spool.quarantine(&e, Some(client_of(&post)))?;
                eprintln!(
                    "forms: the database refused spooled `{}` post {} {tries} times ({err:#}); \
                     set aside in {} so the posts after it go on",
                    post.form,
                    post.key,
                    to.display()
                );
            }
        }
    }
    Ok(n)
}

async fn replay_one(
    forms: &Forms,
    store: &FormStore,
    hooks: &dyn Hooks,
    post: &Spooled,
) -> anyhow::Result<()> {
    let def = forms.get(&post.form).await?.map(|f| f.def);
    let client = client_of(post);
    let mut tx = store.db().begin().await?;
    if !FormStore::claim_replay(&mut tx, &post.key).await? {
        // Replayed before (a crash after the commit): nothing to do.
        return Ok(());
    }
    let id = if post.store {
        match FormStore::insert_keyed(
            &mut tx,
            &post.key,
            &post.form,
            &post.data,
            post.unique.as_deref(),
            post.user_id,
            post.ip.as_deref(),
            &post.user_agent,
        )
        .await?
        {
            Some(id) => Some(id),
            // Stored by the first try after all (its answer was lost), or
            // a unique value already there (`None`: no effects, as when it
            // is sent twice).
            None => FormStore::submission_by_key(&mut tx, &post.key).await?,
        }
    } else {
        None
    };
    match &def {
        Some(def) if !post.store || id.is_some() => {
            // Counted in the hour's limit like a post taken live; over the
            // limit (posts before the outage plus during it), the post is
            // kept but sends nothing.
            let recent = FormStore::recent_accepted_on(&mut tx, &post.form, client, 60).await?;
            if recent >= def.spam.max_per_hour {
                FormStore::record_event_on(
                    &mut tx,
                    &post.form,
                    "spam",
                    client,
                    Some("rate limit (replayed)"),
                )
                .await?;
                eprintln!(
                    "forms: replayed `{}` post from {client} is over the hourly limit: stored, \
                     no mail sent",
                    post.form
                );
            } else {
                FormStore::record_event_on(&mut tx, &post.form, "accepted", client, None).await?;
                let visitor = post.user_id.map(|user_id| Visitor {
                    user_id,
                    csrf: String::new(),
                    email: post.visitor_email.clone().unwrap_or_default(),
                    name: post.visitor_name.clone().unwrap_or_default(),
                });
                let sub = Submission {
                    id: id.unwrap_or(0),
                    form: post.form.clone(),
                    data: post.data.clone(),
                    user_id: post.user_id,
                    ip: post.ip.clone(),
                    created_at: post.created_at.clone(),
                };
                if let Err(err) = hooks
                    .submitted(def, &sub, visitor.as_ref(), Some(&mut tx))
                    .await
                {
                    if engine_db::health::is_unavailable_any(&err) {
                        return Err(err);
                    }
                    eprintln!(
                        "forms: after replayed `{}` post {}: {err:#}",
                        post.form, sub.id
                    );
                }
            }
        }
        Some(_) => {}
        None => eprintln!(
            "forms: replayed a post to `{}`, a form that no longer exists (stored, no mail sent)",
            post.form
        ),
    }
    tx.commit().await?;
    Ok(())
}

/// Replay the spool whenever a post is added, the database comes back,
/// and every few seconds while posts wait. Runs until the process ends.
pub async fn replay_forever(forms: Forms, hooks: Arc<dyn Hooks>) {
    let (Some(spool), Some(store)) = (forms.spool().cloned(), forms.store().cloned()) else {
        return;
    };
    let health = store.db().health().clone();
    let mut up = health.subscribe();
    loop {
        if !spool.is_empty() && !health.is_down() {
            match replay(&forms, hooks.as_ref()).await {
                Ok(0) => {}
                Ok(n) => println!("forms: {n} spooled post(s) written to the database"),
                Err(e) => eprintln!("forms: replaying the spool: {e:#}"),
            }
        }
        let wait = if spool.is_empty() {
            std::time::Duration::from_secs(3600)
        } else {
            std::time::Duration::from_secs(5)
        };
        tokio::select! {
            _ = spool.kicked() => {}
            r = up.changed() => if r.is_err() { return },
            _ = tokio::time::sleep(wait) => {}
        }
    }
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
