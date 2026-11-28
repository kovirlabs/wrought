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
//! `X-Hammerwright-Fragments: hits=2 misses=0`.

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
    /// The site's database: a page's queries share one connection
    /// ([`engine_db::Db::pinned`]).
    pub db: engine_db::Db,
    /// Renders running at once: as many as the database pool has
    /// connections (each render holds one), so requests beyond that wait
    /// here, without a thread or memory of their own, instead of each
    /// taking a blocking thread to wait for a connection.
    pub renders: Arc<tokio::sync::Semaphore>,
    /// The published files: while the database is unavailable, a page
    /// that has a published copy is served from it instead of a `503`.
    pub published: std::path::PathBuf,
    #[cfg(feature = "auth")]
    pub auth: Option<engine_auth::Auth>,
}

/// One rendered page.
pub struct Rendered {
    pub html: String,
    pub stats: RenderStats,
    pub set_cookie: Option<HeaderValue>,
    pub personal: bool,
    /// Strong ETag, for pages that are the same for every visitor (signed
    /// out, no form token): cacheable with revalidation.
    pub etag: Option<Arc<str>>,
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
    ///
    /// Resolving (which reads the page's item) and rendering run in one
    /// blocking task, on one database connection.
    async fn serve(
        &self,
        r: Arc<Renderer>,
        req: PageRequest,
        session: Option<Session>,
    ) -> Option<Response> {
        enum Outcome {
            NotPage,
            Login,
            Page(
                anyhow::Result<(String, Vec<engine_core::tags::CacheTag>, RenderStats)>,
                bool,
            ),
        }
        let personal = session.is_some();
        // A signed-out visitor revalidating a page whose ETag is known and
        // whose data has not changed since: 304, no render, no database.
        let key = match &req.query {
            Some(q) => format!("{}?{q}", req.path),
            None => req.path.clone(),
        };
        let fragments = r.fragments().cloned();
        if !personal
            && let Some(c) = &fragments
            && let Some(etag) = c.page_etag(&key)
            && engine_core::serve::none_match(&req.headers, &etag)
        {
            return Some(not_modified(&etag));
        }
        let epoch = fragments.as_ref().map(|c| c.epoch());
        let (csrf, new_cookie) = self.visitor_csrf(session.as_ref(), &req.headers);
        let ctx = Self::context(
            session.as_ref(),
            &csrf,
            &req.path,
            req.query.as_deref(),
            Value::UNDEFINED,
        );
        let signed_in = session.is_some();
        let has_query = req.query.is_some();
        let path = req.path.clone();
        let db = self.db.clone();
        let Ok(_permit) = self.renders.acquire().await else {
            return Some(failed(&req.path, anyhow::anyhow!("shutting down")));
        };
        let outcome = tokio::task::spawn_blocking(move || {
            db.pinned(|| {
                let route = match r.resolve_path(&path) {
                    Ok(Some(route)) => route,
                    Ok(None) => return Outcome::NotPage,
                    Err(e) => return Outcome::Page(Err(e), false),
                };
                let rc = r.route_config(&route);
                let always_static = rc.is_some_and(|c| c.always_static);
                let needs_user = rc.is_some_and(|c| c.requires_auth());
                let per_request = route.mode == RenderMode::Request
                    || (!always_static && (signed_in || has_query));
                if !per_request {
                    return Outcome::NotPage;
                }
                if needs_user && !signed_in {
                    return Outcome::Login;
                }
                let (html, stats) = fragments::track(|| r.render_with(&route, &ctx));
                Outcome::Page(html.map(|(h, tags)| (h, tags, stats)), needs_user)
            })
        })
        .await;
        match outcome {
            Ok(Outcome::NotPage) => None,
            Ok(Outcome::Login) => {
                let next = match &req.query {
                    Some(q) => format!("{}?{q}", req.path),
                    None => req.path.clone(),
                };
                Some(redirect(&format!(
                    "/account/login/?next={}",
                    url_encode(&next)
                )))
            }
            Ok(Outcome::Page(Ok((html, tags, stats)), needs_user)) => {
                let set_cookie = Self::cookie_if_used(new_cookie, &csrf, &html);
                let etag = shared_etag(personal || needs_user, &csrf, &html);
                if let (Some(etag), Some(c), Some(epoch)) = (&etag, &fragments, epoch) {
                    c.remember_page(key, etag.clone(), tags, epoch);
                }
                if let Some(etag) = &etag
                    && engine_core::serve::none_match(&req.headers, etag)
                {
                    return Some(not_modified(etag));
                }
                Some(page_response(
                    StatusCode::OK,
                    Rendered {
                        html,
                        stats,
                        set_cookie,
                        personal,
                        etag,
                    },
                    needs_user,
                ))
            }
            // By the error's chain: the health flag alone is not enough,
            // the database can be back by the time the render has failed.
            Ok(Outcome::Page(Err(e), _)) if unavailable_cause(&e, self.db.health().is_down()) => {
                eprintln!("request render {}: {e:#}", req.path);
                self.unavailable(&req.path)
            }
            Ok(Outcome::Page(Err(e), _)) => Some(failed(&req.path, e)),
            Err(e) => Some(failed(&req.path, anyhow::anyhow!("{e}"))),
        }
    }

    /// The answer while the database is unavailable: `None` (serve the
    /// published copy) if there is one, else `503` with `Retry-After`.
    fn unavailable(&self, path: &str) -> Option<Response> {
        unavailable_answer(&self.published, path)
    }

    /// The visitor's form token: the session's, or a signed-out visitor's
    /// double-submit cookie (with the cookie to set when it is new).
    fn visitor_csrf(
        &self,
        session: Option<&Session>,
        headers: &HeaderMap,
    ) -> (String, Option<HeaderValue>) {
        match Self::session_csrf(session) {
            Some(t) => (t, None),
            None => self.csrf.token(headers),
        }
    }

    /// A new visitor's token cookie, set only if the page carries the token
    /// (a form): a page without one gets no cookie, so it stays the same
    /// for every visitor.
    fn cookie_if_used(cookie: Option<HeaderValue>, csrf: &str, html: &str) -> Option<HeaderValue> {
        cookie.filter(|_| html.contains(csrf))
    }

    /// The context of a request render: `user`, `csrf`, `request`, plus
    /// `extra`.
    fn context(
        session: Option<&Session>,
        csrf: &str,
        path: &str,
        query: Option<&str>,
        extra: Value,
    ) -> Value {
        merge(
            context! {
                user => Self::user_value(session),
                csrf => csrf,
                request => context! {
                    path => path,
                    query => query,
                    args => query.map(query_args).unwrap_or_default(),
                },
            },
            extra,
        )
    }

    /// Render `route` for this visitor with `extra` merged into the
    /// context (e.g. `form_state`).
    #[cfg(feature = "forms")]
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
        let (csrf, new_cookie) = self.visitor_csrf(session.as_ref(), headers);
        let personal = session.is_some();
        let ctx = Self::context(session.as_ref(), &csrf, path, query, extra);
        let db = self.db.clone();
        let _permit = self.renders.acquire().await?;
        let result = tokio::task::spawn_blocking(move || {
            db.pinned(|| fragments::track(|| r.render_with(&route, &ctx)))
        })
        .await?;
        let ((html, _tags), stats) = (result.0?, result.1);
        let set_cookie = Self::cookie_if_used(new_cookie, &csrf, &html);
        // Re-rendered with a form's state (errors, values): never cached.
        Ok(Rendered {
            html,
            stats,
            set_cookie,
            personal,
            etag: None,
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

/// The ETag of a page that is the same for every visitor: signed out and
/// without the visitor's form token. `None` for anything personal.
fn shared_etag(personal: bool, csrf: &str, html: &str) -> Option<Arc<str>> {
    if personal || (!csrf.is_empty() && html.contains(csrf)) {
        return None;
    }
    let h = engine_core::builder::sha256_hex(html.as_bytes());
    Some(format!("\"r{}\"", &h[..32]).into())
}

/// `304 Not Modified` for a page whose ETag the browser already has.
fn not_modified(etag: &str) -> Response {
    let mut resp = StatusCode::NOT_MODIFIED.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::VARY, HeaderValue::from_static("Cookie"));
    resp
}

/// A rendered page as a response. Pages the same for every visitor are
/// `no-cache` with an ETag (kept, revalidated on every use; a CDN may keep
/// them too, as `Vary: Cookie` separates signed-in visitors); personal
/// pages and pages with a visitor's form token are `private, no-store`.
pub fn page_response(status: StatusCode, p: Rendered, needs_user: bool) -> Response {
    let mut resp = (status, p.html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    let shared = status == StatusCode::OK && !p.personal && !needs_user && p.etag.is_some();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if shared {
            "no-cache"
        } else {
            "private, no-store"
        }),
    );
    if shared
        && let Some(v) = p
            .etag
            .as_deref()
            .and_then(|t| HeaderValue::from_str(t).ok())
    {
        h.insert(header::ETAG, v);
    }
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
        h.insert("x-hammerwright-fragments", v);
    }
    if let Some(c) = p.set_cookie {
        h.append(header::SET_COOKIE, c);
    }
    resp
}

/// Whether a failed render failed because the database is unavailable:
/// by the error's chain (a template function keeps the query's error), or
/// the database is known to be down. The flag alone is not enough: it can
/// say "up" again by the time the render has failed.
fn unavailable_cause(e: &anyhow::Error, down: bool) -> bool {
    engine_db::health::is_unavailable_any(e) || down
}

/// `None` (serve the published copy of `path`) if `published` has one, else
/// `503` with `Retry-After`.
fn unavailable_answer(published: &std::path::Path, path: &str) -> Option<Response> {
    let rel = path.trim_start_matches('/');
    let has_copy = !rel.split('/').any(|c| c == "..")
        && (published.join(rel).join("index.html").is_file()
            || (!rel.is_empty() && published.join(rel).is_file()));
    if has_copy {
        return None;
    }
    Some(engine_core::serve::unavailable(
        engine_core::serve::UNAVAILABLE_PAGE,
    ))
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

    /// The outage test's intermittent 500: a page whose template function
    /// (`list_items`) found the database unavailable, decided after the
    /// health flag already said "up" again. Forced: the function fails with
    /// the unavailable error while nothing is down.
    #[test]
    fn template_function_unavailable_is_503_or_the_published_copy() {
        let mut env = minijinja::Environment::new();
        env.add_function(
            "list_items",
            |_: String| -> Result<Value, minijinja::Error> {
                Err(engine_core::templates::fn_error(
                    engine_db::health::unavailable(),
                ))
            },
        );
        env.add_template(
            "home.html",
            "<ul>{% for u in list_items(\"update\") %}<li>{{ u }}</li>{% endfor %}</ul>",
        )
        .unwrap();
        let e = env
            .get_template("home.html")
            .and_then(|t| t.render(()))
            .map_err(engine_core::templates::render_error)
            .map_err(|e| e.context("rendering / with home.html"))
            .unwrap_err();
        assert!(unavailable_cause(&e, false), "{e:#}");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("home.html") && msg.contains("the database is unavailable"),
            "{msg}"
        );
        // Not every error is "unavailable".
        let other = anyhow::anyhow!("no such template");
        assert!(!unavailable_cause(&other, false));

        let dir = tempfile::tempdir().unwrap();
        let r = unavailable_answer(dir.path(), "/latest/").expect("no copy: 503");
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()[header::RETRY_AFTER], "30");
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        std::fs::write(dir.path().join("index.html"), "home").unwrap();
        assert!(
            unavailable_answer(dir.path(), "/").is_none(),
            "published copy"
        );
        assert!(unavailable_answer(dir.path(), "/../etc/").is_some());
    }

    #[test]
    fn query_args_decode() {
        let a = query_args("page=2&q=a+b%21&flag&=x");
        assert_eq!(a["page"], "2");
        assert_eq!(a["q"], "a b!");
        assert_eq!(a["flag"], "");
        assert_eq!(a.len(), 3);
    }
}
