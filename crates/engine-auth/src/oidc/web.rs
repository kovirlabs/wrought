//! The single sign-on routes:
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /account/oidc/<name>/start/` | sign in with a provider (signed-out CSRF token) |
//! | `POST /account/oidc/<name>/link/` | link a provider to the signed-in account |
//! | `GET /account/oidc/<name>/callback/` | the provider sends the browser back here |
//! | `POST /account/oidc/unlink/` | unlink a provider (`issuer`) |
//!
//! Starting is a POST with a CSRF token, so another site cannot sign a
//! visitor in to an account of its choosing (login CSRF). The attempt is
//! bound to the browser by a random `__Host-` cookie (SameSite=Lax, so it
//! comes back with the provider's redirect) whose SHA-256 names a database
//! row holding the state, nonce and PKCE verifier; the row is deleted when
//! the browser returns, and expires after ten minutes.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::context;
use serde::Deserialize;

use super::accounts::{Purpose, STATE_MINUTES};
use super::{LinkOutcome, Oidc, ProviderConfig, SignInOutcome, UnlinkOutcome, origin_of};
use crate::accounts::Stage;
use crate::web::{
    Client, FormData, Web, anon_csrf_ok, cookie, csrf_ok, forbidden_csrf, safe_next, see_other,
    server_error, set_cookie, url_encode, with_cookies,
};
use crate::{User, secret};

/// State of the routes.
#[derive(Clone)]
struct Sso {
    web: Web,
    oidc: Oidc,
}

/// The `/account/oidc/` routes for the providers of `oidc`.
pub fn router(web: Web, oidc: Oidc) -> Router {
    Router::new()
        .route("/account/oidc/{name}/start/", post(start))
        .route("/account/oidc/{name}/link/", post(link))
        .route("/account/oidc/{name}/callback/", get(callback))
        .route("/account/oidc/unlink/", post(unlink))
        .with_state(Sso { web, oidc })
}

impl Sso {
    fn cookie_name(&self) -> &'static str {
        if self.web.auth.config().cookie_secure {
            "__Host-hammerwright_oidc"
        } else {
            "hammerwright_oidc"
        }
    }

    fn cookie(&self, value: &str, max_age: u64) -> HeaderValue {
        // Lax: the provider's redirect back is a cross-site top-level GET.
        set_cookie(
            self.cookie_name(),
            value,
            max_age,
            self.web.auth.config().cookie_secure,
            "Lax",
        )
    }

    fn redirect_uri(&self, p: &ProviderConfig) -> String {
        format!(
            "{}/account/oidc/{}/callback/",
            self.web.auth.public_url(),
            p.name
        )
    }

    fn message(&self, status: StatusCode, title: &str, message: &str) -> Response {
        self.web.ui.page(
            status,
            "engine/message.html",
            context! { title, message, link => context! { url => "/account/login/", label => "Back to sign-in" } },
        )
    }
}

fn not_found(s: &Sso) -> Response {
    s.message(StatusCode::NOT_FOUND, "Not found", "There is no such page.")
}

fn limited(s: &Sso) -> Response {
    s.message(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many attempts",
        "Too many failed sign-ins came from your network. Wait 15 minutes and try again.",
    )
}

/// `POST /account/oidc/<name>/start/`
async fn start(
    State(s): State<Sso>,
    Path(name): Path<String>,
    client: Client,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let Some(p) = s.oidc.provider(&name).cloned() else {
        return not_found(&s);
    };
    if !anon_csrf_ok(&s.web, &headers, &form) {
        return forbidden_csrf(&s.web.ui);
    }
    let ip = s.web.auth.client_ip(&client);
    match s.web.auth.ip_limited(ip.as_deref()).await {
        Ok(true) => return limited(&s),
        Ok(false) => {}
        Err(e) => return server_error(&s.web, e),
    }
    let next = safe_next(form.get("next").filter(|n| !n.is_empty()), &s.web.home);
    begin(&s, &p, Purpose::Login, None, &next).await
}

/// `POST /account/oidc/<name>/link/`
async fn link(
    State(s): State<Sso>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let Some(p) = s.oidc.provider(&name).cloned() else {
        return not_found(&s);
    };
    let Some(session) = s.web.auth.signed_in(&headers).await else {
        return see_other("/account/login/?next=/account/profile/");
    };
    if !csrf_ok(&session, &headers, &form) {
        return forbidden_csrf(&s.web.ui);
    }
    begin(
        &s,
        &p,
        Purpose::Link,
        Some(session.user.id),
        "/account/profile/",
    )
    .await
}

/// Store the attempt and send the browser to the provider.
async fn begin(
    s: &Sso,
    p: &ProviderConfig,
    purpose: Purpose,
    user_id: Option<i64>,
    next: &str,
) -> Response {
    let started = match s.web.auth.oidc_begin(&p.name, purpose, user_id, next).await {
        Ok(st) => st,
        Err(e) => return server_error(&s.web, e),
    };
    let url = match s
        .oidc
        .authorization_url(
            &p.name,
            &s.redirect_uri(p),
            &started.state,
            &started.nonce,
            &started.verifier,
        )
        .await
    {
        Ok(u) => u,
        Err(e) => {
            eprintln!("oidc: {}: {e}", p.name);
            return s.message(
                StatusCode::BAD_GATEWAY,
                "Sign-in not available",
                &format!(
                    "{} cannot be reached right now. Try again in a moment, or sign in with \
                     your password.",
                    p.label
                ),
            );
        }
    };
    let cookie = s.cookie(&started.cookie, (STATE_MINUTES * 60) as u64);
    // The engine's pages allow form submissions to lead to the issuer's
    // origin (see `form-action`); a provider whose sign-in page lives
    // elsewhere gets a link instead of a redirect the browser would block.
    let r = if origin_of(&url) == p.issuer_origin() {
        see_other(&url)
    } else {
        s.web.ui.page(
            StatusCode::OK,
            "engine/message.html",
            context! {
                title => format!("Sign in with {}", p.label),
                message => format!("Continue to {} to sign in.", p.label),
                link => context! { url, label => format!("Continue to {}", p.label) },
            },
        )
    };
    with_cookies(r, [cookie])
}

#[derive(Deserialize, Default)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
}

/// `GET /account/oidc/<name>/callback/`
async fn callback(
    State(s): State<Sso>,
    Path(name): Path<String>,
    Query(q): Query<CallbackQuery>,
    client: Client,
    headers: HeaderMap,
) -> Response {
    // The attempt cookie is spent whatever happens next.
    let clear = s.cookie("", 0);
    let r = finish(&s, &name, q, &client, &headers).await;
    with_cookies(r, [clear])
}

async fn finish(
    s: &Sso,
    name: &str,
    q: CallbackQuery,
    client: &Client,
    headers: &HeaderMap,
) -> Response {
    let Some(p) = s.oidc.provider(name).cloned() else {
        return not_found(s);
    };
    let auth = &s.web.auth;
    let ip = auth.client_ip(client);
    let ip = ip.as_deref();
    match auth.ip_limited(ip).await {
        Ok(true) => return limited(s),
        Ok(false) => {}
        Err(e) => return server_error(&s.web, e),
    }
    let refuse = |why: &str| {
        eprintln!("oidc: {}: sign-in refused: {why}", p.name);
        s.message(
            StatusCode::BAD_REQUEST,
            "Sign-in not completed",
            &format!(
                "This sign-in with {} has expired, was already used, or was not started in \
                 this browser. Start again from the sign-in page.",
                p.label
            ),
        )
    };
    let pending = match cookie(headers, s.cookie_name()) {
        Some(token) => match auth.oidc_claim(&token).await {
            Ok(p) => p,
            Err(e) => return server_error(&s.web, e),
        },
        None => None,
    };
    let Some(pending) = pending else {
        let _ = auth.oidc_failure(ip).await;
        return refuse("no attempt in progress for this browser (cookie missing, expired or used)");
    };
    let state_ok = q
        .state
        .as_deref()
        .is_some_and(|st| secret::same(st, &pending.state));
    if pending.provider != p.name || !state_ok {
        let _ = auth.oidc_failure(ip).await;
        return refuse("state does not match the attempt");
    }
    // RFC 9207: a provider that names itself must be the one we asked.
    if q.iss.as_deref().is_some_and(|iss| iss != p.issuer) {
        let _ = auth.oidc_failure(ip).await;
        return refuse("the response names another issuer");
    }
    if let Some(err) = &q.error {
        let err: String = err
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(60)
            .collect();
        eprintln!("oidc: {}: the provider answered `{err}`", p.name);
        return s.message(
            StatusCode::BAD_REQUEST,
            "Sign-in not completed",
            &format!("{} did not sign you in ({err}).", p.label),
        );
    }
    let Some(code) = q
        .code
        .as_deref()
        .filter(|c| !c.is_empty() && c.len() <= 2048)
    else {
        let _ = auth.oidc_failure(ip).await;
        return refuse("no code");
    };
    let claims = match s
        .oidc
        .exchange(
            &p.name,
            code,
            &s.redirect_uri(&p),
            &pending.verifier,
            &pending.nonce,
        )
        .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("oidc: {}: {e}", p.name);
            let _ = auth.oidc_failure(ip).await;
            return s.message(
                StatusCode::BAD_GATEWAY,
                "Sign-in not completed",
                &format!(
                    "The answer from {} could not be verified. Start again from the sign-in \
                     page.",
                    p.label
                ),
            );
        }
    };
    match pending.purpose {
        Purpose::Login => sign_in(s, &p, &claims, &pending.next, client, headers).await,
        Purpose::Link => link_done(s, &p, &claims, pending.user_id, headers).await,
    }
}

async fn sign_in(
    s: &Sso,
    p: &ProviderConfig,
    claims: &super::jwt::IdClaims,
    next: &str,
    client: &Client,
    headers: &HeaderMap,
) -> Response {
    let auth = &s.web.auth;
    let ip = auth.client_ip(client);
    let outcome = match auth.oidc_sign_in(p, claims, ip.as_deref()).await {
        Ok(o) => o,
        Err(e) => return server_error(&s.web, e),
    };
    let label = &p.label;
    let (status, title, message) = match outcome {
        SignInOutcome::Ok(user) | SignInOutcome::Created(user) => {
            return start_session(s, user, next, client, headers).await;
        }
        SignInOutcome::Blocked => (
            StatusCode::FORBIDDEN,
            "Account blocked",
            "This account is blocked. Ask the site's administrators.".to_string(),
        ),
        SignInOutcome::Limited => return limited(s),
        SignInOutcome::NotLinked => (
            StatusCode::FORBIDDEN,
            "No account for this sign-in",
            format!(
                "No account on this site uses this {label} sign-in. If you have an account, \
                 sign in with your password and link {label} on your profile page."
            ),
        ),
        SignInOutcome::EmailInUse => (
            StatusCode::CONFLICT,
            "Link your account first",
            format!(
                "An account with this email address already exists, and it is not linked to \
                 {label}. To use {label} for it, sign in with that account's password, then \
                 link {label} on your profile page."
            ),
        ),
        SignInOutcome::NoVerifiedEmail => (
            StatusCode::FORBIDDEN,
            "Email address needed",
            format!(
                "{label} did not confirm an email address for you, so no account can be made. \
                 Confirm your address with {label} and try again."
            ),
        ),
        SignInOutcome::Unverified(user) => {
            if let Err(e) = auth.resend_verification(&user).await {
                return server_error(&s.web, e);
            }
            (
                StatusCode::FORBIDDEN,
                "Confirm your email address",
                "Confirm your email address first: we sent you a link (check your spam folder \
                 too). Follow it, then sign in."
                    .to_string(),
            )
        }
    };
    s.message(status, title, &message)
}

/// A new session, exactly as after a password sign-in (with the two-factor
/// step when the account has it on).
async fn start_session(
    s: &Sso,
    user: User,
    next: &str,
    client: &Client,
    headers: &HeaderMap,
) -> Response {
    let auth = &s.web.auth;
    // Whatever session this browser had ends.
    if let Some(old) = auth.session_from(headers).await
        && let Err(e) = auth.end_session(&old).await
    {
        return server_error(&s.web, e);
    }
    let mut next = next.to_string();
    if next.starts_with("/admin/") && !auth.access(&user).await.is_ok_and(|a| a.can_use_admin()) {
        next = "/".into();
    }
    let stage = if user.totp_enabled {
        Stage::Totp
    } else {
        Stage::Full
    };
    let ip = auth.client_ip(client);
    let token = match auth
        .start_session(user.id, stage, ip.as_deref(), &client.user_agent)
        .await
    {
        Ok(t) => t,
        Err(e) => return server_error(&s.web, e),
    };
    let to = if stage == Stage::Totp {
        format!("/account/totp/?next={}", url_encode(&next))
    } else {
        next
    };
    with_cookies(see_other(&to), [auth.session_cookie(&token)])
}

async fn link_done(
    s: &Sso,
    p: &ProviderConfig,
    claims: &super::jwt::IdClaims,
    user_id: Option<i64>,
    headers: &HeaderMap,
) -> Response {
    // The account that started linking must still be the one signed in.
    let session = s.web.auth.signed_in(headers).await;
    let Some(session) = session.filter(|se| Some(se.user.id) == user_id) else {
        return s.message(
            StatusCode::FORBIDDEN,
            "Not linked",
            "You are no longer signed in as the account that started linking. Sign in and try \
             again from your profile page.",
        );
    };
    let outcome = match s.web.auth.oidc_link(session.user.id, p, claims).await {
        Ok(o) => o,
        Err(e) => return server_error(&s.web, e),
    };
    let label = &p.label;
    match outcome {
        LinkOutcome::Linked => see_other("/account/profile/?done=oidc-linked"),
        LinkOutcome::AlreadyLinked => see_other("/account/profile/?done=oidc-already"),
        LinkOutcome::OtherAccount => s.message(
            StatusCode::CONFLICT,
            "Not linked",
            &format!("This {label} sign-in is already linked to another account."),
        ),
        LinkOutcome::IssuerInUse => s.message(
            StatusCode::CONFLICT,
            "Not linked",
            &format!(
                "Your account is linked to another {label} sign-in. Unlink it on your profile \
                 page first."
            ),
        ),
    }
}

/// `POST /account/oidc/unlink/` (`issuer`).
async fn unlink(
    State(s): State<Sso>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<FormData>,
) -> Response {
    let Some(session) = s.web.auth.signed_in(&headers).await else {
        return see_other("/account/login/?next=/account/profile/");
    };
    if !csrf_ok(&session, &headers, &form) {
        return forbidden_csrf(&s.web.ui);
    }
    let issuer = form.get("issuer").unwrap_or("");
    match s.web.auth.oidc_unlink(session.user.id, issuer).await {
        Ok(UnlinkOutcome::Unlinked) => see_other("/account/profile/?done=oidc-unlinked"),
        Ok(UnlinkOutcome::NotLinked) => see_other("/account/profile/"),
        Ok(UnlinkOutcome::LastWay) => see_other("/account/profile/?done=oidc-last"),
        Err(e) => server_error(&s.web, e),
    }
}
