//! `request` mode in `engine serve`.
//!
//! A GET is rendered for that one request, never written to disk, when:
//!
//! * its route has `mode = "request"` (and `requires = "auth"` routes send
//!   signed-out visitors to the sign-in page), or
//! * the visitor is signed in, or the URL has a query string, and the route
//!   is not `always_static` (the spec's rule: a session cookie or a query
//!   string means `request` mode unless the route opts out).
//!
//! Templates then also get `user` (`id`, `name`, `full_name`, `email`,
//! `roles`, `admin`, `csrf`; or nothing), `csrf` (the visitor's form token:
//! the session's, or for signed-out visitors a double-submit cookie's, set
//! on the response when new) and `request` (`path`, `query`, `args`).
//! Everything else is served from the published files.
//!
//! Pages are rendered in full on every request; parts marked with
//! `fragment()` come from the fragment cache (see
//! `engine_core::fragments`). Responses say how many fragments were reused:
//! `X-Wrought-Fragments: hits=2 misses=0`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine_core::builder::Renderer;
use engine_core::config::RenderMode;
use engine_core::fragments::{self, RenderStats};
use engine_core::routes::normalize_path;
use engine_core::serve::{PageRequest, RequestHandler};
use engine_core::web::{AnonCsrf, parse_query, url_encode};
use minijinja::{Value, context};

use crate::server::CurrentRenderer;

/// A signed-in visitor (feature `auth`).
#[cfg(feature = "auth")]
pub type Session = engine_auth::Session;
#[cfg(not(feature = "auth"))]
pub type Session = std::convert::Infallible;

/// Everything `request` renders need. Cheap to clone.
#[derive(Clone)]
pub struct RequestMode {
    pub current: CurrentRenderer,
    pub csrf: AnonCsrf,
    #[cfg(feature = "auth")]
    pub auth: Option<engine_auth::Auth>,
}

/// One rendered page.
pub struct Rendered {
    pub html: String,
    pub stats: RenderStats,
    pub set_cookie: Option<HeaderValue>,
    pub personal: bool,
}

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
    parse_query(q).into_iter().collect()
}

impl RequestMode {
    /// The signed-in visitor, if any.
    pub async fn session(&self, headers: &HeaderMap) -> Option<Session> {
        #[cfg(feature = "auth")]
        if let Some(a) = &self.auth {
            return a.signed_in(headers).await;
        }
        let _ = headers;
        None
    }

    /// What templates see as `user`.
    #[allow(unused_variables)]
    pub fn user_value(session: Option<&Session>) -> Option<Value> {
        #[cfg(feature = "auth")]
        {
            session.map(|s| {
                context! {
                    id => s.user.id, name => s.user.display_name(), full_name => s.user.name,
                    email => s.user.email, roles => s.access.roles,
                    admin => s.access.can_use_admin(),
                    // For forms on personal pages (e.g. sign out); these
                    // pages are never written to disk, and fragments never
                    // see the token.
                    csrf => s.csrf,
                }
            })
        }
        #[cfg(not(feature = "auth"))]
        {
            None
        }
    }

    #[allow(unused_variables)]
    fn session_csrf(session: Option<&Session>) -> Option<String> {
        #[cfg(feature = "auth")]
        {
            session.map(|s| s.csrf.clone())
        }
        #[cfg(not(feature = "auth"))]
        {
            None
        }
    }

    pub fn handler(self) -> RequestHandler {
        Arc::new(move |req: PageRequest| {
            let me = self.clone();
            Box::pin(async move {
                if req.method != Method::GET && req.method != Method::HEAD {
                    return None;
                }
                if req.path.starts_with("/_engine/") {
                    return None;
                }
                let session = me.session(&req.headers).await;
                let r = (me.current)();
                if session.is_none() && req.query.is_none() && !maybe_request_route(&r, &req.path) {
                    return None;
                }
                me.serve(r, req, session).await
            })
        })
    }

    /// Resolve and render one request. `None`: not a page (serve files).
    async fn serve(
        &self,
        r: Arc<Renderer>,
        req: PageRequest,
        session: Option<Session>,
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
            return Some(redirect(&format!(
                "/account/login/?next={}",
                url_encode(&next)
            )));
        }
        let page = self
            .render_route(
                r,
                route,
                &req.path,
                req.query.as_deref(),
                &req.headers,
                session,
                Value::UNDEFINED,
            )
            .await;
        Some(match page {
            Ok(p) => page_response(StatusCode::OK, p, needs_user),
            Err(e) => failed(&req.path, e),
        })
    }

    /// Render `route` for this visitor with `extra` merged into the
    /// context (e.g. `form_state`).
    #[allow(clippy::too_many_arguments)]
    async fn render_route(
        &self,
        r: Arc<Renderer>,
        route: engine_core::routes::Route,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        session: Option<Session>,
        extra: Value,
    ) -> anyhow::Result<Rendered> {
        let (csrf, set_cookie) = match Self::session_csrf(session.as_ref()) {
            Some(t) => (t, None),
            None => self.csrf.token(headers),
        };
        let personal = session.is_some();
        let ctx = merge(
            context! {
                user => Self::user_value(session.as_ref()),
                csrf => csrf,
                request => context! {
                    path => path,
                    query => query,
                    args => query.map(query_args).unwrap_or_default(),
                },
            },
            extra,
        );
        let result =
            tokio::task::spawn_blocking(move || fragments::track(|| r.render_with(&route, &ctx)))
                .await?;
        let ((html, _tags), stats) = (result.0?, result.1);
        Ok(Rendered {
            html,
            stats,
            set_cookie,
            personal,
        })
    }

    /// Render the page at `path` per request for this visitor, with
    /// `extra` in the context (forms showing errors). `None` if no route
    /// is there.
    #[cfg(feature = "forms")]
    pub async fn render_path(
        &self,
        path: &str,
        headers: &HeaderMap,
        extra: Value,
    ) -> Option<anyhow::Result<Rendered>> {
        let r = (self.current)();
        let rr = r.clone();
        let p = path.to_string();
        let route = match tokio::task::spawn_blocking(move || rr.resolve_path(&p)).await {
            Ok(Ok(Some(route))) => route,
            Ok(Ok(None)) => return None,
            Ok(Err(e)) => return Some(Err(e)),
            Err(e) => return Some(Err(anyhow::anyhow!("{e}"))),
        };
        if r.route_config(&route).is_some_and(|c| c.requires_auth())
            && self.session(headers).await.is_none()
        {
            return None;
        }
        let session = self.session(headers).await;
        Some(
            self.render_route(r, route, path, None, headers, session, extra)
                .await,
        )
    }
}

fn merge(base: Value, extra: Value) -> Value {
    if extra.is_undefined() || extra.is_none() {
        return base;
    }
    let mut m = engine_core::templates::value_map(&base);
    m.extend(engine_core::templates::value_map(&extra));
    Value::from(m)
}

fn redirect(to: &str) -> Response {
    let mut r = axum::response::Redirect::to(to).into_response();
    *r.status_mut() = StatusCode::SEE_OTHER;
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A rendered page as a response: never stored by shared caches.
pub fn page_response(status: StatusCode, p: Rendered, needs_user: bool) -> Response {
    let mut resp = (status, p.html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Personal pages must not be stored by shared caches; pages with a
    // per-visitor form token neither.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if p.personal || needs_user {
            "private, no-store"
        } else {
            "no-cache, private"
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
    if let Ok(v) =
        HeaderValue::from_str(&format!("hits={} misses={}", p.stats.hits, p.stats.misses))
    {
        h.insert("x-wrought-fragments", v);
    }
    if let Some(c) = p.set_cookie {
        h.append(header::SET_COOKIE, c);
    }
    resp
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
    }
}
