//! `GET /admin/api/components` and `GET /admin/api/tokens`: the component
//! catalogue of this build and site, and the site's design tokens, as JSON
//! for signed-in admin users (the visual editor's data; also handy for
//! tools). Signed-out requests get 401, accounts without admin rights 403,
//! both as JSON, never a redirect.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::Admin;

/// What the endpoints serve, computed when the server starts.
#[derive(Debug, Clone, Default)]
pub struct Design {
    /// `engine_core::components::Catalogue::to_json()`.
    pub components: serde_json::Value,
    /// `engine_core::ui::tokens_json()`.
    pub tokens: serde_json::Value,
}

fn json(status: StatusCode, v: serde_json::Value) -> Response {
    let mut r = (status, Json(v)).into_response();
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    json(
        status,
        serde_json::json!({"error": {"status": status.as_u16(), "code": code, "message": message}}),
    )
}

async fn serve(
    a: &Admin,
    headers: &HeaderMap,
    pick: fn(&Design) -> &serde_json::Value,
) -> Response {
    // The one admin gate (session, admin access, TOTP enrolment).
    if let Err(r) = crate::blocksapi::gated(a, headers).await {
        return r;
    }
    let Some(d) = a.design() else {
        return error(StatusCode::NOT_FOUND, "not_found", "Not available.");
    };
    json(StatusCode::OK, pick(d).clone())
}

async fn components(State(a): State<Admin>, headers: HeaderMap) -> Response {
    serve(&a, &headers, |d| &d.components).await
}

async fn tokens(State(a): State<Admin>, headers: HeaderMap) -> Response {
    serve(&a, &headers, |d| &d.tokens).await
}

pub(crate) fn routes(r: Router<Admin>) -> Router<Admin> {
    r.route("/admin/api/components", get(components))
        .route("/admin/api/tokens", get(tokens))
}
