//! The media library (`media` feature): `/admin/media/`.
//!
//! Upload (multipart, checked by `engine_media`), list with type filter and
//! paging, describe (alt text, version-checked), delete, and download of
//! private files (form attachments). Who: anyone who may create or edit
//! some content type ([`engine_auth::Access::can_manage_media`]).

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use engine_auth::Session;
use engine_auth::web::{FormData, csrf_ok, forbidden_csrf, see_other};
use engine_media::upload::{body_limit, read_multipart};
use engine_media::{ImageData, Media, MediaError, Upload};
use minijinja::context;
use serde::Deserialize;

use crate::{Admin, PER_PAGE, Pager};

#[derive(Deserialize)]
pub(crate) struct ListQ {
    page: Option<i64>,
    kind: Option<String>,
    private: Option<String>,
    notice: Option<String>,
}

fn allowed(a: &Admin, s: &Session) -> Result<(), Response> {
    if s.access.can_manage_media() {
        Ok(())
    } else {
        Err(a.forbidden(s, "use the media library"))
    }
}

fn view(m: &Media) -> minijinja::Value {
    context! {
        id => m.id, name => m.file_name(), original_name => m.original_name, mime => m.mime,
        kb => (m.bytes + 1023) / 1024, width => m.width, height => m.height, alt => m.alt,
        private => m.private, version => m.version, created_at => m.created_at,
        is_image => m.is_image(), url => m.url(), srcset => m.srcset(),
        thumb => m.variants.first().and_then(|w| m.variant_url(*w)).or_else(|| m.url()),
        variants => m.variants.iter().filter_map(|w| m.variant_url(*w).map(|u| context! { width => w, url => u })).collect::<Vec<_>>(),
        data => ImageData::of(m).map(|d| minijinja::Value::from_serialize(&d)),
    }
}

async fn list_page(
    a: &Admin,
    s: &Session,
    q: &ListQ,
    status: StatusCode,
    error: Option<String>,
) -> Response {
    let Some(store) = a.media() else {
        return a.not_found(s);
    };
    let kind = q.kind.as_deref().filter(|k| ["image", "pdf"].contains(k));
    let private = q.private.as_deref() == Some("1");
    let total = match store.count(private, kind).await {
        Ok(n) => n,
        Err(e) => return a.error(s, e),
    };
    let href = |p: i64| {
        format!(
            "/admin/media/?page={p}{}{}",
            kind.map(|k| format!("&kind={k}")).unwrap_or_default(),
            if private { "&private=1" } else { "" }
        )
    };
    let pager = Pager::new(q.page.unwrap_or(1), total, href);
    let items = match store.list(private, kind, PER_PAGE, pager.offset()).await {
        Ok(v) => v,
        Err(e) => return a.error(s, e),
    };
    let items: Vec<_> = items.iter().map(view).collect();
    let cfg = store.config();
    a.page(
        status,
        "admin/media.html",
        a.base(s, "Media"),
        context! {
            items, pager, kind, private, error, notice => q.notice,
            max_mb => cfg.max_mb, max_megapixels => cfg.max_megapixels,
        },
    )
}

pub(crate) async fn list(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Query(q): Query<ListQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = allowed(&a, &s) {
        return r;
    }
    list_page(&a, &s, &q, StatusCode::OK, None).await
}

pub(crate) async fn upload(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    mp: Multipart,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = allowed(&a, &s) {
        return r;
    }
    let Some(store) = a.media() else {
        return a.not_found(&s);
    };
    let q = ListQ {
        page: None,
        kind: None,
        private: None,
        notice: None,
    };
    let parts = match read_multipart(mp, store.config().max_bytes(), 1).await {
        Ok(p) => p,
        Err(e) => {
            let status = media_status(&e);
            return list_page(&a, &s, &q, status, Some(e.to_string())).await;
        }
    };
    // The CSRF token travels in the multipart body.
    if !csrf_ok(&s, &headers, &FormData(parts.fields.clone())) {
        return forbidden_csrf(a.ui());
    }
    let alt = parts
        .fields
        .iter()
        .rev()
        .find(|(k, _)| k == "alt")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let Some(file) = parts.files.first() else {
        return list_page(
            &a,
            &s,
            &q,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("Choose a file to upload.".into()),
        )
        .await;
    };
    match store
        .upload(Upload {
            bytes: &file.bytes,
            filename: &file.filename,
            alt,
            private: false,
            uploaded_by: Some(s.user.id),
        })
        .await
    {
        Ok(m) => see_other(&format!("/admin/media/{}/?notice=uploaded", m.id)),
        Err(e @ MediaError::Other(_)) => a.error(&s, e),
        Err(e) => list_page(&a, &s, &q, media_status(&e), Some(e.to_string())).await,
    }
}

fn media_status(e: &MediaError) -> StatusCode {
    match e {
        MediaError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        MediaError::Refused(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        MediaError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        MediaError::Conflict { .. } => StatusCode::CONFLICT,
        MediaError::NotFound => StatusCode::NOT_FOUND,
        MediaError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Deserialize)]
pub(crate) struct ItemQ {
    notice: Option<String>,
}

async fn item_page(
    a: &Admin,
    s: &Session,
    id: i64,
    status: StatusCode,
    notice: Option<String>,
    error: Option<String>,
) -> Response {
    let Some(store) = a.media() else {
        return a.not_found(s);
    };
    let m = match store.get(id).await {
        Ok(Some(m)) => m,
        Ok(None) => return a.not_found(s),
        Err(e) => return a.error(s, e),
    };
    a.page(
        status,
        "admin/media_item.html",
        a.base(s, &m.file_name()),
        context! { m => view(&m), notice, error },
    )
}

pub(crate) async fn item(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Query(q): Query<ItemQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = allowed(&a, &s) {
        return r;
    }
    item_page(&a, &s, id, StatusCode::OK, q.notice, None).await
}

pub(crate) async fn save(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = allowed(&a, &s) {
        return r;
    }
    let Some(store) = a.media() else {
        return a.not_found(&s);
    };
    let version: i32 = form
        .get("_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    match store
        .set_alt(id, version, form.get("alt").unwrap_or(""))
        .await
    {
        Ok(_) => see_other(&format!("/admin/media/{id}/?notice=saved")),
        Err(MediaError::Other(e)) => a.error(&s, e),
        Err(e) => item_page(&a, &s, id, media_status(&e), None, Some(e.to_string())).await,
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
        Err(r) => return r,
    };
    if let Err(r) = allowed(&a, &s) {
        return r;
    }
    let Some(store) = a.media() else {
        return a.not_found(&s);
    };
    let version: i32 = form
        .get("_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    match store.delete(id, version).await {
        Ok(()) => see_other("/admin/media/?notice=deleted"),
        Err(MediaError::Other(e)) => a.error(&s, e),
        Err(e) => item_page(&a, &s, id, media_status(&e), None, Some(e.to_string())).await,
    }
}

/// `GET /admin/media/<id>/file`: the file itself, private ones included,
/// always as a download with the safe headers.
pub(crate) async fn file(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(store) = a.media() else {
        return a.not_found(&s);
    };
    let m = match store.get(id).await {
        Ok(Some(m)) => m,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    // Private files are form attachments: for people who read submissions.
    let ok = if m.private {
        s.access.can_manage_forms()
    } else {
        s.access.can_manage_media()
    };
    if !ok {
        return a.forbidden(&s, "open this file");
    }
    let Some(kind) = m.kind() else {
        return a.not_found(&s);
    };
    let bytes = match store.read(&m).await {
        Ok(b) => b,
        Err(e) => return a.error(&s, e),
    };
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    for (k, v) in engine_media::web::safe_headers(kind) {
        h.insert(k, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", m.file_name())) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    resp
}

/// The library's routes; uploads get a body limit that fits `[media] max_mb`.
pub(crate) fn routes(r: Router<Admin>, max_bytes: u64) -> Router<Admin> {
    r.route(
        "/admin/media/",
        get(list)
            .post(upload)
            .layer(DefaultBodyLimit::max(body_limit(max_bytes, 1))),
    )
    .route("/admin/media/{id}/", get(item).post(save))
    .route("/admin/media/{id}/delete/", post(delete))
    .route("/admin/media/{id}/file", get(file))
}
