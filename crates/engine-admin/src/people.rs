//! Accounts, roles and your own profile.

use crate::Refusal;
use std::collections::BTreeMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use engine_auth::web::{FormData, see_other};
use engine_auth::{AuthError, Session, UserChanges, totp};
use minijinja::{Value, context};
use serde::Deserialize;

use crate::Admin;

#[derive(Deserialize, Default)]
pub(crate) struct NoticeQ {
    notice: Option<String>,
}

fn users_allowed(a: &Admin, s: &Session) -> Result<(), Refusal> {
    if s.access.can_manage_users() {
        Ok(())
    } else {
        Err(Box::new(a.forbidden(s, "manage accounts")))
    }
}

fn errors_map(errs: Vec<(String, String)>) -> BTreeMap<String, String> {
    errs.into_iter().collect()
}

/// Role names and labels for checkboxes.
async fn role_choices(a: &Admin) -> Vec<Value> {
    #[cfg(feature = "roles")]
    {
        a.auth()
            .roles()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| context! { name => r.name, label => r.label })
            .collect()
    }
    #[cfg(not(feature = "roles"))]
    {
        let _ = a;
        Vec::new()
    }
}

/// Only administrators may change administrators' accounts (otherwise an
/// account manager could take one over through its email address).
async fn guard_target(a: &Admin, s: &Session, target: &engine_auth::User) -> Result<(), Refusal> {
    if s.access.is_admin() {
        return Ok(());
    }
    match a.auth().access(target).await {
        Ok(acc) if acc.is_admin() => Err(Box::new(
            a.forbidden(s, "change an administrator's account"),
        )),
        Ok(_) => Ok(()),
        Err(e) => Err(Box::new(a.error(s, e))),
    }
}

/// Whether `roles` names an administrator role.
async fn names_admin_role(a: &Admin, roles: &[String]) -> bool {
    #[cfg(feature = "roles")]
    {
        a.auth()
            .roles()
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.is_admin && roles.contains(&r.name))
    }
    #[cfg(not(feature = "roles"))]
    {
        let _ = (a, roles);
        false
    }
}

fn roles_from(form: &FormData) -> Option<Vec<String>> {
    if cfg!(feature = "roles") {
        Some(form.all("roles").iter().map(|s| s.to_string()).collect())
    } else {
        None
    }
}

pub(crate) async fn users(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Query(q): Query<NoticeQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    let users = match a.auth().users().await {
        Ok(u) => u,
        Err(e) => return a.error(&s, e),
    };
    a.page(
        StatusCode::OK,
        "admin/users.html",
        a.base(&s, "Accounts"),
        context! { users, notice => q.notice, roles_feature => cfg!(feature = "roles") },
    )
}

async fn user_page(
    a: &Admin,
    s: &Session,
    status: StatusCode,
    user: Option<&engine_auth::User>,
    values: Value,
    errors: BTreeMap<String, String>,
    notice: Option<String>,
) -> Response {
    let title = match user {
        Some(u) => format!("Account: {}", u.display_name()),
        None => "New account".to_string(),
    };
    let roles = role_choices(a).await;
    let identities = match user {
        Some(u) => a.auth().identities(u.id).await.unwrap_or_else(|e| {
            eprintln!("admin: reading sign-in identities: {e:#}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    a.page(
        status,
        "admin/user.html",
        a.base(s, &title),
        context! {
            account => user, values, errors, roles, notice, identities,
            roles_feature => cfg!(feature = "roles"),
            is_self => user.is_some_and(|u| u.id == s.user.id),
        },
    )
}

pub(crate) async fn new_user_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    user_page(
        &a,
        &s,
        StatusCode::OK,
        None,
        context! { active => true, roles => Vec::<String>::new() },
        BTreeMap::new(),
        None,
    )
    .await
}

pub(crate) async fn create_user(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    let email = form.get("email").unwrap_or("").to_string();
    let name = form.get("name").unwrap_or("").to_string();
    let password = form.get("password").filter(|p| !p.is_empty());
    let roles = roles_from(&form).unwrap_or_default();
    if !s.access.is_admin() && names_admin_role(&a, &roles).await {
        return a.forbidden(&s, "grant administrator roles");
    }
    match a.auth().create_user(&email, &name, password, &roles).await {
        Ok(u) => {
            if password.is_none() {
                // No password: the new owner chooses one through a link.
                if let Err(e) = a.auth().request_reset(&u.email, None).await {
                    return a.error(&s, e);
                }
                return see_other(&format!("/admin/users/{}/?notice=invited", u.id));
            }
            see_other(&format!("/admin/users/{}/?notice=created", u.id))
        }
        Err(AuthError::Invalid(errs)) => {
            user_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                context! { email, name, roles, active => true },
                errors_map(errs),
                None,
            )
            .await
        }
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn user_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Query(q): Query<NoticeQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    let u = match a.auth().user(id).await {
        Ok(Some(u)) => u,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let values = context! {
        email => u.email, name => u.name, active => u.active, roles => u.roles, version => u.version,
    };
    user_page(
        &a,
        &s,
        StatusCode::OK,
        Some(&u),
        values,
        BTreeMap::new(),
        q.notice,
    )
    .await
}

pub(crate) async fn save_user(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    let u = match a.auth().user(id).await {
        Ok(Some(u)) => u,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    if let Err(r) = guard_target(&a, &s, &u).await {
        return *r;
    }
    // Only administrators hand out administrator roles.
    let roles = roles_from(&form);
    if !s.access.is_admin()
        && let Some(new) = &roles
        && names_admin_role(&a, new).await
    {
        return a.forbidden(&s, "grant administrator roles");
    }
    let version = form
        .get("_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let ch = UserChanges {
        email: form.get("email").map(str::to_string),
        name: form.get("name").map(str::to_string),
        active: Some(form.all("active").contains(&"true")),
        roles,
    };
    match a.auth().update_user(id, version, &ch).await {
        Ok(_) => see_other(&format!("/admin/users/{id}/?notice=saved")),
        Err(AuthError::Invalid(errs)) => {
            let values = context! {
                email => ch.email, name => ch.name, active => ch.active, version,
                roles => ch.roles.clone().unwrap_or_else(|| u.roles.clone()),
            };
            user_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(&u),
                values,
                errors_map(errs),
                None,
            )
            .await
        }
        Err(AuthError::Conflict { current, .. }) => a.page(
            StatusCode::CONFLICT,
            "admin/message.html",
            a.base(&s, "Not saved"),
            context! {
                message => format!("Someone changed this account (now version {current}) after you opened it, so your changes were not saved."),
                link => context! { url => format!("/admin/users/{id}/"), label => "Open the current version" },
            },
        ),
        Err(AuthError::NotFound) => a.not_found(&s),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn send_reset(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    let u = match a.auth().user(id).await {
        Ok(Some(u)) => u,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    if let Err(r) = guard_target(&a, &s, &u).await {
        return *r;
    }
    match a.auth().request_reset(&u.email, None).await {
        Ok(()) => see_other(&format!("/admin/users/{id}/?notice=reset-sent")),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn user_totp_off(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<i64>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if let Err(r) = users_allowed(&a, &s) {
        return *r;
    }
    match a.auth().user(id).await {
        Ok(Some(u)) => {
            if let Err(r) = guard_target(&a, &s, &u).await {
                return *r;
            }
        }
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    }
    if let Err(e) = a.auth().disable_totp(id).await {
        return a.error(&s, e);
    }
    see_other(&format!("/admin/users/{id}/?notice=totp-off"))
}

// ----- profile ----------------------------------------------------------------

async fn profile_page(
    a: &Admin,
    s: &Session,
    status: StatusCode,
    notice: Option<String>,
    errors: BTreeMap<String, String>,
) -> Response {
    let (secret, uri, qr) = if s.user.totp_enabled || !a.auth().totp_available() {
        (None, None, None)
    } else {
        let secret = totp::new_secret();
        let uri = totp::uri(&secret, a.auth().site_name(), &s.user.email);
        let qr = totp::qr_svg(&uri).map(minijinja::Value::from_safe_string);
        (Some(secret), Some(uri), qr)
    };
    a.page(
        status,
        "admin/profile.html",
        a.base(s, "Your profile"),
        context! {
            account => s.user, notice, errors, totp_secret => secret, totp_uri => uri,
            totp_qr => qr, totp_available => a.auth().totp_available(),
            totp_key_env => engine_auth::sitekey::KEY_ENV,
            totp_required => a.auth().config().require_totp_for_admins && s.access.is_admin(),
        },
    )
}

pub(crate) async fn profile(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Query(q): Query<NoticeQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    profile_page(&a, &s, StatusCode::OK, q.notice, BTreeMap::new()).await
}

pub(crate) async fn change_password(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let fail = |field: &str, msg: &str| BTreeMap::from([(field.to_string(), msg.to_string())]);
    match a
        .auth()
        .check_password(s.user.id, form.get("current").unwrap_or(""))
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return profile_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                fail("current", "is not right"),
            )
            .await;
        }
        Err(e) => return a.error(&s, e),
    }
    let new = form.get("password").unwrap_or("");
    if form.get("password_again") != Some(new) {
        return profile_page(
            &a,
            &s,
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
            fail("password_again", "does not match"),
        )
        .await;
    }
    match a.auth().set_password(s.user.id, new, Some(&s)).await {
        Ok(()) => see_other("/admin/profile/?notice=password"),
        Err(AuthError::Invalid(errs)) => {
            profile_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                errors_map(errs),
            )
            .await
        }
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn enable_totp(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let secret = form.get("secret").unwrap_or("");
    if engine_auth::secret::base32_decode(secret).is_none_or(|k| k.len() < 16) {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Not changed",
            "The form's secret is not valid. Reload the page.",
        );
    }
    match a
        .auth()
        .enable_totp(s.user.id, secret, form.get("code").unwrap_or(""))
        .await
    {
        Ok(true) => see_other("/admin/profile/?notice=totp-on"),
        Ok(false) => {
            let errs = BTreeMap::from([(
                "code".to_string(),
                "is not right; use the current code".to_string(),
            )]);
            profile_page(&a, &s, StatusCode::UNPROCESSABLE_ENTITY, None, errs).await
        }
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn disable_totp(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match a
        .auth()
        .check_password(s.user.id, form.get("current").unwrap_or(""))
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            let errs = BTreeMap::from([("totp_current".to_string(), "is not right".to_string())]);
            return profile_page(&a, &s, StatusCode::UNPROCESSABLE_ENTITY, None, errs).await;
        }
        Err(e) => return a.error(&s, e),
    }
    match a.auth().disable_totp(s.user.id).await {
        Ok(()) => see_other("/admin/profile/?notice=totp-off"),
        Err(e) => a.error(&s, e),
    }
}

// ----- roles -------------------------------------------------------------------

#[cfg(feature = "roles")]
mod role_pages {
    use super::*;
    use engine_auth::{Action, FieldAccess, Grants, TypeGrant};

    /// The grid of a role form: one row per type (and `*`).
    fn grid(a: &Admin, g: &Grants) -> Vec<Value> {
        let reg = a.store().registry();
        let mut rows = vec![{
            let tg = g.types.get("*").cloned().unwrap_or_default();
            context! { name => "*", label => "Every other type", grant => tg, fields => Vec::<Value>::new() }
        }];
        for t in reg.types() {
            let own = g.types.get(&t.name);
            let tg = own.cloned().unwrap_or_default();
            let fields: Vec<Value> = t
                .fields
                .iter()
                .map(|f| {
                    let level = tg.fields.get(&f.name).copied().unwrap_or_default();
                    context! { name => f.name, label => f.label.clone().unwrap_or_else(|| f.name.clone()), level => level.as_str() }
                })
                .collect();
            rows.push(context! { name => t.name, label => t.label, grant => tg, own => own.is_some(), fields });
        }
        rows
    }

    /// Grants from a role form. A type row only counts when its "own rules"
    /// box is ticked (otherwise `*` applies).
    fn parse_grants(a: &Admin, form: &FormData) -> Grants {
        let reg = a.store().registry();
        let mut g = Grants {
            taxonomy: form.get("taxonomy") == Some("true"),
            forms: form.get("forms") == Some("true"),
            users: form.get("users") == Some("true"),
            ..Default::default()
        };
        let names: Vec<String> = std::iter::once("*".to_string())
            .chain(reg.types().map(|t| t.name.clone()))
            .collect();
        for n in names {
            if n != "*" && form.get(&format!("own.{n}")) != Some("true") {
                continue;
            }
            let mut tg = TypeGrant::default();
            let on = |a: Action| form.all(&format!("t.{n}.{}", a.as_str())).contains(&"true");
            tg.view = on(Action::View);
            tg.create = on(Action::Create);
            tg.edit = on(Action::Edit);
            tg.delete = on(Action::Delete);
            tg.publish = on(Action::Publish);
            if let Some(t) = reg.get(&n) {
                for f in &t.fields {
                    let level = form
                        .get(&format!("f.{n}.{}", f.name))
                        .and_then(FieldAccess::parse)
                        .unwrap_or_default();
                    if level != FieldAccess::Write {
                        tg.fields.insert(f.name.clone(), level);
                    }
                }
            }
            if n == "*" && tg == TypeGrant::default() {
                continue;
            }
            g.types.insert(n, tg);
        }
        g
    }

    #[allow(clippy::too_many_arguments)]
    async fn role_page(
        a: &Admin,
        s: &Session,
        status: StatusCode,
        role: Option<&engine_auth::roles::Role>,
        name: &str,
        label: &str,
        grants: &Grants,
        errors: BTreeMap<String, String>,
        notice: Option<String>,
    ) -> Response {
        let title = match role {
            Some(r) => format!("Role: {}", r.label),
            None => "New role".into(),
        };
        a.page(
            status,
            "admin/role.html",
            a.base(s, &title),
            context! {
                role, name, label, errors, notice,
                taxonomy => grants.taxonomy, users => grants.users, forms => grants.forms,
                forms_feature => cfg!(feature = "forms"),
                grid => grid(a, grants),
                actions => ["view", "create", "edit", "delete", "publish"],
            },
        )
    }

    pub(crate) async fn roles(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
        Query(q): Query<NoticeQ>,
    ) -> Response {
        let s = match a.require(&headers, &uri).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        match a.auth().roles().await {
            Ok(roles) => a.page(
                StatusCode::OK,
                "admin/roles.html",
                a.base(&s, "Roles"),
                context! { roles, notice => q.notice },
            ),
            Err(e) => a.error(&s, e),
        }
    }

    pub(crate) async fn new_role_form(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let s = match a.require(&headers, &uri).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        role_page(
            &a,
            &s,
            StatusCode::OK,
            None,
            "",
            "",
            &Grants::default(),
            BTreeMap::new(),
            None,
        )
        .await
    }

    pub(crate) async fn create_role(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
        Form(form): Form<FormData>,
    ) -> Response {
        let s = match a.require_post(&headers, &uri, &form).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        let g = parse_grants(&a, &form);
        let (name, label) = (
            form.get("name").unwrap_or(""),
            form.get("label").unwrap_or(""),
        );
        match a.auth().create_role(name, label, &g).await {
            Ok(r) => see_other(&format!("/admin/roles/{}/?notice=created", r.name)),
            Err(AuthError::Invalid(errs)) => {
                role_page(
                    &a,
                    &s,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                    name,
                    label,
                    &g,
                    errors_map(errs),
                    None,
                )
                .await
            }
            Err(e) => a.error(&s, e),
        }
    }

    pub(crate) async fn role_form(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
        Path(name): Path<String>,
        Query(q): Query<NoticeQ>,
    ) -> Response {
        let s = match a.require(&headers, &uri).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        match a.auth().role(&name).await {
            Ok(Some(r)) => {
                role_page(
                    &a,
                    &s,
                    StatusCode::OK,
                    Some(&r),
                    &r.name,
                    &r.label,
                    &r.grants,
                    BTreeMap::new(),
                    q.notice,
                )
                .await
            }
            Ok(None) => a.not_found(&s),
            Err(e) => a.error(&s, e),
        }
    }

    pub(crate) async fn save_role(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
        Path(name): Path<String>,
        Form(form): Form<FormData>,
    ) -> Response {
        let s = match a.require_post(&headers, &uri, &form).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        let role = match a.auth().role(&name).await {
            Ok(Some(r)) => r,
            Ok(None) => return a.not_found(&s),
            Err(e) => return a.error(&s, e),
        };
        let g = parse_grants(&a, &form);
        let label = form.get("label").unwrap_or("");
        let version = form
            .get("_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match a.auth().update_role(&name, version, label, &g).await {
            Ok(_) => see_other(&format!("/admin/roles/{name}/?notice=saved")),
            Err(AuthError::Invalid(errs)) => {
                role_page(&a, &s, StatusCode::UNPROCESSABLE_ENTITY, Some(&role), &name, label, &g, errors_map(errs), None).await
            }
            Err(AuthError::Conflict { current, .. }) => a.page(
                StatusCode::CONFLICT,
                "admin/message.html",
                a.base(&s, "Not saved"),
                context! {
                    message => format!("Someone changed this role (now version {current}) after you opened it, so your changes were not saved."),
                    link => context! { url => format!("/admin/roles/{name}/"), label => "Open the current version" },
                },
            ),
            Err(AuthError::NotFound) => a.not_found(&s),
            Err(e) => a.error(&s, e),
        }
    }

    pub(crate) async fn delete_role(
        State(a): State<Admin>,
        headers: HeaderMap,
        uri: Uri,
        Path(name): Path<String>,
        Form(form): Form<FormData>,
    ) -> Response {
        let s = match a.require_post(&headers, &uri, &form).await {
            Ok(s) => s,
            Err(r) => return *r,
        };
        if let Err(r) = users_allowed(&a, &s) {
            return *r;
        }
        let version = form
            .get("_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match a.auth().delete_role(&name, version).await {
            Ok(()) => see_other("/admin/roles/?notice=deleted"),
            Err(AuthError::Invalid(errs)) => a.message(
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Not deleted",
                &errs
                    .into_iter()
                    .map(|(_, m)| m)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            Err(AuthError::Conflict { .. }) => a.message(
                &s,
                StatusCode::CONFLICT,
                "Not deleted",
                "Someone changed this role after you opened it. Look at it again first.",
            ),
            Err(AuthError::NotFound) => see_other("/admin/roles/"),
            Err(e) => a.error(&s, e),
        }
    }
}

#[cfg(feature = "roles")]
pub(crate) use role_pages::{create_role, delete_role, new_role_form, role_form, roles, save_role};
