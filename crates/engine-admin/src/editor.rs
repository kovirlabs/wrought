//! The visual editor (`visual-editor` feature): a block editor for the
//! block fields of an item, with a live preview rendered by the engine.
//!
//! * `GET /admin/editor/{type}/{id}/` — the editor page. The app (Svelte
//!   and Tiptap, built from `editor/` into `editor-dist/` and embedded) is
//!   one same-origin module; the admin's strict CSP applies unchanged.
//! * `POST /admin/api/items/{type}/{id}/preview/` — body `{"tab": "...",
//!   "fields": {"<block field>": [...]}}`: renders the item with these
//!   (unsaved) block trees and keeps the page for the frame below. The
//!   blocks go through exactly the checks of a save
//!   ([`engine_content::Store::validate_changes`]: block types, fields,
//!   URL schemes, token names, ids) and the page through exactly the
//!   rendering of a published page (Markdown sanitised, values escaped);
//!   nothing is saved, cached or written. The engine fetches nothing for
//!   a preview (no URL in a block is ever requested by the server), so it
//!   is no SSRF vector.
//! * `GET /admin/editor/{type}/{id}/frame/?tab=...` — the page last
//!   rendered for this user and editor tab (or the saved item), for the
//!   editor's `<iframe>`: same-origin framing only, scripts only from this
//!   origin, forms cannot submit, never cached or indexed.
//! * `PUT /admin/api/items/{type}/{id}/blocks/` — body `{"version": n,
//!   "fields": {...}, "status": "published"?}`: saves every edited block
//!   field in one revision, with the version check (409 with
//!   `current_version`, never an overwrite).
//!
//! All of them go through the one admin gate (session, admin access, TOTP
//! enrolment); the state-changing ones need the CSRF token in
//! `X-CSRF-Token` from this origin; each checks the type's edit right and
//! write access to every field it touches.

use crate::Refusal;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use engine_auth::web::{FormData, UiAsset, csrf_ok};
use engine_auth::{Action, FieldAccess, Session};
use engine_content::store::{Changes, Item, Key, SaveError, Status};
use engine_content::types::{FieldKind, TypeDef};
use minijinja::context;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::Admin;
use crate::blocksapi::{error, gated, json_response, server_error};

// EDITOR_JS, EDITOR_CSS, FRAME_CSS, LICENSES: asset names with a hash of
// their content (they are served as immutable), from build.rs.
include!(concat!(env!("OUT_DIR"), "/editor_assets.rs"));

/// The editor page's template (`admin/editor.html`), kept out of the
/// admin's own folder so a build without the feature does not carry it.
pub(crate) static TEMPLATES: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/templates-editor");

/// The editor's files, served under `/_engine/ui/`.
pub(crate) const ASSETS: [UiAsset; 4] = [
    UiAsset {
        name: EDITOR_JS,
        body: include_bytes!("../editor-dist/editor.mjs"),
        content_type: "text/javascript; charset=utf-8",
    },
    UiAsset {
        name: EDITOR_CSS,
        body: include_bytes!("../editor-dist/editor.css"),
        content_type: "text/css; charset=utf-8",
    },
    UiAsset {
        name: FRAME_CSS,
        body: include_bytes!("../editor-frame.css"),
        content_type: "text/css; charset=utf-8",
    },
    UiAsset {
        name: LICENSES,
        body: include_bytes!("../editor-dist/editor-licenses.txt"),
        content_type: "text/plain; charset=utf-8",
    },
];

/// CSP of the preview frame: framed only by the admin (same origin),
/// scripts and styles of the site's own origin (its pages may use https
/// stylesheets, images and video players), no form submissions, no
/// plugins, no `<base>` tricks.
const FRAME_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https:; \
     img-src 'self' data: https:; media-src 'self' https:; font-src 'self' data: https:; \
     frame-src https:; connect-src 'self'; form-action 'none'; frame-ancestors 'self'; \
     base-uri 'none'; object-src 'none'";

/// How long an unsaved preview is kept for its frame, and how many.
const FRAME_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_FRAMES: usize = 64;
/// Largest preview page kept (a page this big is not a block page).
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

type FrameKey = (i64, String, i64, String);

/// Previews waiting for their frame: per user, item and editor tab, the
/// last page rendered. In memory only; bounded in age and number.
#[derive(Default)]
pub(crate) struct Frames(Mutex<HashMap<FrameKey, (Instant, Arc<String>)>>);

impl Frames {
    fn put(&self, key: FrameKey, html: String) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        m.retain(|_, (at, _)| now.duration_since(*at) < FRAME_TTL);
        if m.len() >= MAX_FRAMES && !m.contains_key(&key) {
            // Drop the oldest.
            if let Some(k) = m
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                m.remove(&k);
            }
        }
        m.insert(key, (now, Arc::new(html)));
    }

    fn get(&self, key: &FrameKey) -> Option<Arc<String>> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(key)
            .filter(|(at, _)| at.elapsed() < FRAME_TTL)
            .map(|(_, h)| h.clone())
    }
}

/// An editor tab's key: letters and digits, at most 32.
fn valid_tab(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|c| c.is_ascii_alphanumeric())
}

pub(crate) fn routes(r: Router<Admin>) -> Router<Admin> {
    r.route("/admin/editor/{ty}/{id}/", get(page))
        .route("/admin/editor/{ty}/{id}/frame/", get(frame))
        .route("/admin/api/items/{ty}/{id}/preview/", post(preview))
        .route("/admin/api/items/{ty}/{id}/blocks/", put(save))
}

/// Block fields of `t` this session may at least read, in order.
fn block_fields<'a>(s: &Session, t: &'a TypeDef) -> Vec<&'a engine_content::types::FieldDef> {
    t.fields
        .iter()
        .filter(|f| {
            f.kind == FieldKind::Blocks
                && s.access.field(&t.name, &f.name, Action::Edit) != FieldAccess::None
        })
        .collect()
}

/// `GET /admin/editor/{type}/{id}/`.
async fn page(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(t) = a.store().registry().get(&ty).cloned() else {
        return a.not_found(&s);
    };
    if !s.access.can(&t.name, Action::View) {
        return a.not_found(&s);
    }
    let item = match a.store().get(&t.name, &Key::Id(id), None).await {
        Ok(Some(i)) => i,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    if block_fields(&s, &t).is_empty() {
        return a.message(
            &s,
            StatusCode::NOT_FOUND,
            "No blocks to edit",
            "This type has no block fields you may edit, so there is nothing for the visual editor.",
        );
    }
    let title = crate::content::title_of(&t, &item);
    a.page(
        StatusCode::OK,
        "admin/editor.html",
        a.base(&s, &format!("Visual editor: {title}")),
        context! {
            item_title => title,
            ty => context! { name => t.name, label => t.label },
            id => item.id,
            edit_url => format!("/admin/content/{}/{}/", t.name, item.id),
            js => format!("{}{EDITOR_JS}", engine_auth::web::ASSETS),
            css => format!("{}{EDITOR_CSS}", engine_auth::web::ASSETS),
            frame_css => format!("{}{FRAME_CSS}", engine_auth::web::ASSETS),
            licenses => format!("{}{LICENSES}", engine_auth::web::ASSETS),
        },
    )
}

#[derive(Deserialize)]
struct FrameQuery {
    #[serde(default)]
    tab: String,
}

fn frame_response(html: String) -> Response {
    let mut r = (StatusCode::OK, html).into_response();
    let h = r.headers_mut();
    for (k, v) in [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
        (header::REFERRER_POLICY, "same-origin"),
        (header::CONTENT_SECURITY_POLICY, FRAME_CSP),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    r
}

/// `GET /admin/editor/{type}/{id}/frame/`.
async fn frame(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
    Query(q): Query<FrameQuery>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let Some(t) = a.store().registry().get(&ty).cloned() else {
        return a.not_found(&s);
    };
    if !s.access.can(&t.name, Action::View) {
        return a.not_found(&s);
    }
    if valid_tab(&q.tab)
        && let Some(html) = a
            .frames()
            .get(&(s.user.id, t.name.clone(), id, q.tab.clone()))
    {
        return frame_response(html.as_ref().clone());
    }
    let item = match a.store().get(&t.name, &Key::Id(id), None).await {
        Ok(Some(i)) => i,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    match render(&a, &item, &headers).await {
        Ok(Some(html)) => frame_response(html),
        Ok(None) => frame_response(no_page_html()),
        Err(e) => a.error(&s, e),
    }
}

fn no_page_html() -> String {
    "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>No preview</title></head>\
     <body><main><p>This type has no page of its own on the site, so there is no preview. \
     The blocks are still saved.</p></main></body></html>"
        .to_string()
}

async fn render(a: &Admin, item: &Item, headers: &HeaderMap) -> anyhow::Result<Option<String>> {
    let Some(p) = a.preview() else {
        return Ok(None);
    };
    p.render(item, headers, true).await
}

/// Parse `{"fields": {...}}` of a preview or save: only block fields this
/// session may write.
fn block_changes(s: &Session, t: &TypeDef, body: &Json) -> Result<Vec<(String, Json)>, Refusal> {
    let Some(fields) = body.get("fields").and_then(Json::as_object) else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "\"fields\" must be an object of block fields",
        )));
    };
    let mut out = Vec::new();
    for (name, v) in fields {
        match t.field(name) {
            Some(f) if f.kind == FieldKind::Blocks => {}
            _ => {
                return Err(Box::new(error(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    &format!("no block field `{name}`"),
                )));
            }
        }
        if s.access.field(&t.name, name, Action::Edit) != FieldAccess::Write {
            return Err(Box::new(error(
                StatusCode::FORBIDDEN,
                "field_forbidden",
                &format!("you may not change field `{name}`"),
            )));
        }
        if !v.is_array() {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &format!("`{name}` must be a list of blocks"),
            )));
        }
        out.push((name.clone(), v.clone()));
    }
    Ok(out)
}

/// The gate, the CSRF header, the type and its edit right, and the body.
async fn writer(
    a: &Admin,
    headers: &HeaderMap,
    ty: &str,
    body: &Bytes,
) -> Result<(Session, TypeDef, Json), Refusal> {
    let s = gated(a, headers).await?;
    if !csrf_ok(&s, headers, &FormData::default()) {
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "csrf",
            "send the session's CSRF token in X-CSRF-Token, from this site",
        )));
    }
    let Some(t) = a.store().registry().get(ty).cloned() else {
        return Err(Box::new(error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such content type",
        )));
    };
    if !s.access.can(&t.name, Action::Edit) {
        return Err(Box::new(error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "you may not edit these items",
        )));
    }
    let Ok(body) = serde_json::from_slice::<Json>(body) else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_json",
            "the body is not JSON",
        )));
    };
    Ok((s, t, body))
}

/// `POST /admin/api/items/{type}/{id}/preview/`.
async fn preview(
    State(a): State<Admin>,
    headers: HeaderMap,
    Path((ty, id)): Path<(String, i64)>,
    body: Bytes,
) -> Response {
    let (s, t, body) = match writer(&a, &headers, &ty, &body).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let tab = body.get("tab").and_then(Json::as_str).unwrap_or("");
    if !valid_tab(tab) {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "\"tab\" must be 1 to 32 letters and digits",
        );
    }
    let fields = match block_changes(&s, &t, &body) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let ch = Changes {
        fields,
        ..Changes::default()
    };
    // The same validation as a save: the values are stored-form JSON.
    let values = match a.store().validate_changes(&t.name, &ch) {
        Ok(v) => v,
        Err(SaveError::Invalid(e)) => {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid", &e.to_string());
        }
        Err(e) => return server_error(e),
    };
    let mut item = match a.store().get(&t.name, &Key::Id(id), None).await {
        Ok(Some(i)) => i,
        Ok(None) => return error(StatusCode::NOT_FOUND, "not_found", "no such item"),
        Err(e) => return server_error(e),
    };
    for (k, v) in &values {
        item.fields.insert(k.clone(), v.clone());
    }
    let html = match render(&a, &item, &headers).await {
        Ok(Some(h)) => h,
        Ok(None) => no_page_html(),
        Err(e) => {
            eprintln!("visual editor: preview of {} #{id}: {e:#}", t.name);
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "render",
                &format!("the page could not be rendered: {e:#}"),
            );
        }
    };
    if html.len() > MAX_FRAME_BYTES {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the preview page is too large",
        );
    }
    a.frames().put(
        (s.user.id, t.name.clone(), id, tab.to_string()),
        html.clone(),
    );
    let normalised: Map<String, Json> = values.into_iter().collect();
    json_response(
        StatusCode::OK,
        &json!({
            "frame": format!("/admin/editor/{}/{id}/frame/?tab={tab}", t.name),
            "fields": normalised,
            "html": html,
        }),
    )
}

/// `PUT /admin/api/items/{type}/{id}/blocks/`.
async fn save(
    State(a): State<Admin>,
    headers: HeaderMap,
    Path((ty, id)): Path<(String, i64)>,
    body: Bytes,
) -> Response {
    let (s, t, body) = match writer(&a, &headers, &ty, &body).await {
        Ok(x) => x,
        Err(r) => return *r,
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
    let fields = match block_changes(&s, &t, &body) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let mut ch = Changes {
        fields,
        ..Changes::default()
    };
    match body.get("status") {
        None | Some(Json::Null) => {}
        Some(v) => {
            let Some(st) = v.as_str().and_then(Status::parse) else {
                return error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "\"status\" must be \"draft\" or \"published\"",
                );
            };
            if !s.access.can(&t.name, Action::Publish) {
                return error(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "your roles cannot publish or unpublish these items",
                );
            }
            ch.status = Some(st);
        }
    }
    match a
        .store()
        .update(&t.name, id, version, &ch, Some(&Admin::author(&s)))
        .await
    {
        Ok(item) => {
            let saved: Map<String, Json> = ch
                .fields
                .iter()
                .map(|(k, _)| (k.clone(), item.fields.get(k).cloned().unwrap_or(Json::Null)))
                .collect();
            json_response(
                StatusCode::OK,
                &json!({
                    "id": item.id,
                    "version": item.version,
                    "status": item.status.as_str(),
                    "url": a.site().item_path(&item),
                    "fields": saved,
                }),
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_bounded() {
        let f = Frames::default();
        for i in 0..(MAX_FRAMES as i64 + 10) {
            f.put((1, "a".into(), i, "t".into()), format!("{i}"));
        }
        let n = f.0.lock().unwrap().len();
        assert_eq!(n, MAX_FRAMES);
        assert!(
            f.get(&(1, "a".into(), 0, "t".into())).is_none(),
            "oldest dropped"
        );
        assert_eq!(
            f.get(&(1, "a".into(), MAX_FRAMES as i64 + 9, "t".into()))
                .unwrap()
                .as_str(),
            &format!("{}", MAX_FRAMES + 9)
        );
        // Another user's tab is another key.
        assert!(
            f.get(&(2, "a".into(), MAX_FRAMES as i64 + 9, "t".into()))
                .is_none()
        );
    }

    #[test]
    fn tab_keys() {
        assert!(valid_tab("a1B2"));
        for bad in ["", "a b", "a/b", &"x".repeat(33), "é"] {
            assert!(!valid_tab(bad), "{bad}");
        }
    }

    #[test]
    fn frame_policy_is_strict() {
        for must in [
            "script-src 'self';",
            "form-action 'none'",
            "frame-ancestors 'self'",
            "object-src 'none'",
            "base-uri 'none'",
        ] {
            assert!(FRAME_CSP.contains(must), "{must}");
        }
        assert!(!FRAME_CSP.contains("unsafe-eval"));
        assert!(!FRAME_CSP.contains("script-src 'self' 'unsafe-inline'"));
    }
}
