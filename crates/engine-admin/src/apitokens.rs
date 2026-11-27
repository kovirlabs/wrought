//! API tokens (`api` feature): `/admin/api-tokens/`.
//!
//! Everyone who can use the admin manages their own tokens; administrators
//! see and revoke everyone's. A new token's secret is shown once, on the
//! page that answers the form (never stored, never in a redirect URL).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router};
use engine_api::{Scope, Token};
use engine_auth::Session;
use engine_auth::web::{FormData, see_other};
use minijinja::context;

use crate::Admin;

async fn tokens_for(a: &Admin, s: &Session) -> anyhow::Result<Vec<Token>> {
    let store = a.api_tokens().expect("routes exist only with tokens");
    if s.access.is_admin() {
        store.all(500).await
    } else {
        store.of_user(s.user.id).await
    }
}

async fn page(
    a: &Admin,
    s: &Session,
    status: StatusCode,
    created: Option<(Token, String)>,
    error: Option<String>,
) -> Response {
    let list = match tokens_for(a, s).await {
        Ok(l) => l,
        Err(e) => return a.error(s, e),
    };
    let mut owners = std::collections::BTreeMap::new();
    for t in &list {
        if !owners.contains_key(&t.user_id)
            && let Ok(Some(u)) = a.auth().user(t.user_id).await
        {
            owners.insert(t.user_id, u.email);
        }
    }
    let rows: Vec<_> = list
        .iter()
        .map(|t| {
            context! {
                id => t.id, name => t.name, prefix => t.prefix,
                scopes => t.scopes.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                created_at => t.created_at, expires_at => t.expires_at,
                last_used_at => t.last_used_at, revoked_at => t.revoked_at,
                owner => owners.get(&t.user_id), mine => t.user_id == s.user.id,
            }
        })
        .collect();
    let types: Vec<_> = a
        .store()
        .registry()
        .types()
        .filter(|t| s.access.can(&t.name, engine_auth::Action::View))
        .map(|t| context! { name => t.name, label => t.label })
        .collect();
    let created = created.map(|(t, secret)| context! { name => t.name, secret });
    a.page(
        status,
        "admin/api_tokens.html",
        a.base(s, "API tokens"),
        context! { tokens => rows, types, created, error, all => s.access.is_admin() },
    )
}

pub(crate) async fn list(State(a): State<Admin>, headers: HeaderMap, uri: Uri) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    page(&a, &s, StatusCode::OK, None, None).await
}

pub(crate) async fn create(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(store) = a.api_tokens() else {
        return a.not_found(&s);
    };
    let mut scopes = Vec::new();
    for v in form.all("scope") {
        match Scope::parse(v) {
            Some(sc) => scopes.push(sc),
            None => {
                return page(
                    &a,
                    &s,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                    Some(format!("`{v}` is not a scope")),
                )
                .await;
            }
        }
    }
    let days = match form.get("days").map(str::trim).filter(|d| !d.is_empty()) {
        None => None,
        Some(d) => match d.parse::<u32>() {
            Ok(n) => Some(n),
            Err(_) => {
                return page(
                    &a,
                    &s,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                    Some("the lifetime is a number of days".into()),
                )
                .await;
            }
        },
    };
    match store
        .create(s.user.id, form.get("name").unwrap_or(""), &scopes, days)
        .await
    {
        Ok(n) => page(&a, &s, StatusCode::CREATED, Some((n.token, n.secret)), None).await,
        Err(msg) => page(&a, &s, StatusCode::UNPROCESSABLE_ENTITY, None, Some(msg)).await,
    }
}

pub(crate) async fn revoke(
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
    let Some(store) = a.api_tokens() else {
        return a.not_found(&s);
    };
    let t = match store.get(id).await {
        Ok(Some(t)) => t,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    if t.user_id != s.user.id && !s.access.is_admin() {
        return a.forbidden(&s, "revoke other people's tokens");
    }
    match store.revoke(id).await {
        Ok(_) => see_other("/admin/api-tokens/?notice=token-revoked"),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) fn routes(r: Router<Admin>) -> Router<Admin> {
    r.route("/admin/api-tokens/", get(list).post(create))
        .route("/admin/api-tokens/{id}/revoke/", post(revoke))
}
