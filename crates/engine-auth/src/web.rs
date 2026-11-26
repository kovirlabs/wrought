//! HTTP side of accounts: cookies, CSRF, client addresses, the engine's own
//! HTML pages ([`Ui`]) and the routes under `/account/`:
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET/POST /account/login/` | sign in (email + password) |
//! | `GET/POST /account/totp/` | second step when two-factor sign-in is on |
//! | `POST /account/logout/` | sign out |
//! | `GET/POST /account/reset/` | ask for a password-reset link |
//! | `GET/POST /account/reset/confirm/?token=` | set a new password |
//! | `GET /_engine/ui/<file>` | bundled CSS/JS for these pages and the admin |
//!
//! CSRF: every form carries `_csrf`. Signed-in forms use the session's
//! token; signed-out forms (sign-in, reset) use a double-submit cookie.
//! Every POST is also refused when its `Origin` header names another host.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use include_dir::{Dir, include_dir};
use minijinja::{AutoEscape, Environment, Value, context};
use serde::Deserialize;

use crate::accounts::{LoginResult, Session, Stage};
use crate::{Auth, AuthError, secret};

/// Templates of the sign-in pages and the shared layout.
pub static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// URL prefix of engine UI assets.
pub const ASSETS: &str = "/_engine/ui/";

/// A file served under [`ASSETS`].
#[derive(Debug, Clone, Copy)]
pub struct UiAsset {
    pub name: &'static str,
    pub body: &'static [u8],
    pub content_type: &'static str,
}

/// The engine's own pages: a minijinja environment over embedded template
/// folders (later folders override earlier ones) plus the assets they use.
pub struct Ui {
    env: Environment<'static>,
    assets: Vec<UiAsset>,
    site_name: String,
}

fn tagbrush() -> UiAsset {
    let a = engine_core::ui::TAGBRUSH_CSS;
    UiAsset {
        name: a.path.rsplit('/').next().unwrap_or("tagbrush.css"),
        body: a.body,
        content_type: "text/css; charset=utf-8",
    }
}

fn tagaction() -> UiAsset {
    let a = engine_core::ui::TAGACTION_JS;
    UiAsset {
        name: a.path.rsplit('/').next().unwrap_or("tagaction.js"),
        body: a.body,
        content_type: "text/javascript; charset=utf-8",
    }
}

impl Ui {
    /// `extra`: more template folders (the admin's), searched first.
    pub fn new(site_name: &str, extra: &[&'static Dir<'static>], assets: &[UiAsset]) -> Self {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        env.set_formatter(|out, state, value| {
            if value.is_none() || value.is_undefined() {
                return Ok(());
            }
            let err = |_| minijinja::Error::new(minijinja::ErrorKind::WriteFailure, "format");
            if !matches!(state.auto_escape(), AutoEscape::None) && !value.is_safe() {
                let s = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                engine_core::templates::escape_html(&s, out).map_err(err)
            } else {
                write!(out, "{value}").map_err(err)
            }
        });
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        let dirs: Vec<&'static Dir<'static>> = extra.iter().copied().chain([&TEMPLATES]).collect();
        env.set_loader(move |name| {
            if engine_core::templates::validate_template_name(name).is_err() {
                return Ok(None);
            }
            Ok(dirs
                .iter()
                .find_map(|d| d.get_file(name))
                .and_then(|f| f.contents_utf8())
                .map(str::to_string))
        });
        env.add_filter("date", |s: String| {
            // `2026-11-20T10:00:00Z` -> `2026-11-20 10:00 UTC`
            match s.get(..16) {
                Some(d) if s.len() >= 16 => format!("{} UTC", d.replace('T', " ")),
                _ => s,
            }
        });
        let mut all = vec![tagbrush(), tagaction()];
        all.extend_from_slice(assets);
        let css = format!("{ASSETS}{}", all[0].name);
        let js = format!("{ASSETS}{}", all[1].name);
        let extra_js: Vec<String> = all[2..]
            .iter()
            .filter(|a| a.content_type.starts_with("text/javascript"))
            .map(|a| format!("{ASSETS}{}", a.name))
            .collect();
        env.add_global(
            "ui",
            context! { css => css, js => js, extra_js => extra_js },
        );
        env.add_global("site_name", site_name.to_string());
        Ui {
            env,
            assets: all,
            site_name: site_name.to_string(),
        }
    }

    pub fn site_name(&self) -> &str {
        &self.site_name
    }

    pub fn env(&self) -> &Environment<'static> {
        &self.env
    }

    /// Render a template.
    pub fn render(&self, name: &str, ctx: Value) -> Result<String, minijinja::Error> {
        self.env.get_template(name).and_then(|t| t.render(ctx))
    }

    /// A full page response with the engine UI's security headers. A
    /// template failure becomes a plain 500 (and is logged).
    pub fn page(&self, status: StatusCode, name: &str, ctx: Value) -> Response {
        match self.render(name, ctx) {
            Ok(html) => html_response(status, html),
            Err(e) => {
                eprintln!("engine UI: rendering {name}: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal error\n").into_response()
            }
        }
    }

    fn asset(&self, name: &str) -> Option<&UiAsset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// Headers on every engine page: no caching, no framing, a strict CSP
/// (scripts only from this origin, no inline scripts).
pub fn secure_headers(h: &mut HeaderMap) {
    for (k, v) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; \
             object-src 'none'",
        ),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
}

pub fn html_response(status: StatusCode, html: String) -> Response {
    let mut r = (status, html).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    secure_headers(r.headers_mut());
    r
}

/// 303 See Other to a site path.
pub fn see_other(to: &str) -> Response {
    let mut r = Redirect::to(to).into_response();
    *r.status_mut() = StatusCode::SEE_OTHER;
    secure_headers(r.headers_mut());
    r
}

/// The value of cookie `name`.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
}

/// A `Set-Cookie` value. `max_age` 0 deletes the cookie.
pub fn set_cookie(
    name: &str,
    value: &str,
    max_age: u64,
    secure: bool,
    same_site: &str,
) -> HeaderValue {
    let mut s =
        format!("{name}={value}; Path=/; HttpOnly; SameSite={same_site}; Max-Age={max_age}");
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).expect("cookie values are tokens")
}

/// Who is asking: peer address, forwarded-for header, user agent.
#[derive(Debug, Clone, Default)]
pub struct Client {
    pub peer: Option<IpAddr>,
    pub forwarded_for: Option<String>,
    pub user_agent: String,
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let text = |n: header::HeaderName| {
            parts
                .headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        Ok(Client {
            peer: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
            forwarded_for: text(header::HeaderName::from_static("x-forwarded-for")),
            user_agent: text(header::USER_AGENT).unwrap_or_default(),
        })
    }
}

impl Auth {
    /// The client's address for rate limits: the peer, or behind a proxy
    /// (`[auth] behind_proxy`) the last `X-Forwarded-For` entry.
    pub fn client_ip(&self, c: &Client) -> Option<String> {
        if self.config().behind_proxy
            && let Some(last) = c
                .forwarded_for
                .as_deref()
                .and_then(|f| f.rsplit(',').next())
                .map(str::trim)
                .and_then(|s| s.parse::<IpAddr>().ok())
        {
            return Some(last.to_string());
        }
        c.peer.map(|p| p.to_string())
    }

    /// The session named by the request's cookie, if valid.
    pub async fn session_from(&self, headers: &HeaderMap) -> Option<Session> {
        let token = cookie(headers, self.config().session_cookie())?;
        match self.session(&token).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("auth: reading session: {e:#}");
                None
            }
        }
    }

    /// A signed-in (both factors) session, or `None`.
    pub async fn signed_in(&self, headers: &HeaderMap) -> Option<Session> {
        self.session_from(headers).await.filter(Session::is_full)
    }

    pub fn session_cookie(&self, token: &str) -> HeaderValue {
        let cfg = self.config();
        set_cookie(
            cfg.session_cookie(),
            token,
            u64::from(cfg.session_hours) * 3600,
            cfg.cookie_secure,
            "Lax",
        )
    }

    pub fn clear_session_cookie(&self) -> HeaderValue {
        let cfg = self.config();
        set_cookie(cfg.session_cookie(), "", 0, cfg.cookie_secure, "Lax")
    }
}

/// Whether a POST's `Origin` (when sent) is this host. Browsers send it on
/// every cross-origin POST, so this stops cross-site form posts even before
/// the CSRF token is checked.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match origin.split_once("://") {
        Some((_, rest)) => !host.is_empty() && rest.eq_ignore_ascii_case(host),
        None => false,
    }
}

/// A submitted form, as ordered name/value pairs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct FormData(pub Vec<(String, String)>);

impl FormData {
    /// The last value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k == name)
    }
}

/// Response for a failed CSRF or origin check.
pub fn forbidden_csrf(ui: &Ui) -> Response {
    ui.page(
        StatusCode::FORBIDDEN,
        "engine/message.html",
        context! {
            title => "Form expired",
            message => "This form was not accepted: it is missing its security token, the token \
                        is out of date, or it was sent from another site. Go back, reload the \
                        page and try again.",
        },
    )
}

/// Check a signed-in form's CSRF token (and origin).
pub fn csrf_ok(session: &Session, headers: &HeaderMap, form: &FormData) -> bool {
    same_origin(headers)
        && form
            .get("_csrf")
            .or_else(|| headers.get("x-csrf-token").and_then(|v| v.to_str().ok()))
            .is_some_and(|t| secret::same(t, &session.csrf))
}

/// A site path to go to after signing in: only same-site paths.
pub fn safe_next(next: Option<&str>, default: &str) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.contains('\\')
                && !n.chars().any(|c| c.is_control() || c.is_whitespace())
                && !n.starts_with("/account/") =>
        {
            n.to_string()
        }
        _ => default.to_string(),
    }
}

/// State of the `/account/` routes.
#[derive(Clone)]
pub struct Web {
    pub auth: Auth,
    pub ui: Arc<Ui>,
    /// Where to go after signing in when no `next` is given.
    pub home: String,
}

#[derive(Deserialize, Default)]
struct NextQuery {
    next: Option<String>,
    token: Option<String>,
    done: Option<String>,
}

/// The signed-out CSRF token: the cookie's, or a new one to set.
fn anon_csrf(web: &Web, headers: &HeaderMap) -> (String, Option<HeaderValue>) {
    let cfg = web.auth.config();
    match cookie(headers, cfg.csrf_cookie()).filter(|t| secret::is_token(t)) {
        Some(t) => (t, None),
        None => {
            let t = secret::token();
            let c = set_cookie(cfg.csrf_cookie(), &t, 4 * 3600, cfg.cookie_secure, "Strict");
            (t, Some(c))
        }
    }
}

fn anon_csrf_ok(web: &Web, headers: &HeaderMap, form: &FormData) -> bool {
    let Some(c) = cookie(headers, web.auth.config().csrf_cookie()) else {
        return false;
    };
    same_origin(headers)
        && secret::is_token(&c)
        && form.get("_csrf").is_some_and(|f| secret::same(f, &c))
}

fn with_cookies(mut r: Response, cookies: impl IntoIterator<Item = HeaderValue>) -> Response {
    for c in cookies {
        r.headers_mut().append(header::SET_COOKIE, c);
    }
    r
}

/// The `/account/` routes and `/_engine/ui/` assets.
pub fn router(web: Web) -> Router {
    Router::new()
        .route("/account/login/", get(login_page).post(login_post))
        .route("/account/totp/", get(totp_page).post(totp_post))
        .route("/account/logout/", post(logout))
        .route("/account/reset/", get(reset_page).post(reset_post))
        .route(
            "/account/reset/confirm/",
            get(reset_confirm_page).post(reset_confirm_post),
        )
        .route(
            "/account/login",
            get(|| async { see_other("/account/login/") }),
        )
        .route("/_engine/ui/{file}", get(asset))
        .with_state(web)
}

async fn asset(
    State(web): State<Web>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> Response {
    match web.ui.asset(&file) {
        Some(a) => (
            [
                (header::CONTENT_TYPE, a.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            a.body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not found\n").into_response(),
    }
}

fn login_form(
    web: &Web,
    headers: &HeaderMap,
    status: StatusCode,
    next: &str,
    email: &str,
    error: Option<&str>,
    notice: Option<&str>,
) -> Response {
    let (csrf, set) = anon_csrf(web, headers);
    let r = web.ui.page(
        status,
        "engine/login.html",
        context! { title => "Sign in", csrf, next, email, error, notice },
    );
    with_cookies(r, set)
}

async fn login_page(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let next = safe_next(q.next.as_deref(), &web.home);
    if web.auth.signed_in(&headers).await.is_some() {
        return see_other(&next);
    }
    let notice = match q.done.as_deref() {
        Some("reset") => Some("Your password was changed. Sign in with the new one."),
        Some("logout") => Some("You are signed out."),
        _ => None,
    };
    login_form(&web, &headers, StatusCode::OK, &next, "", None, notice)
}

async fn login_post(
    State(web): State<Web>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    if !anon_csrf_ok(&web, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    let next = safe_next(form.get("next"), &web.home);
    let email = form.get("email").unwrap_or("").trim().to_string();
    let password = form.get("password").unwrap_or("");
    let ip = web.auth.client_ip(&client);
    let result = match web.auth.login(&email, password, ip.as_deref()).await {
        Ok(r) => r,
        Err(e) => return server_error(&web, e),
    };
    match result {
        LoginResult::Ok(user) => {
            let stage = if user.totp_enabled {
                Stage::Totp
            } else {
                Stage::Full
            };
            let token = match web
                .auth
                .start_session(user.id, stage, ip.as_deref(), &client.user_agent)
                .await
            {
                Ok(t) => t,
                Err(e) => return server_error(&web, e),
            };
            let to = if stage == Stage::Totp {
                format!("/account/totp/?next={}", url_encode(&next))
            } else {
                next
            };
            with_cookies(see_other(&to), [web.auth.session_cookie(&token)])
        }
        LoginResult::Invalid => login_form(
            &web,
            &headers,
            StatusCode::UNAUTHORIZED,
            &next,
            &email,
            Some("The email address or password is not right."),
            None,
        ),
        LoginResult::Limited => login_form(
            &web,
            &headers,
            StatusCode::TOO_MANY_REQUESTS,
            &next,
            &email,
            Some(
                "Too many failed attempts. Wait 15 minutes and try again, or reset your password.",
            ),
            None,
        ),
    }
}

/// Percent-encode a query value.
pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn server_error(web: &Web, e: anyhow::Error) -> Response {
    eprintln!("auth: {e:#}");
    web.ui.page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "engine/message.html",
        context! { title => "Something went wrong", message => "The server could not finish this request. Try again in a moment." },
    )
}

async fn totp_page(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let next = safe_next(q.next.as_deref(), &web.home);
    match web.auth.session_from(&headers).await {
        Some(s) if s.stage == Stage::Totp => web.ui.page(
            StatusCode::OK,
            "engine/totp.html",
            context! { title => "Two-factor sign-in", csrf => s.csrf, next, error => () },
        ),
        Some(_) => see_other(&next),
        None => see_other("/account/login/"),
    }
}

async fn totp_post(
    State(web): State<Web>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let next = safe_next(form.get("next"), &web.home);
    let Some(s) = web.auth.session_from(&headers).await else {
        return see_other("/account/login/");
    };
    if !csrf_ok(&s, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    if s.stage != Stage::Totp {
        return see_other(&next);
    }
    let ip = web.auth.client_ip(&client);
    let code = form.get("code").unwrap_or("");
    let res = match web.auth.check_totp(&s, code, ip.as_deref()).await {
        Ok(r) => r,
        Err(e) => return server_error(&web, e),
    };
    let error = match res {
        LoginResult::Ok(user) => {
            // Replace the half session with a full one (new token).
            let _ = web.auth.end_session(&s).await;
            let token = match web
                .auth
                .start_session(user.id, Stage::Full, ip.as_deref(), &client.user_agent)
                .await
            {
                Ok(t) => t,
                Err(e) => return server_error(&web, e),
            };
            return with_cookies(see_other(&next), [web.auth.session_cookie(&token)]);
        }
        LoginResult::Invalid => "That code is not right. Use the current code from your app.",
        LoginResult::Limited => "Too many failed attempts. Wait 15 minutes and sign in again.",
    };
    let status = if matches!(res, LoginResult::Limited) {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::UNAUTHORIZED
    };
    web.ui.page(
        status,
        "engine/totp.html",
        context! { title => "Two-factor sign-in", csrf => s.csrf, next, error },
    )
}

async fn logout(
    State(web): State<Web>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    if let Some(s) = web.auth.session_from(&headers).await {
        if !csrf_ok(&s, &headers, &form) {
            return forbidden_csrf(&web.ui);
        }
        if let Err(e) = web.auth.end_session(&s).await {
            return server_error(&web, e);
        }
    }
    with_cookies(
        see_other("/account/login/?done=logout"),
        [web.auth.clear_session_cookie()],
    )
}

async fn reset_page(State(web): State<Web>, headers: HeaderMap) -> Response {
    let (csrf, set) = anon_csrf(&web, &headers);
    with_cookies(
        web.ui.page(
            StatusCode::OK,
            "engine/reset_request.html",
            context! { title => "Reset your password", csrf, sent => false },
        ),
        set,
    )
}

async fn reset_post(
    State(web): State<Web>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    if !anon_csrf_ok(&web, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    let email = form.get("email").unwrap_or("").trim();
    let ip = web.auth.client_ip(&client);
    if !email.is_empty()
        && let Err(e) = web.auth.request_reset(email, ip.as_deref()).await
    {
        return server_error(&web, e);
    }
    let (csrf, _) = anon_csrf(&web, &headers);
    web.ui.page(
        StatusCode::OK,
        "engine/reset_request.html",
        context! { title => "Reset your password", csrf, sent => true },
    )
}

async fn reset_confirm_page(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let token = q.token.unwrap_or_default();
    let user = match web.auth.reset_user(&token).await {
        Ok(u) => u,
        Err(e) => return server_error(&web, e),
    };
    if user.is_none() {
        return invalid_reset(&web);
    }
    let (csrf, set) = anon_csrf(&web, &headers);
    with_cookies(
        web.ui.page(
            StatusCode::OK,
            "engine/reset_confirm.html",
            context! { title => "Choose a new password", csrf, token, errors => () },
        ),
        set,
    )
}

fn invalid_reset(web: &Web) -> Response {
    web.ui.page(
        StatusCode::BAD_REQUEST,
        "engine/message.html",
        context! {
            title => "Link not valid",
            message => "This password-reset link is invalid, was already used, or has expired.",
            link => context! { url => "/account/reset/", label => "Ask for a new link" },
        },
    )
}

async fn reset_confirm_post(
    State(web): State<Web>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    if !anon_csrf_ok(&web, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    let token = form.get("token").unwrap_or("").to_string();
    let password = form.get("password").unwrap_or("");
    if form.get("password_again") != Some(password) {
        let (csrf, _) = anon_csrf(&web, &headers);
        return web.ui.page(
            StatusCode::UNPROCESSABLE_ENTITY,
            "engine/reset_confirm.html",
            context! { title => "Choose a new password", csrf, token,
            errors => BTreeMap::from([("password_again", "does not match")]) },
        );
    }
    match web.auth.reset_password(&token, password).await {
        Ok(_) => see_other("/account/login/?done=reset"),
        Err(AuthError::Invalid(errs)) if errs.iter().any(|(f, _)| f == "token") => {
            invalid_reset(&web)
        }
        Err(AuthError::Invalid(errs)) => {
            let (csrf, _) = anon_csrf(&web, &headers);
            let errors: BTreeMap<String, String> = errs.into_iter().collect();
            web.ui.page(
                StatusCode::UNPROCESSABLE_ENTITY,
                "engine/reset_confirm.html",
                context! { title => "Choose a new password", csrf, token, errors },
            )
        }
        Err(e) => server_error(&web, anyhow::anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_paths_stay_on_site() {
        assert_eq!(safe_next(Some("/admin/x/"), "/"), "/admin/x/");
        for bad in [
            "//evil.example",
            "https://evil.example",
            "/\\evil",
            "/a b",
            "javascript:x",
            "/account/logout/",
        ] {
            assert_eq!(safe_next(Some(bad), "/home/"), "/home/", "{bad}");
        }
        assert_eq!(safe_next(None, "/admin/"), "/admin/");
    }

    #[test]
    fn cookies_and_origin() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-wrought_session=tok; b=2"),
        );
        assert_eq!(cookie(&h, "__Host-wrought_session").as_deref(), Some("tok"));
        assert_eq!(cookie(&h, "c"), None);
        let c = set_cookie("n", "v", 60, true, "Lax");
        let c = c.to_str().unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age=60"] {
            assert!(c.contains(part), "{c}");
        }
        h.insert(header::HOST, HeaderValue::from_static("site.example"));
        assert!(same_origin(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://site.example"),
        );
        assert!(same_origin(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!same_origin(&h));
        h.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(!same_origin(&h));
    }

    #[test]
    fn engine_templates_render() {
        let ui = Ui::new("Site <x>", &[], &[]);
        let html = ui
            .render(
                "engine/login.html",
                context! { title => "Sign in", csrf => "t", next => "/admin/", email => "<b>", error => "bad" },
            )
            .unwrap();
        assert!(html.contains("Site &lt;x&gt;"), "{html}");
        assert!(html.contains("value=\"&lt;b&gt;\""), "{html}");
        assert!(html.contains("name=\"_csrf\" value=\"t\""), "{html}");
        assert!(
            html.contains("/_engine/ui/tagbrush-2.0.1.min.css"),
            "{html}"
        );
    }
}
