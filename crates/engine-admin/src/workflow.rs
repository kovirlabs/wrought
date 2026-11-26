//! The `workflow` feature: draft → review → published, preview and
//! notifications.
//!
//! * An editor who may edit but not publish a type submits a draft for
//!   review (with a note). Everyone who may publish that type is notified
//!   (by email with the `mail` feature, else in the server log).
//! * A reviewer previews the draft rendered by the site's own templates
//!   (`/admin/content/<type>/<id>/preview/`), then approves it (it is
//!   published, version-checked like any save) or sends it back with a
//!   note. The submitter is notified either way.
//! * `/admin/review/` lists what waits for the signed-in user.
//!
//! An item under review is still a draft in its content table; the
//! `engine_workflow` row marks it, and `engine_workflow_log` keeps the
//! history.

use std::future::Future;
use std::pin::Pin;

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use engine_auth::notify::Message;
use engine_auth::web::{FormData, see_other};
use engine_auth::{Action, Session};
use engine_content::store::{Changes, Item, Key, SaveError, Status};
use minijinja::{Value, context};

use crate::Admin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Renders a draft with the site's templates (implemented by the CLI,
/// which owns the renderer).
pub trait Preview: Send + Sync {
    /// The page HTML, or `None` if the item's type has no page.
    fn render<'a>(
        &'a self,
        item: &'a Item,
        headers: &'a HeaderMap,
    ) -> BoxFuture<'a, anyhow::Result<Option<String>>>;
}

fn pool(a: &Admin) -> &sqlx::PgPool {
    match a.auth().db() {
        engine_db::Db::Postgres(p) => p,
    }
}

struct Waiting {
    submitted_by: Option<i64>,
    by: Option<String>,
    submitted_at: String,
    note: String,
}

async fn waiting(a: &Admin, ty: &str, id: i64) -> anyhow::Result<Option<Waiting>> {
    Ok(sqlx::query_as!(
        Waiting,
        r#"SELECT w.submitted_by, coalesce(nullif(u.name, ''), u.email) AS by,
             to_char(w.submitted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "submitted_at!",
             w.note
           FROM engine_workflow w LEFT JOIN engine_users u ON u.id = w.submitted_by
           WHERE w.item_type = $1 AND w.item_id = $2"#,
        ty,
        id
    )
    .fetch_optional(pool(a))
    .await?)
}

async fn log(
    a: &Admin,
    ty: &str,
    id: i64,
    action: &str,
    user: i64,
    note: &str,
) -> anyhow::Result<()> {
    sqlx::query!(
        r#"INSERT INTO engine_workflow_log (item_type, item_id, action, user_id, note)
           VALUES ($1, $2, $3, $4, $5)"#,
        ty,
        id,
        action,
        user,
        note
    )
    .execute(pool(a))
    .await?;
    Ok(())
}

/// The workflow box of the edit page.
pub(crate) async fn item_state(
    a: &Admin,
    s: &Session,
    ty: &str,
    item: &Item,
) -> anyhow::Result<Value> {
    let w = waiting(a, ty, item.id).await?;
    let published = item.status == Status::Published;
    if published && w.is_some() {
        // Published by other means meanwhile: the review is over.
        sqlx::query!(
            "DELETE FROM engine_workflow WHERE item_type = $1 AND item_id = $2",
            ty,
            item.id
        )
        .execute(pool(a))
        .await?;
    }
    let w = w.filter(|_| !published);
    let can_publish = s.access.can(ty, Action::Publish);
    let can_edit = s.access.can(ty, Action::Edit);
    let history = sqlx::query!(
        r#"SELECT l.action, coalesce(nullif(u.name, ''), u.email) AS by, l.note,
             to_char(l.at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "at!"
           FROM engine_workflow_log l LEFT JOIN engine_users u ON u.id = l.user_id
           WHERE l.item_type = $1 AND l.item_id = $2 ORDER BY l.id DESC LIMIT 10"#,
        ty,
        item.id
    )
    .fetch_all(pool(a))
    .await?
    .into_iter()
    .map(|r| context! { action => r.action, by => r.by, note => r.note, at => r.at })
    .collect::<Vec<_>>();
    Ok(context! {
        state => if published { "published" } else if w.is_some() { "review" } else { "draft" },
        by => w.as_ref().and_then(|w| w.by.clone()),
        at => w.as_ref().map(|w| w.submitted_at.clone()),
        note => w.as_ref().map(|w| w.note.clone()),
        can_submit => !published && w.is_none() && can_edit && !can_publish,
        can_review => !published && w.is_some() && can_publish,
        can_withdraw => w.as_ref().is_some_and(|w| w.submitted_by == Some(s.user.id)),
        preview_url => format!("/admin/content/{ty}/{}/preview/", item.id),
        history => history,
    })
}

async fn load(a: &Admin, s: &Session, ty: &str, id: i64) -> Result<Item, Response> {
    if a.store().registry().get(ty).is_none() || !s.access.can(ty, Action::View) {
        return Err(a.not_found(s));
    }
    match a.store().get(ty, &Key::Id(id), None).await {
        Ok(Some(i)) => Ok(i),
        Ok(None) => Err(a.not_found(s)),
        Err(e) => Err(a.error(s, e)),
    }
}

/// `GET /admin/content/<type>/<id>/preview/`: the item, draft or not, in
/// the site's own page template.
pub(crate) async fn preview(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let item = match load(&a, &s, &ty, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    let Some(p) = a.preview() else {
        return a.not_found(&s);
    };
    match p.render(&item, &headers).await {
        Ok(Some(html)) => {
            let banner = format!(
                "<p role=\"status\"><strong>Preview</strong> of {} version {} ({}). Not visible on the site{}. <a href=\"/admin/content/{ty}/{id}/\">Back to the editor</a></p>",
                if item.status == Status::Published {
                    "the published"
                } else {
                    "a draft,"
                },
                item.version,
                item.status.as_str(),
                if item.status == Status::Published {
                    " as shown here until saved"
                } else {
                    ""
                },
            );
            let html = match html.find("<body") {
                Some(i) => match html[i..].find('>') {
                    Some(j) => format!("{}{banner}{}", &html[..i + j + 1], &html[i + j + 1..]),
                    None => html,
                },
                None => format!("{banner}{html}"),
            };
            let mut r = (StatusCode::OK, html).into_response();
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            h.insert(
                "x-robots-tag",
                HeaderValue::from_static("noindex, nofollow"),
            );
            h.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            h.insert(
                header::X_FRAME_OPTIONS,
                HeaderValue::from_static("SAMEORIGIN"),
            );
            r
        }
        Ok(None) => a.message(
            &s,
            StatusCode::NOT_FOUND,
            "No preview",
            "This type has no page of its own on the site, so there is nothing to preview.",
        ),
        Err(e) => a.error(&s, e),
    }
}

/// Everyone (else) who may publish `ty`.
async fn reviewers(a: &Admin, ty: &str, except: i64) -> anyhow::Result<Vec<engine_auth::User>> {
    let mut out = Vec::new();
    for u in a.auth().users().await? {
        if !u.active || u.id == except {
            continue;
        }
        if a.auth().access(&u).await?.can(ty, Action::Publish) {
            out.push(u);
        }
    }
    Ok(out)
}

async fn notify(a: &Admin, to: &engine_auth::User, template: &str, data: serde_json::Value) {
    let msg = Message {
        to: to.email.clone(),
        to_name: to.name.clone(),
        template: template.into(),
        data,
    };
    if let Err(e) = a.auth().notifier().send(msg).await {
        eprintln!("workflow: notifying {}: {e:#}", to.email);
    }
}

/// `POST /admin/content/<type>/<id>/review/` with `action` = `submit`,
/// `approve`, `reject` or `withdraw`.
pub(crate) async fn review(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let item = match load(&a, &s, &ty, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    let t = a.store().registry().get(&ty).cloned().expect("checked");
    let title = crate::content::title_of(&t, &item);
    let note: String = form
        .get("note")
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(2000)
        .collect();
    let base = a.auth().public_url().to_string();
    let edit_url = format!("{base}/admin/content/{ty}/{id}/");
    let w = match waiting(&a, &ty, id).await {
        Ok(w) => w,
        Err(e) => return a.error(&s, e),
    };
    let me = s.user.display_name().to_string();
    match form.get("action") {
        Some("submit") => {
            if !s.access.can(&ty, Action::Edit) {
                return a.forbidden(&s, &format!("edit {} items", ty));
            }
            if item.status == Status::Published {
                return a.message(
                    &s,
                    StatusCode::CONFLICT,
                    "Already published",
                    "This item is already on the site.",
                );
            }
            let r = sqlx::query!(
                r#"INSERT INTO engine_workflow (item_type, item_id, submitted_by, note)
                   VALUES ($1, $2, $3, $4)
                   ON CONFLICT (item_type, item_id) DO UPDATE
                   SET submitted_by = EXCLUDED.submitted_by, note = EXCLUDED.note, submitted_at = now()"#,
                ty,
                id,
                s.user.id,
                note
            )
            .execute(pool(&a))
            .await;
            if let Err(e) = r {
                return a.error(&s, e);
            }
            let _ = log(&a, &ty, id, "submit", s.user.id, &note).await;
            match reviewers(&a, &ty, s.user.id).await {
                Ok(list) => {
                    for u in list {
                        notify(
                            &a,
                            &u,
                            "workflow_review",
                            serde_json::json!({
                                "item_title": title, "type_label": t.label, "submitted_by": me,
                                "note": note, "review_url": edit_url,
                                "preview_url": format!("{edit_url}preview/"),
                            }),
                        )
                        .await;
                    }
                }
                Err(e) => eprintln!("workflow: finding reviewers: {e:#}"),
            }
            see_other(&format!("/admin/content/{ty}/{id}/?notice=submitted"))
        }
        Some(action @ ("approve" | "reject")) => {
            if !s.access.can(&ty, Action::Publish) {
                return a.forbidden(&s, &format!("publish {} items", ty));
            }
            let Some(w) = w else {
                return a.message(
                    &s,
                    StatusCode::CONFLICT,
                    "Not in review",
                    "This item is not waiting for review (any more).",
                );
            };
            if action == "approve" {
                let Some(version) = form.get("_version").and_then(|v| v.parse::<i32>().ok()) else {
                    return a.message(
                        &s,
                        StatusCode::BAD_REQUEST,
                        "Form incomplete",
                        "Reload the page and try again.",
                    );
                };
                match a
                    .store()
                    .update(
                        &ty,
                        id,
                        version,
                        &Changes::default().status(Status::Published),
                        Some(&Admin::author(&s)),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(SaveError::Conflict { .. }) => {
                        return a.message(
                            &s,
                            StatusCode::CONFLICT,
                            "Changed meanwhile",
                            "Someone saved this item after you opened it, so it was not published. Reload it, check the changes and approve again.",
                        );
                    }
                    Err(e) => return a.error(&s, e),
                }
            }
            if let Err(e) = sqlx::query!(
                "DELETE FROM engine_workflow WHERE item_type = $1 AND item_id = $2",
                ty,
                id
            )
            .execute(pool(&a))
            .await
            {
                return a.error(&s, e);
            }
            let _ = log(&a, &ty, id, action, s.user.id, &note).await;
            if let Some(uid) = w.submitted_by
                && uid != s.user.id
                && let Ok(Some(u)) = a.auth().user(uid).await
            {
                notify(
                    &a,
                    &u,
                    "workflow_decision",
                    serde_json::json!({
                        "item_title": title, "type_label": t.label, "decided_by": me,
                        "approved": action == "approve", "note": note, "edit_url": edit_url,
                    }),
                )
                .await;
            }
            let notice = if action == "approve" {
                "approved"
            } else {
                "returned"
            };
            see_other(&format!("/admin/content/{ty}/{id}/?notice={notice}"))
        }
        Some("withdraw") => {
            if w.is_none_or(|w| w.submitted_by != Some(s.user.id)) {
                return a.forbidden(&s, "withdraw someone else's request");
            }
            let _ = sqlx::query!(
                "DELETE FROM engine_workflow WHERE item_type = $1 AND item_id = $2",
                ty,
                id
            )
            .execute(pool(&a))
            .await;
            let _ = log(&a, &ty, id, "withdraw", s.user.id, "").await;
            see_other(&format!("/admin/content/{ty}/{id}/?notice=withdrawn"))
        }
        _ => a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Unknown action",
            "Reload the page and try again.",
        ),
    }
}

/// `GET /admin/review/`: drafts waiting for review that this user may
/// publish, and their own requests.
pub(crate) async fn queue(State(a): State<Admin>, headers: HeaderMap, uri: Uri) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rows = match sqlx::query!(
        r#"SELECT w.item_type, w.item_id, w.submitted_by, coalesce(nullif(u.name, ''), u.email) AS by,
             to_char(w.submitted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "at!", w.note
           FROM engine_workflow w LEFT JOIN engine_users u ON u.id = w.submitted_by
           ORDER BY w.submitted_at"#
    )
    .fetch_all(pool(&a))
    .await
    {
        Ok(r) => r,
        Err(e) => return a.error(&s, e),
    };
    let mut items = Vec::new();
    for r in rows {
        let Some(t) = a.store().registry().get(&r.item_type).cloned() else {
            continue;
        };
        let mine = r.submitted_by == Some(s.user.id);
        if !(s.access.can(&t.name, Action::Publish) || mine) {
            continue;
        }
        let Ok(Some(item)) = a.store().get(&t.name, &Key::Id(r.item_id), None).await else {
            continue;
        };
        if item.status == Status::Published {
            continue;
        }
        items.push(context! {
            ty => t.name, type_label => t.label, id => item.id,
            title => crate::content::title_of(&t, &item),
            by => r.by, at => r.at, note => r.note, mine,
        });
    }
    a.page(
        StatusCode::OK,
        "admin/review.html",
        a.base(&s, "Waiting for review"),
        context! { items },
    )
}
