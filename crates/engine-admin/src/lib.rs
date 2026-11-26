//! Wrought's `admin` feature: a server-rendered admin under `/admin/`.
//!
//! * content of every type in `types/*.toml`: list (search, pagination),
//!   create, edit, delete, with forms generated from the type
//!   ([`forms`]); revision history with view and restore;
//! * taxonomy terms: rename, delete;
//! * accounts, and with `roles`, roles with per-type and per-field rights;
//! * your profile: password, two-factor sign-in.
//!
//! Every page is plain HTML forms that work without JavaScript; htmx
//! (bundled, served from `/_engine/ui/`) boosts links and forms and filters
//! lists as you type. Saves carry the version they edited: a save over
//! someone else's newer version is refused with a conflict screen that keeps
//! the editor's changes and shows the current version (optimistic locking,
//! see `engine_content::store`). Rights are checked on the server for every
//! request (see [`forms::parse`]).

// Guards return `Result<_, Response>`: the error *is* the page to send, as
// in axum's own extractors; boxing it would only add noise.
#![allow(clippy::result_large_err)]

mod content;
pub mod forms;
mod people;

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use engine_auth::web::{
    FormData, Ui, UiAsset, Web, csrf_ok, forbidden_csrf, see_other, url_encode,
};
use engine_auth::{Action, Auth, Session};
use engine_content::{ContentSite, Store};
use include_dir::{Dir, include_dir};
use minijinja::{Value, context};
use serde::Serialize;

static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// htmx, bundled (see `vendor/VENDOR.toml`).
pub const HTMX_VERSION: &str = "2.0.11";
pub const HTMX: UiAsset = UiAsset {
    name: "htmx-2.0.11.min.js",
    body: include_bytes!("../vendor/htmx/htmx.min.js"),
    content_type: "text/javascript; charset=utf-8",
};
const HTMX_LICENSE: UiAsset = UiAsset {
    name: "htmx-LICENSE.txt",
    body: include_bytes!("../vendor/htmx/LICENSE"),
    content_type: "text/plain; charset=utf-8",
};

/// Items per admin list page.
pub const PER_PAGE: i64 = 25;

/// The engine UI with the admin's templates and htmx.
pub fn ui(site_name: &str) -> Arc<Ui> {
    Arc::new(Ui::new(site_name, &[&TEMPLATES], &[HTMX, HTMX_LICENSE]))
}

/// What the admin needs. Cheap to clone.
#[derive(Clone)]
pub struct Admin {
    inner: Arc<Inner>,
}

struct Inner {
    web: Web,
    store: Store,
    site: ContentSite,
}

impl Admin {
    /// `web.ui` must come from [`ui`].
    pub fn new(web: Web, store: Store, site: ContentSite) -> Self {
        Admin {
            inner: Arc::new(Inner { web, store, site }),
        }
    }

    pub(crate) fn auth(&self) -> &Auth {
        &self.inner.web.auth
    }
    pub(crate) fn ui(&self) -> &Ui {
        &self.inner.web.ui
    }
    pub(crate) fn store(&self) -> &Store {
        &self.inner.store
    }
    pub(crate) fn site(&self) -> &ContentSite {
        &self.inner.site
    }

    /// The signed-in session allowed into the admin, or the response to
    /// send instead (sign-in redirect, 403, two-factor enrolment).
    pub(crate) async fn require(
        &self,
        headers: &HeaderMap,
        uri: &Uri,
    ) -> Result<Session, Response> {
        let Some(s) = self.auth().signed_in(headers).await else {
            let next = uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/admin/");
            return Err(see_other(&format!(
                "/account/login/?next={}",
                url_encode(next)
            )));
        };
        if !s.access.can_use_admin() {
            return Err(self.message(
                &s,
                StatusCode::FORBIDDEN,
                "No access",
                "Your account has no rights in the admin. Ask an administrator for a role.",
            ));
        }
        if self.auth().config().require_totp_for_admins
            && s.access.is_admin()
            && !s.user.totp_enabled
            && !uri.path().starts_with("/admin/profile/")
        {
            return Err(see_other("/admin/profile/?notice=totp-required"));
        }
        Ok(s)
    }

    /// [`Self::require`] plus the CSRF check for a POST.
    pub(crate) async fn require_post(
        &self,
        headers: &HeaderMap,
        uri: &Uri,
        form: &FormData,
    ) -> Result<Session, Response> {
        let s = self.require(headers, uri).await?;
        if !csrf_ok(&s, headers, form) {
            return Err(forbidden_csrf(self.ui()));
        }
        Ok(s)
    }

    /// Base context of every admin page.
    pub(crate) fn base(&self, s: &Session, title: &str) -> Value {
        let a = &s.access;
        let reg = self.store().registry();
        let types: Vec<Value> = reg
            .types()
            .filter(|t| a.can(&t.name, Action::View))
            .map(|t| context! { name => t.name, label => t.label })
            .collect();
        context! {
            title => title,
            user => context! { name => s.user.display_name(), email => s.user.email, roles => s.access.roles },
            csrf => s.csrf,
            nav => context! {
                types => types,
                taxonomy => a.can_manage_taxonomy() && reg.vocabularies().next().is_some(),
                users => a.can_manage_users(),
                roles => a.can_manage_users() && cfg!(feature = "roles"),
            },
        }
    }

    pub(crate) fn page(&self, status: StatusCode, name: &str, base: Value, ctx: Value) -> Response {
        self.ui().page(status, name, merge(base, ctx))
    }

    pub(crate) fn message(
        &self,
        s: &Session,
        status: StatusCode,
        title: &str,
        message: &str,
    ) -> Response {
        self.page(
            status,
            "admin/message.html",
            self.base(s, title),
            context! { message },
        )
    }

    pub(crate) fn not_found(&self, s: &Session) -> Response {
        self.message(
            s,
            StatusCode::NOT_FOUND,
            "Not found",
            "There is nothing here. It may have been deleted.",
        )
    }

    pub(crate) fn forbidden(&self, s: &Session, what: &str) -> Response {
        self.message(
            s,
            StatusCode::FORBIDDEN,
            "Not allowed",
            &format!("Your roles do not allow you to {what}."),
        )
    }

    pub(crate) fn error(&self, s: &Session, e: impl std::fmt::Display) -> Response {
        eprintln!("admin: {e}");
        self.message(
            s,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "The server could not finish this request. Nothing was changed if the page did not say so.",
        )
    }

    /// Who revisions are attributed to.
    pub(crate) fn author(s: &Session) -> String {
        s.user.email.clone()
    }
}

/// Merge two map values (right wins).
pub(crate) fn merge(a: Value, b: Value) -> Value {
    let mut m = engine_core::templates::value_map(&a);
    m.extend(engine_core::templates::value_map(&b));
    Value::from(m)
}

/// Page numbers for a list footer.
#[derive(Debug, Serialize)]
pub(crate) struct Pager {
    pub page: i64,
    pub pages: i64,
    pub total: i64,
    pub prev: Option<String>,
    pub next: Option<String>,
}

impl Pager {
    pub fn new(page: i64, total: i64, href: impl Fn(i64) -> String) -> Self {
        let pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
        let page = page.clamp(1, pages);
        Pager {
            page,
            pages,
            total,
            prev: (page > 1).then(|| href(page - 1)),
            next: (page < pages).then(|| href(page + 1)),
        }
    }

    pub fn offset(&self) -> i64 {
        (self.page - 1) * PER_PAGE
    }
}

/// The `/admin/` routes. Merge with [`engine_auth::web::router`] (same
/// [`Web`]) so sign-in and the assets exist.
pub fn router(admin: Admin) -> Router {
    let r = Router::new()
        .route("/admin", get(|| async { see_other("/admin/") }))
        .route("/admin/", get(content::dashboard))
        .route("/admin/content/{ty}/", get(content::list))
        .route(
            "/admin/content/{ty}/new/",
            get(content::new_form).post(content::create),
        )
        .route(
            "/admin/content/{ty}/{id}/",
            get(content::edit_form).post(content::save),
        )
        .route(
            "/admin/content/{ty}/{id}/delete/",
            get(content::delete_form).post(content::delete),
        )
        .route(
            "/admin/content/{ty}/{id}/revisions/",
            get(content::revisions),
        )
        .route(
            "/admin/content/{ty}/{id}/revisions/{version}/",
            get(content::revision),
        )
        .route(
            "/admin/content/{ty}/{id}/revisions/{version}/restore/",
            post(content::restore),
        )
        .route("/admin/taxonomy/", get(content::vocabularies))
        .route("/admin/taxonomy/{vocab}/", get(content::terms))
        .route("/admin/taxonomy/{vocab}/{id}/", post(content::rename_term))
        .route(
            "/admin/taxonomy/{vocab}/{id}/delete/",
            post(content::delete_term),
        )
        .route("/admin/users/", get(people::users))
        .route(
            "/admin/users/new/",
            get(people::new_user_form).post(people::create_user),
        )
        .route(
            "/admin/users/{id}/",
            get(people::user_form).post(people::save_user),
        )
        .route("/admin/users/{id}/reset/", post(people::send_reset))
        .route("/admin/users/{id}/totp-off/", post(people::user_totp_off))
        .route("/admin/profile/", get(people::profile))
        .route("/admin/profile/password/", post(people::change_password))
        .route("/admin/profile/totp/", post(people::enable_totp))
        .route("/admin/profile/totp/disable/", post(people::disable_totp));
    #[cfg(feature = "roles")]
    let r = r
        .route("/admin/roles/", get(people::roles))
        .route(
            "/admin/roles/new/",
            get(people::new_role_form).post(people::create_role),
        )
        .route(
            "/admin/roles/{name}/",
            get(people::role_form).post(people::save_role),
        )
        .route("/admin/roles/{name}/delete/", post(people::delete_role));
    r.with_state(admin)
}
