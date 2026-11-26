//! `GET /media/<id>/<file>`: public files, with headers that keep any file
//! from running script on the site's origin. Paths are checked against the
//! shape the engine writes (`<digits>/<slug>[-<width>].<ext>`), so nothing
//! from the request reaches the file system unchecked.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::MediaStore;
use crate::sniff::Kind;

/// The headers every media response carries.
pub fn safe_headers(kind: Kind) -> Vec<(header::HeaderName, HeaderValue)> {
    let mut h = vec![
        (header::CONTENT_TYPE, HeaderValue::from_static(kind.mime())),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        // Even a file a browser decides to render as a document gets no
        // script, no plugins, no forms and a unique origin.
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox",
            ),
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-site"),
        ),
    ];
    if !kind.is_image() {
        h.push((
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        ));
    }
    h
}

/// Whether `file` has the shape of a stored file name; returns its type.
pub fn parse_file_name(file: &str) -> Option<Kind> {
    let (stem, ext) = file.rsplit_once('.')?;
    let kind = Kind::from_ext(ext)?;
    let ok = !stem.is_empty()
        && stem.len() <= 70
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !stem.starts_with('-')
        && !stem.ends_with('-');
    ok.then_some(kind)
}

async fn serve(
    State(store): State<MediaStore>,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "not found").into_response();
    if id.is_empty() || id.len() > 18 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return not_found();
    }
    let Some(kind) = parse_file_name(&file) else {
        return not_found();
    };
    let path = store.public_dir().join(&id).join(&file);
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return not_found();
    };
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    for (k, v) in safe_headers(kind) {
        h.insert(k, v);
    }
    // A stored file never changes (a new upload gets a new id).
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    resp
}

/// The public `/media/` route.
pub fn router(store: MediaStore) -> Router {
    Router::new()
        .route("/media/{id}/{file}", get(serve))
        .with_state(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_engine_shaped_names_are_served() {
        assert_eq!(parse_file_name("harbour-at-dawn.jpg"), Some(Kind::Jpeg));
        assert_eq!(
            parse_file_name("harbour-at-dawn-640.webp"),
            Some(Kind::Webp)
        );
        for bad in [
            "../secret.jpg",
            "a/b.jpg",
            ".jpg",
            "x.svg",
            "x.html",
            "X.jpg",
            "x.jpg.php",
            "-x.jpg",
            "x%2e.jpg",
            "x..jpg",
        ] {
            assert_eq!(parse_file_name(bad), None, "{bad}");
        }
    }
}
