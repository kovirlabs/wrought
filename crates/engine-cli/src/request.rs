//! `request` mode in `engine serve` (feature `auth`).
//!
//! A GET is rendered for that one request, never written to disk, when:
//!
//! * its route has `mode = "request"` (and `requires = "auth"` routes send
//!   signed-out visitors to the sign-in page), or
//! * the visitor is signed in, or the URL has a query string, and the route
//!   is not `always_static` (the spec's rule: a session cookie or a query
//!   string means `request` mode unless the route opts out).
//!
//! Templates then also get `user` (`id`, `name`, `email`, `roles`, `admin`,
//! `csrf`; or nothing) and `request` (`path`, `query`, `args`). Everything else is served from the published files.
//!
//! v0.3 renders every such request in full; the fragment cache with
//! tag-based eviction is roadmap v0.4 and slots in at [`render`] (the
//! renderer already reports each render's cache tags).

use std::collections::BTreeMap;

use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine_auth::Auth;
use engine_auth::web::{see_other, url_encode};
use engine_core::builder::Renderer;
use engine_core::config::RenderMode;
use engine_core::routes::normalize_path;
use engine_core::serve::{PageRequest, RequestHandler};
use minijinja::context;

use crate::server::CurrentRenderer;

/// Whether `path` could be a `request`-mode route (cheap; no database).
fn maybe_request_route(r: &Renderer, path: &str) -> bool {
    let cfg = r.config();
    if cfg.render.default == RenderMode::Request {
        return true;
    }
    let normal = normalize_path(path);
    cfg.routes.iter().any(|rc| {
        cfg.effective_mode(rc.mode) == RenderMode::Request
            && match rc.wildcard_prefix() {
                Some(prefix) => path.starts_with(prefix),
                None => normalize_path(&rc.path) == normal,
            }
    }) || r
        .content()
        .pages
        .iter()
        .any(|p| p.mode == Some(RenderMode::Request) && p.url == normal)
}

fn query_args(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let dec = |s: &str| percent_decode(&s.replace('+', " "));
            (!k.is_empty()).then(|| (dec(k), dec(v)))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn handler(current: CurrentRenderer, auth: Auth) -> RequestHandler {
    std::sync::Arc::new(move |req: PageRequest| {
        let current = current.clone();
        let auth = auth.clone();
        Box::pin(async move {
            if req.method != Method::GET && req.method != Method::HEAD {
                return None;
            }
            if req.path.starts_with("/_engine/") {
                return None;
            }
            let session = auth.signed_in(&req.headers).await;
            let r = current();
            if session.is_none() && req.query.is_none() && !maybe_request_route(&r, &req.path) {
                return None;
            }
            render(r, req, session).await
        })
    })
}

/// Resolve and render one request. `None`: not a page (serve files).
async fn render(
    r: std::sync::Arc<Renderer>,
    req: PageRequest,
    session: Option<engine_auth::Session>,
) -> Option<Response> {
    let path = req.path.clone();
    let rr = r.clone();
    let route = match tokio::task::spawn_blocking(move || rr.resolve_path(&path)).await {
        Ok(Ok(Some(route))) => route,
        Ok(Ok(None)) => return None,
        Ok(Err(e)) => return Some(failed(&req.path, e)),
        Err(e) => return Some(failed(&req.path, anyhow::anyhow!("{e}"))),
    };
    let rc = r.route_config(&route);
    let always_static = rc.is_some_and(|c| c.always_static);
    let needs_user = rc.is_some_and(|c| c.requires_auth());
    let per_request = route.mode == RenderMode::Request
        || (!always_static && (session.is_some() || req.query.is_some()));
    if !per_request {
        return None;
    }
    if needs_user && session.is_none() {
        let next = match &req.query {
            Some(q) => format!("{}?{q}", req.path),
            None => req.path.clone(),
        };
        return Some(see_other(&format!(
            "/account/login/?next={}",
            url_encode(&next)
        )));
    }
    let user = session.as_ref().map(|s| {
        context! {
            id => s.user.id, name => s.user.display_name(), email => s.user.email,
            roles => s.access.roles, admin => s.access.can_use_admin(),
            // For forms on personal pages (e.g. sign out); these pages are
            // never cached or written to disk.
            csrf => s.csrf,
        }
    });
    let extra = context! {
        user => user,
        request => context! {
            path => req.path,
            query => req.query,
            args => req.query.as_deref().map(query_args).unwrap_or_default(),
        },
    };
    let shown = req.path.clone();
    let result = tokio::task::spawn_blocking(move || r.render_with(&route, &extra)).await;
    let html = match result {
        Ok(Ok((html, _tags))) => html,
        Ok(Err(e)) => return Some(failed(&shown, e)),
        Err(e) => return Some(failed(&shown, anyhow::anyhow!("{e}"))),
    };
    let mut resp = (StatusCode::OK, html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Personal pages must not be stored by shared caches.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if session.is_some() || needs_user {
            "private, no-store"
        } else {
            "no-cache"
        }),
    );
    h.insert(header::VARY, HeaderValue::from_static("Cookie"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    Some(resp)
}

fn failed(path: &str, e: anyhow::Error) -> Response {
    eprintln!("request render {path}: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "This page could not be rendered.\n",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_args_decode() {
        let a = query_args("page=2&q=a+b%21&flag&=x");
        assert_eq!(a["page"], "2");
        assert_eq!(a["q"], "a b!");
        assert_eq!(a["flag"], "");
        assert_eq!(a.len(), 3);
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%é%C3%A9"), "%éé");
    }
}
