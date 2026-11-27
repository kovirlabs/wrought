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
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
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
    let a = *engine_core::ui::TAGBRUSH_CSS;
    UiAsset {
        name: a.path.rsplit('/').next().unwrap_or("tagbrush.css"),
        body: a.body,
        content_type: "text/css; charset=utf-8",
    }
}

fn tagaction() -> UiAsset {
    let a = *engine_core::ui::TAGACTION_JS;
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
        // Classic scripts every page loads; `.mjs` modules are loaded only
        // by the page that needs them (the visual editor).
        let extra_js: Vec<String> = all[2..]
            .iter()
            .filter(|a| a.content_type.starts_with("text/javascript") && !a.name.ends_with(".mjs"))
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
///
/// `Referrer-Policy: same-origin`, not `no-referrer`: under `no-referrer`
/// browsers send `Origin: null` with form posts, which the origin check
/// (rightly) refuses, so signing in from a real browser failed. With
/// `same-origin` no address (with its reset token) leaves the site.
pub fn secure_headers(h: &mut HeaderMap) {
    for (k, v) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "same-origin"),
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

pub use engine_core::web::{Client, FormData, cookie, same_origin, set_cookie, url_encode};

impl Auth {
    /// The client's address for rate limits: the peer, or behind a proxy
    /// (`[auth] behind_proxy`) the last `X-Forwarded-For` entry.
    pub fn client_ip(&self, c: &Client) -> Option<String> {
        c.ip(self.config().behind_proxy)
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
pub(crate) fn anon_csrf(web: &Web, headers: &HeaderMap) -> (String, Option<HeaderValue>) {
    web.auth.anon_csrf().token(headers)
}

pub(crate) fn anon_csrf_ok(web: &Web, headers: &HeaderMap, form: &FormData) -> bool {
    web.auth.anon_csrf().check(headers, form)
}

pub(crate) fn with_cookies(
    mut r: Response,
    cookies: impl IntoIterator<Item = HeaderValue>,
) -> Response {
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
        .route("/account/register/", get(register_page).post(register_post))
        .route("/account/verify/", get(verify_page))
        .route("/account/profile/", get(profile_page).post(profile_post))
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
    let registration = web.auth.config().registration == crate::Registration::Open;
    let oidc = sso_buttons(&web.auth);
    let r = web.ui.page(
        status,
        "engine/login.html",
        context! { title => "Sign in", csrf, next, email, error, notice, registration, oidc },
    );
    with_cookies(allow_sso_form_action(&web.auth, r), set)
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
        Some("verified") => Some("Your email address is confirmed. Sign in to continue."),
        _ => None,
    };
    login_form(&web, &headers, StatusCode::OK, &next, "", None, notice)
}

/// `/account/verify/?token=...`: the link from the welcome message.
async fn verify_page(State(web): State<Web>, Query(q): Query<NextQuery>) -> Response {
    match web
        .auth
        .verify_email(q.token.as_deref().unwrap_or(""))
        .await
    {
        Ok(Some(_)) => see_other("/account/login/?done=verified"),
        Ok(None) => web.ui.page(
            StatusCode::BAD_REQUEST,
            "engine/message.html",
            context! {
                title => "Link not valid",
                message => "This confirmation link is not valid: it was used already or it \
                            expired. Sign in to get a new one.",
            },
        ),
        Err(e) => server_error(&web, e),
    }
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
    let explicit_next = form.get("next").filter(|n| !n.is_empty());
    let mut next = safe_next(explicit_next, &web.home);
    let email = form.get("email").unwrap_or("").trim().to_string();
    let password = form.get("password").unwrap_or("");
    let ip = web.auth.client_ip(&client);
    let result = match web.auth.login(&email, password, ip.as_deref()).await {
        Ok(r) => r,
        Err(e) => return server_error(&web, e),
    };
    match result {
        LoginResult::Ok(user) => {
            // Members without admin rights land on the site, not the admin.
            if explicit_next.is_none()
                && next.starts_with("/admin/")
                && !web
                    .auth
                    .access(&user)
                    .await
                    .is_ok_and(|a| a.can_use_admin())
            {
                next = "/".into();
            }
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
        LoginResult::Unverified(user) => {
            if let Err(e) = web.auth.resend_verification(&user).await {
                return server_error(&web, e);
            }
            login_form(
                &web,
                &headers,
                StatusCode::FORBIDDEN,
                &next,
                &email,
                Some(
                    "Confirm your email address first: we sent you a link (check your spam \
                     folder too). Follow it, then sign in.",
                ),
                None,
            )
        }
    }
}

pub(crate) fn server_error(web: &Web, e: anyhow::Error) -> Response {
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
        LoginResult::Invalid | LoginResult::Unverified(_) => {
            "That code is not right. Use the current code from your app."
        }
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

fn registration_open(web: &Web) -> bool {
    web.auth.config().registration == crate::Registration::Open
}

fn not_found(web: &Web) -> Response {
    web.ui.page(
        StatusCode::NOT_FOUND,
        "engine/message.html",
        context! { title => "Not found", message => "There is no such page." },
    )
}

#[allow(clippy::too_many_arguments)]
fn register_form(
    web: &Web,
    headers: &HeaderMap,
    status: StatusCode,
    next: &str,
    name: &str,
    email: &str,
    error: Option<&str>,
    errors: BTreeMap<String, String>,
) -> Response {
    let (csrf, set) = anon_csrf(web, headers);
    with_cookies(
        web.ui.page(
            status,
            "engine/register.html",
            context! { title => "Create an account", csrf, next, name, email, error, errors },
        ),
        set,
    )
}

async fn register_page(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    if !registration_open(&web) {
        return not_found(&web);
    }
    let next = safe_next(q.next.as_deref(), "/");
    if web.auth.signed_in(&headers).await.is_some() {
        return see_other(&next);
    }
    register_form(
        &web,
        &headers,
        StatusCode::OK,
        &next,
        "",
        "",
        None,
        BTreeMap::new(),
    )
}

async fn register_post(
    State(web): State<Web>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    if !registration_open(&web) {
        return not_found(&web);
    }
    if !anon_csrf_ok(&web, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    let next = safe_next(form.get("next"), "/");
    let name = form.get("name").unwrap_or("").trim().to_string();
    let email = form.get("email").unwrap_or("").trim().to_string();
    // Honeypot: people never see this field.
    if form.get("website").is_some_and(|w| !w.is_empty()) {
        eprintln!("auth: registration refused (honeypot filled)");
        return register_form(
            &web,
            &headers,
            StatusCode::UNPROCESSABLE_ENTITY,
            &next,
            &name,
            &email,
            Some("This form was not accepted."),
            BTreeMap::new(),
        );
    }
    let ip = web.auth.client_ip(&client);
    let password = form.get("password").unwrap_or("");
    match web
        .auth
        .register(&email, &name, password, ip.as_deref())
        .await
    {
        Ok(user) if !user.email_verified => web.ui.page(
            StatusCode::OK,
            "engine/message.html",
            context! {
                title => "Check your email",
                message => format!(
                    "We sent a link to {} to confirm the address. Follow it to finish \
                     creating your account, then sign in.",
                    user.email
                ),
            },
        ),
        Ok(user) => {
            let token = match web
                .auth
                .start_session(user.id, Stage::Full, ip.as_deref(), &client.user_agent)
                .await
            {
                Ok(t) => t,
                Err(e) => return server_error(&web, e),
            };
            with_cookies(see_other(&next), [web.auth.session_cookie(&token)])
        }
        Err(AuthError::Invalid(errs)) => register_form(
            &web,
            &headers,
            StatusCode::UNPROCESSABLE_ENTITY,
            &next,
            &name,
            &email,
            None,
            errs.into_iter().collect(),
        ),
        Err(AuthError::Limited) => register_form(
            &web,
            &headers,
            StatusCode::TOO_MANY_REQUESTS,
            &next,
            &name,
            &email,
            Some("Too many accounts were created from your network recently. Try again later."),
            BTreeMap::new(),
        ),
        Err(e) => server_error(&web, anyhow::anyhow!("{e}")),
    }
}

async fn profile_view(
    web: &Web,
    s: &Session,
    status: StatusCode,
    next: &str,
    notice: Option<&str>,
    errors: BTreeMap<String, String>,
) -> Response {
    // Two-factor sign-in for every account (members too): a fresh key is
    // offered until it is turned on.
    let (secret, uri, qr) = if s.user.totp_enabled || !web.auth.totp_available() {
        (None, None, None)
    } else {
        let secret = crate::totp::new_secret();
        let uri = crate::totp::uri(&secret, web.auth.site_name(), &s.user.email);
        let qr = crate::totp::qr_svg(&uri).map(minijinja::Value::from_safe_string);
        (Some(secret), Some(uri), qr)
    };
    let sso = match sso_view(&web.auth, s.user.id).await {
        Ok(v) => v,
        Err(e) => return server_error(web, e),
    };
    let has_password = match web.auth.has_password(s.user.id).await {
        Ok(v) => v,
        Err(e) => return server_error(web, e),
    };
    let r = web.ui.page(
        status,
        "engine/profile.html",
        context! {
            title => "Your profile", csrf => s.csrf, account => s.user, next, notice, errors,
            totp_secret => secret, totp_uri => uri, totp_qr => qr,
            totp_available => web.auth.totp_available(), sso, has_password,
        },
    );
    allow_sso_form_action(&web.auth, r)
}

/// The profile's single sign-on section: each configured provider with the
/// account's identity from it (if linked), then identities of providers no
/// longer configured.
async fn sso_view(auth: &Auth, user_id: i64) -> anyhow::Result<Vec<Value>> {
    let providers = auth.oidc_providers();
    if providers.is_empty() {
        return Ok(Vec::new());
    }
    let ids = auth.identities(user_id).await?;
    let mut out: Vec<Value> = providers
        .iter()
        .map(|p| {
            let linked = ids.iter().find(|i| i.issuer == p.issuer);
            context! { name => p.name, label => p.label, issuer => p.issuer, linked, configured => true }
        })
        .collect();
    for i in ids
        .iter()
        .filter(|i| !providers.iter().any(|p| p.issuer == i.issuer))
    {
        out.push(context! { name => i.provider, label => i.provider, issuer => i.issuer, linked => i, configured => false });
    }
    Ok(out)
}

/// Engine pages with single sign-on buttons let their forms lead to the
/// providers' origins (`form-action`): the button posts here, and the
/// answer redirects to the provider, which browsers check against the
/// submitting page's policy.
fn allow_sso_form_action(auth: &Auth, mut r: Response) -> Response {
    let origins: Vec<String> = auth
        .oidc_providers()
        .iter()
        .map(|p| p.issuer_origin())
        .collect();
    if origins.is_empty() {
        return r;
    }
    let Some(csp) = r
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return r;
    };
    let csp = csp.replacen(
        "form-action 'self'",
        &format!("form-action 'self' {}", origins.join(" ")),
        1,
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    r
}

/// The `[[auth.oidc]]` providers as template values (`name`, `label`).
pub fn sso_buttons(auth: &Auth) -> Vec<Value> {
    auth.oidc_providers()
        .iter()
        .map(|p| context! { name => p.name, label => p.label })
        .collect()
}

async fn profile_page(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let Some(s) = web.auth.signed_in(&headers).await else {
        return see_other("/account/login/?next=/account/profile/");
    };
    let notice = match q.done.as_deref() {
        Some("name") => Some("Your name was saved."),
        Some("password") => Some("Your password was changed. Other devices were signed out."),
        Some("totp-on") => Some("Two-factor sign-in is on."),
        Some("totp-off") => Some("Two-factor sign-in is off."),
        Some("oidc-linked") => Some("The sign-in provider is linked to your account."),
        Some("oidc-already") => Some("That sign-in was already linked to your account."),
        Some("oidc-unlinked") => Some("The sign-in provider was unlinked."),
        Some("oidc-last") => Some(
            "Not unlinked: your account has no password, so this is your only way to sign in. \
             Set a password first (use \"Forgot your password?\" on the sign-in page).",
        ),
        _ => None,
    };
    let next = safe_next(q.next.as_deref(), "");
    profile_view(&web, &s, StatusCode::OK, &next, notice, BTreeMap::new()).await
}

async fn profile_post(
    State(web): State<Web>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let Some(s) = web.auth.signed_in(&headers).await else {
        return see_other("/account/login/?next=/account/profile/");
    };
    if !csrf_ok(&s, &headers, &form) {
        return forbidden_csrf(&web.ui);
    }
    let next = safe_next(form.get("next"), "");
    let back = |done: &str| {
        if next.is_empty() {
            see_other(&format!("/account/profile/?done={done}"))
        } else {
            let sep = if next.contains('?') { '&' } else { '?' };
            see_other(&format!("{next}{sep}done={done}"))
        }
    };
    let fail = |field: &str, msg: &str| BTreeMap::from([(field.to_string(), msg.to_string())]);
    match form.get("action") {
        Some("name") => {
            let changes = crate::UserChanges {
                name: Some(form.get("name").unwrap_or("").to_string()),
                ..Default::default()
            };
            match web
                .auth
                .update_user(s.user.id, s.user.version, &changes)
                .await
            {
                Ok(_) => back("name"),
                Err(AuthError::Invalid(errs)) => {
                    profile_view(
                        &web,
                        &s,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &next,
                        None,
                        errs.into_iter().collect(),
                    )
                    .await
                }
                Err(AuthError::Conflict { .. }) => {
                    profile_view(
                        &web,
                        &s,
                        StatusCode::CONFLICT,
                        &next,
                        None,
                        fail(
                            "name",
                            "was changed elsewhere in the meantime; check it and save again",
                        ),
                    )
                    .await
                }
                Err(e) => server_error(&web, anyhow::anyhow!("{e}")),
            }
        }
        Some("password") => {
            match web
                .auth
                .check_password(s.user.id, form.get("current").unwrap_or(""))
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return profile_view(
                        &web,
                        &s,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &next,
                        None,
                        fail("current", "is not right"),
                    )
                    .await;
                }
                Err(e) => return server_error(&web, e),
            }
            let new = form.get("password").unwrap_or("");
            if form.get("password_again") != Some(new) {
                return profile_view(
                    &web,
                    &s,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &next,
                    None,
                    fail("password_again", "does not match"),
                )
                .await;
            }
            match web.auth.set_password(s.user.id, new, Some(&s)).await {
                Ok(()) => back("password"),
                Err(AuthError::Invalid(errs)) => {
                    profile_view(
                        &web,
                        &s,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &next,
                        None,
                        errs.into_iter().collect(),
                    )
                    .await
                }
                Err(e) => server_error(&web, anyhow::anyhow!("{e}")),
            }
        }
        Some("totp_on") => {
            let secret = form.get("secret").unwrap_or("");
            if crate::secret::base32_decode(secret).is_none_or(|k| k.len() < 16) {
                return profile_view(
                    &web,
                    &s,
                    StatusCode::BAD_REQUEST,
                    &next,
                    None,
                    fail(
                        "code",
                        "could not be checked: reload the page and try again",
                    ),
                )
                .await;
            }
            match web
                .auth
                .enable_totp(s.user.id, secret, form.get("code").unwrap_or(""))
                .await
            {
                Ok(true) => back("totp-on"),
                Ok(false) => {
                    profile_view(
                        &web,
                        &s,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &next,
                        None,
                        fail("code", "is not right; use the current code"),
                    )
                    .await
                }
                Err(e) => server_error(&web, e),
            }
        }
        Some("totp_off") => {
            match web
                .auth
                .check_password(s.user.id, form.get("current").unwrap_or(""))
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return profile_view(
                        &web,
                        &s,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &next,
                        None,
                        fail("totp_current", "is not right"),
                    )
                    .await;
                }
                Err(e) => return server_error(&web, e),
            }
            match web.auth.disable_totp(s.user.id).await {
                Ok(()) => back("totp-off"),
                Err(e) => server_error(&web, e),
            }
        }
        _ => {
            profile_view(
                &web,
                &s,
                StatusCode::BAD_REQUEST,
                &next,
                None,
                BTreeMap::new(),
            )
            .await
        }
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
            html.contains(&format!(
                "/_engine/ui/{}",
                engine_core::ui::TAGBRUSH_CSS
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap()
            )),
            "{html}"
        );
    }
}
