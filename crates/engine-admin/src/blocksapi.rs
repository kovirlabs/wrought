//! Block trees as JSON, for the visual editor (next milestone) and any
//! script running inside the admin.
//!
//! * `GET /admin/api/items/{type}/{id}/` — the item's stored fields (block
//!   trees as saved, not rendered), its `version`, `status` and which fields
//!   hold blocks and may be written by this user. Hidden fields are left out.
//!   For each block field: its label, help and the block types it allows;
//!   and `block_types`, those types' fields (as in `types/blocks/*.toml`,
//!   or taken from the component's manifest) and editor hints (the block
//!   file's, else the component manifest's), which is all an editor needs
//!   to offer a new block type or component without code changes.
//! * `PUT /admin/api/items/{type}/{id}/blocks/{field}/` — body
//!   `{"version": n, "blocks": [...]}`: replaces one block field. The same
//!   checks as the admin's form: a signed-in session, the CSRF token in
//!   `X-CSRF-Token` (and a same-origin request), the edit right on the type
//!   and write access to the field, block validation against
//!   `types/blocks/*.toml`, and the version check: a stale `version` gets
//!   409 with `current_version`, never an overwrite. The save writes a
//!   revision and goes through the change log like any other.
//!
//! Errors use the JSON API's shape: `{"error": {"status", "code", "message"}}`.

use crate::Refusal;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine_auth::web::{FormData, csrf_ok};
use engine_auth::{Action, FieldAccess};
use engine_content::store::{Changes, Key, SaveError};
use engine_content::types::FieldKind;
use serde_json::{Map, Value as Json, json};

use crate::{Admin, Denied};

pub(crate) fn json_response(status: StatusCode, body: &Json) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

/// The admin gate ([`Admin::gate`]) with JSON refusals.
pub(crate) async fn gated(a: &Admin, headers: &HeaderMap) -> Result<engine_auth::Session, Refusal> {
    match a.gate(headers, "/admin/api/").await {
        Ok(s) => Ok(s),
        Err(Denied::SignedOut) => Err(Box::new(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "sign in first",
        ))),
        Err(Denied::NoAdminAccess(_)) => Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this account has no rights in the admin",
        ))),
        Err(Denied::Unavailable) => Err(Box::new(server_error(engine_db::health::unavailable()))),
        Err(Denied::TotpRequired) => Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "totp_required",
            "administrators must turn on two-step sign-in first",
        ))),
    }
}

/// A failure of a JSON endpoint: `503` with `Retry-After` if its cause is
/// that the database is unavailable (by the error's chain), else `500`.
pub(crate) fn server_error(e: impl Into<anyhow::Error>) -> Response {
    let e = e.into();
    eprintln!("admin api: {e:#}");
    if engine_db::health::is_unavailable_any(&e) {
        let mut r = error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the database is unavailable; try again shortly",
        );
        r.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("30"),
        );
        return r;
    }
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "error",
        &format!("{e:#}"),
    )
}

pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        &json!({ "error": { "status": status.as_u16(), "code": code, "message": message } }),
    )
}

pub(crate) async fn get_item(
    State(a): State<Admin>,
    headers: HeaderMap,
    Path((ty, id)): Path<(String, i64)>,
) -> Response {
    let s = match gated(&a, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(t) = a.store().registry().get(&ty).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such content type");
    };
    if !s.access.can(&t.name, Action::View) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "you may not see these items",
        );
    }
    let item = match a.store().get(&t.name, &Key::Id(id), None).await {
        Ok(Some(i)) => i,
        Ok(None) => return error(StatusCode::NOT_FOUND, "not_found", "no such item"),
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "error",
                &format!("{e:#}"),
            );
        }
    };
    let mut fields = Map::new();
    let mut blocks = Vec::new();
    let reg = a.store().registry();
    let mut block_types = Map::new();
    for f in &t.fields {
        let access = s.access.field(&t.name, &f.name, Action::Edit);
        if access == FieldAccess::None {
            continue;
        }
        if let Some(v) = item.fields.get(&f.name) {
            fields.insert(f.name.clone(), v.clone());
        }
        if f.kind == FieldKind::Blocks {
            let allowed: Vec<String> = reg
                .blocks()
                .filter(|b| f.blocks.is_empty() || f.blocks.contains(&b.name))
                .map(|b| b.name.clone())
                .collect();
            for name in &allowed {
                if let Some(b) = reg.block(name) {
                    block_types
                        .entry(name.clone())
                        .or_insert_with(|| block_type_json(&a, b));
                }
            }
            blocks.push(json!({
                "field": f.name,
                "label": crate::forms::label_of(f),
                "help": f.help,
                "blocks": allowed,
                "writable": access == FieldAccess::Write && s.access.can(&t.name, Action::Edit),
            }));
        }
    }
    json_response(
        StatusCode::OK,
        &json!({
            "type": t.name,
            "id": item.id,
            "slug": item.slug,
            "status": item.status.as_str(),
            "version": item.version,
            "fields": fields,
            "block_fields": blocks,
            "block_types": block_types,
            "title": crate::content::title_of(&t, &item),
            "type_label": t.label,
            "url": a.site().item_path(&item),
            "can_publish": s.access.can(&t.name, Action::Publish),
            "preview": cfg!(feature = "workflow")
                .then(|| format!("/admin/content/{}/{}/preview/", t.name, item.id)),
            "editor": cfg!(feature = "visual-editor")
                .then(|| format!("/admin/editor/{}/{}/", t.name, item.id)),
        }),
    )
}

/// A block type for editors: fields, and the editor hints of its type file
/// merged over those of the component it names (from the catalogue).
fn block_type_json(a: &Admin, b: &engine_content::types::BlockDef) -> Json {
    let component = b.component.as_ref().and_then(|c| {
        a.design()?
            .components
            .get("components")?
            .as_array()?
            .iter()
            .find(|x| x.get("name").and_then(Json::as_str) == Some(c.as_str()))
            .cloned()
    });
    let mut hints = component
        .as_ref()
        .and_then(|c| c.get("editor").cloned())
        .and_then(|e| e.as_object().cloned())
        .unwrap_or_default();
    if let Some(own) = b
        .editor
        .as_ref()
        .and_then(|e| serde_json::to_value(e).ok())
        .and_then(|v| v.as_object().cloned())
    {
        hints.extend(own);
    }
    let description = b.description.clone().or_else(|| {
        component
            .as_ref()
            .and_then(|c| c.get("description").and_then(Json::as_str))
            .map(str::to_string)
    });
    json!({
        "name": b.name,
        "label": b.label,
        "description": description,
        "component": b.component,
        "editor": hints,
        "fields": b.fields,
    })
}

pub(crate) async fn put_blocks(
    State(a): State<Admin>,
    headers: HeaderMap,
    Path((ty, id, field)): Path<(String, i64, String)>,
    body: Bytes,
) -> Response {
    let s = match gated(&a, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !csrf_ok(&s, &headers, &FormData::default()) {
        return error(
            StatusCode::FORBIDDEN,
            "csrf",
            "send the session's CSRF token in X-CSRF-Token, from this site",
        );
    }
    let Some(t) = a.store().registry().get(&ty).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such content type");
    };
    if !s.access.can(&t.name, Action::Edit) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "you may not edit these items",
        );
    }
    match t.field(&field) {
        Some(f) if f.kind == FieldKind::Blocks => {}
        _ => return error(StatusCode::NOT_FOUND, "not_found", "no such block field"),
    }
    if s.access.field(&t.name, &field, Action::Edit) != FieldAccess::Write {
        return error(
            StatusCode::FORBIDDEN,
            "field_forbidden",
            "you may not change this field",
        );
    }
    let Ok(body) = serde_json::from_slice::<Json>(&body) else {
        return error(StatusCode::BAD_REQUEST, "bad_json", "the body is not JSON");
    };
    let Some(version) = body
        .get("version")
        .and_then(Json::as_i64)
        .and_then(|v| i32::try_from(v).ok())
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "version_required",
            "say which version you edited (\"version\")",
        );
    };
    let Some(blocks) = body.get("blocks").filter(|b| b.is_array()).cloned() else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "\"blocks\" must be a list",
        );
    };
    let ch = Changes::default().set(&field, blocks);
    match a
        .store()
        .update(&t.name, id, version, &ch, Some(&crate::Admin::author(&s)))
        .await
    {
        Ok(item) => json_response(
            StatusCode::OK,
            &json!({ "id": item.id, "version": item.version }),
        ),
        Err(SaveError::Conflict { current, .. }) => json_response(
            StatusCode::CONFLICT,
            &json!({ "error": {
                "status": 409, "code": "conflict",
                "message": "someone saved this item after the version you edited",
                "current_version": current,
            } }),
        ),
        Err(SaveError::Invalid(e)) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, "invalid", &e.to_string())
        }
        Err(SaveError::NotFound { .. }) => {
            error(StatusCode::NOT_FOUND, "not_found", "no such item")
        }
        Err(e) => server_error(e),
    }
}
