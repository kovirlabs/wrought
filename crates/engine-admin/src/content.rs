//! Content, revisions and taxonomy pages.

use std::collections::BTreeMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use engine_auth::web::{FormData, see_other, url_encode};
use engine_auth::{Access, Action, FieldAccess, Session};
use engine_content::store::{Item, Key, ListQuery, SaveError, Status};
use engine_content::types::TypeDef;
use minijinja::{Value, context};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::forms::{self, Parsed, Refused, Values};
use crate::{Admin, PER_PAGE, Pager};

#[derive(Deserialize, Default)]
pub(crate) struct ListQ {
    page: Option<i64>,
    q: Option<String>,
    status: Option<String>,
    notice: Option<String>,
}

pub(crate) fn title_of(t: &TypeDef, item: &Item) -> String {
    t.title_field
        .as_ref()
        .and_then(|f| item.fields.get(f))
        .and_then(Json::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| item.slug.clone())
}

fn edit_url(ty: &str, id: i64) -> String {
    format!("/admin/content/{ty}/{id}/")
}

/// Choices for relation fields: every item of each target type.
async fn relation_options(
    a: &Admin,
    t: &TypeDef,
    access: &Access,
) -> anyhow::Result<BTreeMap<String, Vec<(String, String)>>> {
    let mut out = BTreeMap::new();
    for f in &t.fields {
        let Some(to) = f.to.as_deref() else { continue };
        if out.contains_key(to) || access.field(&t.name, &f.name, Action::View) == FieldAccess::None
        {
            continue;
        }
        let Some(tt) = a.store().registry().get(to) else {
            continue;
        };
        let items = a
            .store()
            .list(
                to,
                &ListQuery {
                    status: None,
                    limit: Some(500),
                    order: "title".into(),
                    ..Default::default()
                },
            )
            .await?;
        let opts = items
            .iter()
            .map(|i| {
                let mut label = title_of(tt, i);
                if i.status == Status::Draft {
                    label.push_str(" (draft)");
                }
                (i.id.to_string(), label)
            })
            .collect();
        out.insert(to.to_string(), opts);
    }
    #[cfg(feature = "media")]
    if let Some(m) = a.media()
        && t.fields
            .iter()
            .any(|f| f.kind == engine_content::types::FieldKind::Media)
    {
        let files = m.list(false, None, 500, 0).await?;
        let opts = files
            .iter()
            .map(|f| {
                let what = if f.alt.is_empty() { &f.name } else { &f.alt };
                (f.id.to_string(), format!("#{} {what}", f.id))
            })
            .collect();
        out.insert(crate::forms::MEDIA_OPTIONS.to_string(), opts);
    }
    Ok(out)
}

pub(crate) async fn dashboard(State(a): State<Admin>, headers: HeaderMap, uri: Uri) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut types = Vec::new();
    for t in a.store().registry().types() {
        if !s.access.can(&t.name, Action::View) {
            continue;
        }
        let all = ListQuery {
            status: None,
            ..Default::default()
        };
        let drafts = ListQuery {
            status: Some(Status::Draft),
            ..Default::default()
        };
        let (n, d) = match (
            a.store().count(&t.name, &all).await,
            a.store().count(&t.name, &drafts).await,
        ) {
            (Ok(n), Ok(d)) => (n, d),
            (Err(e), _) | (_, Err(e)) => return a.error(&s, e),
        };
        types.push(context! {
            name => t.name, label => t.label, singleton => t.singleton, count => n, drafts => d,
            can_create => s.access.can(&t.name, Action::Create),
        });
    }
    a.page(
        StatusCode::OK,
        "admin/dashboard.html",
        a.base(&s, "Admin"),
        context! { types },
    )
}

/// The type named in the path, if the user may view it.
fn viewable<'a>(a: &'a Admin, s: &Session, ty: &str) -> Result<&'a TypeDef, Response> {
    match a.store().registry().get(ty) {
        Some(t) if s.access.can(&t.name, Action::View) => Ok(t),
        Some(_) => Err(a.forbidden(s, &format!("see {ty} items"))),
        None => Err(a.not_found(s)),
    }
}

pub(crate) async fn list(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(ty): Path<String>,
    Query(q): Query<ListQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if t.singleton {
        return match a.store().singleton(&t.name).await {
            Ok(Some(i)) => see_other(&edit_url(&ty, i.id)),
            Ok(None) => see_other(&format!("/admin/content/{ty}/new/")),
            Err(e) => a.error(&s, e),
        };
    }
    let status = q.status.as_deref().and_then(Status::parse);
    let search = q.q.clone().filter(|x| !x.trim().is_empty());
    let mut lq = ListQuery {
        status,
        order: "updated".into(),
        search: search.clone(),
        ..Default::default()
    };
    let total = match a.store().count(&t.name, &lq).await {
        Ok(n) => n,
        Err(e) => return a.error(&s, e),
    };
    let href = |p: i64| {
        let mut h = format!("/admin/content/{ty}/?page={p}");
        if let Some(st) = status {
            h.push_str(&format!("&status={}", st.as_str()));
        }
        if let Some(x) = &search {
            h.push_str(&format!("&q={}", url_encode(x)));
        }
        h
    };
    let pager = Pager::new(q.page.unwrap_or(1), total, href);
    lq.limit = Some(PER_PAGE);
    lq.offset = pager.offset();
    let items = match a.store().list(&t.name, &lq).await {
        Ok(i) => i,
        Err(e) => return a.error(&s, e),
    };
    let rows: Vec<Value> = items
        .iter()
        .map(|i| {
            context! {
                id => i.id, title => title_of(&t, i), slug => i.slug, status => i.status.as_str(),
                version => i.version, updated_at => i.updated_at, edit => edit_url(&ty, i.id),
                url => a.site().item_path(i),
            }
        })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/list.html",
        a.base(&s, &t.label),
        context! {
            ty => context! { name => t.name, label => t.label },
            rows, pager, q => search.unwrap_or_default(),
            status => status.map(|x| x.as_str()),
            can_create => s.access.can(&t.name, Action::Create),
            notice => q.notice,
        },
    )
}

/// Render the edit (or new) form.
#[allow(clippy::too_many_arguments)]
async fn form_page(
    a: &Admin,
    s: &Session,
    t: &TypeDef,
    item: Option<&Item>,
    values: &Values,
    version: Option<i32>,
    errors: &BTreeMap<String, String>,
    status: StatusCode,
    extra: Value,
) -> Response {
    let action = if item.is_some() {
        Action::Edit
    } else {
        Action::Create
    };
    let rel = match relation_options(a, t, &s.access).await {
        Ok(r) => r,
        Err(e) => return a.error(s, e),
    };
    let fields = forms::views(
        a.store().registry(),
        t,
        &s.access,
        action,
        values,
        errors,
        &rel,
    );
    let editable = s.access.can(&t.name, action);
    let form_status = values
        .get("_status")
        .and_then(Json::as_str)
        .or(item.map(|i| i.status.as_str()))
        .unwrap_or("draft");
    let title = match item {
        Some(i) => format!("{}: {}", t.label, title_of(t, i)),
        None => format!("New {}", t.label.to_lowercase()),
    };
    #[cfg(feature = "workflow")]
    let workflow = match item {
        Some(i) => match crate::workflow::item_state(a, s, &t.name, i).await {
            Ok(v) => Some(v),
            Err(e) => return a.error(s, e),
        },
        None => None,
    };
    #[cfg(not(feature = "workflow"))]
    let workflow: Option<Value> = None;
    let ctx = context! {
        ty => context! { name => t.name, label => t.label, singleton => t.singleton },
        item => item.map(|i| context! {
            id => i.id, slug => i.slug, status => i.status.as_str(), version => i.version,
            updated_at => i.updated_at, url => a.site().item_path(i),
        }),
        fields,
        version,
        slug => values.get("_slug").and_then(Json::as_str).map(str::to_string)
            .or(item.map(|i| i.slug.clone())),
        form_status,
        editable,
        can_publish => s.access.can(&t.name, Action::Publish),
        can_delete => item.is_some() && s.access.can(&t.name, Action::Delete),
        workflow,
        visual_editor => cfg!(feature = "visual-editor")
            && t.fields.iter().any(|f| f.kind == engine_content::types::FieldKind::Blocks
                && s.access.field(&t.name, &f.name, Action::Edit) != FieldAccess::None),
        errors => errors.clone(),
        action_url => match item {
            Some(i) => edit_url(&t.name, i.id),
            None => format!("/admin/content/{}/new/", t.name),
        },
    };
    a.page(
        status,
        "admin/edit.html",
        a.base(s, &title),
        crate::merge(ctx, extra),
    )
}

fn refused(a: &Admin, s: &Session, r: Refused) -> Response {
    let Refused::Forbidden(fields) = r;
    a.message(
        s,
        StatusCode::FORBIDDEN,
        "Not allowed",
        &format!(
            "Nothing was saved: your roles do not allow you to change {}.",
            fields
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

fn validation_errors(e: &engine_content::value::ValidationErrors) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (f, msg) in &e.0 {
        m.entry(f.clone()).or_insert_with(|| msg.clone());
    }
    m
}

#[derive(Deserialize, Default)]
pub(crate) struct NoticeQ {
    notice: Option<String>,
}

pub(crate) async fn new_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(ty): Path<String>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Create) {
        return a.forbidden(&s, &format!("create {} items", t.name));
    }
    if t.singleton
        && let Ok(Some(i)) = a.store().singleton(&t.name).await
    {
        return see_other(&edit_url(&ty, i.id));
    }
    form_page(
        &a,
        &s,
        &t,
        None,
        &Values::new(),
        None,
        &BTreeMap::new(),
        StatusCode::OK,
        context! {},
    )
    .await
}

pub(crate) async fn create(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(ty): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Create) {
        return a.forbidden(&s, &format!("create {} items", t.name));
    }
    let reg = a.store().registry();
    let mut p = match forms::parse(reg, &t, &s.access, Action::Create, &form, None) {
        Ok(p) => p,
        Err(r) => return refused(&a, &s, r),
    };
    if let Some(st) = form.get("_status") {
        p.values.insert("_status".into(), Json::String(st.into()));
    }
    if let Some(sl) = form.get("_slug") {
        p.values.insert("_slug".into(), Json::String(sl.into()));
    }
    if p.action.is_some() {
        forms::apply_block_action(reg, &t, &s.access, Action::Create, &mut p, &form);
        return form_page(
            &a,
            &s,
            &t,
            None,
            &p.values,
            None,
            &p.errors,
            StatusCode::OK,
            context! {},
        )
        .await;
    }
    if !p.errors.is_empty() {
        return form_page(
            &a,
            &s,
            &t,
            None,
            &p.values,
            None,
            &p.errors,
            StatusCode::UNPROCESSABLE_ENTITY,
            context! {},
        )
        .await;
    }
    match a
        .store()
        .create(&t.name, &p.changes, Some(&Admin::author(&s)))
        .await
    {
        Ok(item) => see_other(&format!("{}?notice=created", edit_url(&ty, item.id))),
        Err(SaveError::Invalid(e)) => {
            form_page(
                &a,
                &s,
                &t,
                None,
                &p.values,
                None,
                &validation_errors(&e),
                StatusCode::UNPROCESSABLE_ENTITY,
                context! {},
            )
            .await
        }
        Err(e) => a.error(&s, e),
    }
}

async fn load_item(a: &Admin, s: &Session, t: &TypeDef, id: i64) -> Result<Item, Response> {
    match a.store().get(&t.name, &Key::Id(id), None).await {
        Ok(Some(i)) => Ok(i),
        Ok(None) => Err(a.not_found(s)),
        Err(e) => Err(a.error(s, e)),
    }
}

fn current_values(t: &TypeDef, item: &Item) -> Values {
    let mut v = forms::item_values(t, item);
    v.insert("_status".into(), Json::String(item.status.as_str().into()));
    v
}

pub(crate) async fn edit_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
    Query(q): Query<NoticeQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    let item = match load_item(&a, &s, &t, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    form_page(
        &a,
        &s,
        &t,
        Some(&item),
        &current_values(&t, &item),
        Some(item.version),
        &BTreeMap::new(),
        StatusCode::OK,
        context! { notice => q.notice },
    )
    .await
}

/// One row of the conflict screen.
#[derive(serde::Serialize)]
struct ConflictRow {
    label: String,
    yours: String,
    theirs: String,
    /// The other editor changed this field too (yours and theirs both moved
    /// away from the version you started from).
    both: bool,
}

pub(crate) async fn save(
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
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Edit) {
        return a.forbidden(&s, &format!("edit {} items", t.name));
    }
    let current = match load_item(&a, &s, &t, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    let reg = a.store().registry();
    let cur_values = current_values(&t, &current);
    let mut p = match forms::parse(reg, &t, &s.access, Action::Edit, &form, Some(&cur_values)) {
        Ok(p) => p,
        Err(r) => return refused(&a, &s, r),
    };
    if let Some(st) = form.get("_status") {
        p.values.insert("_status".into(), Json::String(st.into()));
    }
    if let Some(sl) = form.get("_slug") {
        p.values.insert("_slug".into(), Json::String(sl.into()));
    }
    let Some(version) = p.version else {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Form incomplete",
            "This form did not say which version it edited, so it was not saved. Reload the page and try again.",
        );
    };
    // A slug the user did not change is not a change.
    if p.changes.slug.as_deref() == Some(current.slug.as_str()) {
        p.changes.slug = None;
    }
    if p.changes.status == Some(current.status) {
        p.changes.status = None;
    }
    if p.action.is_some() {
        forms::apply_block_action(reg, &t, &s.access, Action::Edit, &mut p, &form);
        return form_page(
            &a,
            &s,
            &t,
            Some(&current),
            &p.values,
            Some(version),
            &p.errors,
            StatusCode::OK,
            context! {},
        )
        .await;
    }
    if !p.errors.is_empty() {
        return form_page(
            &a,
            &s,
            &t,
            Some(&current),
            &p.values,
            Some(version),
            &p.errors,
            StatusCode::UNPROCESSABLE_ENTITY,
            context! {},
        )
        .await;
    }
    match a
        .store()
        .update(&t.name, id, version, &p.changes, Some(&Admin::author(&s)))
        .await
    {
        Ok(_) => see_other(&format!("{}?notice=saved", edit_url(&ty, id))),
        Err(SaveError::Conflict {
            expected, current: now, ..
        }) => conflict(&a, &s, &t, id, expected, now, &p).await,
        Err(SaveError::Invalid(e)) => {
            form_page(
                &a,
                &s,
                &t,
                Some(&current),
                &p.values,
                Some(version),
                &validation_errors(&e),
                StatusCode::UNPROCESSABLE_ENTITY,
                context! {},
            )
            .await
        }
        Err(SaveError::NotFound { .. }) => a.message(
            &s,
            StatusCode::CONFLICT,
            "Deleted meanwhile",
            "Someone deleted this item while you were editing it, so your changes could not be saved.",
        ),
        Err(e) => a.error(&s, e),
    }
}

/// The conflict screen: nothing was saved; show what differs between the
/// editor's version and the current one, and the form again with the
/// editor's values and the *current* version, so "save" now knowingly
/// replaces it.
async fn conflict(
    a: &Admin,
    s: &Session,
    t: &TypeDef,
    id: i64,
    expected: i32,
    now: i32,
    p: &Parsed,
) -> Response {
    let current = match load_item(a, s, t, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    let revisions = a.store().revisions(&t.name, id).await.unwrap_or_default();
    let base: Option<Values> = revisions
        .iter()
        .find(|r| r.version == expected)
        .and_then(|r| serde_json::from_value::<Item>(r.data.clone()).ok())
        .map(|i| forms::item_values(t, &i));
    let latest = revisions.iter().find(|r| r.version == current.version);
    let theirs = forms::item_values(t, &current);
    let mut rows = Vec::new();
    for f in &t.fields {
        if s.access.field(&t.name, &f.name, Action::View) == FieldAccess::None {
            continue;
        }
        let Some(mine) = p.values.get(&f.name) else {
            continue;
        };
        if !p.changes.fields.iter().any(|(n, _)| n == &f.name) {
            continue;
        }
        let (y, th) = (
            forms::display(f.kind, mine),
            forms::display(f.kind, theirs.get(&f.name).unwrap_or(&Json::Null)),
        );
        if y.trim() == th.trim() {
            continue;
        }
        let b = base
            .as_ref()
            .map(|b| forms::display(f.kind, b.get(&f.name).unwrap_or(&Json::Null)));
        rows.push(ConflictRow {
            label: forms::label_of(f),
            both: b
                .as_ref()
                .is_some_and(|b| b.trim() != th.trim() && b.trim() != y.trim()),
            yours: y,
            theirs: th,
        });
    }
    let ctx = context! {
        conflict => context! {
            expected, current => now, rows,
            by => latest.and_then(|r| r.author.clone()),
            at => latest.map(|r| r.created_at.clone()),
            status_changed => current.status.as_str() != p.values.get("_status").and_then(Json::as_str).unwrap_or(current.status.as_str()),
        },
    };
    form_page(
        a,
        s,
        t,
        Some(&current),
        &p.values,
        Some(now),
        &p.errors,
        StatusCode::CONFLICT,
        ctx,
    )
    .await
}

pub(crate) async fn delete_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Delete) {
        return a.forbidden(&s, &format!("delete {} items", t.name));
    }
    let item = match load_item(&a, &s, &t, id).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    a.page(
        StatusCode::OK,
        "admin/delete.html",
        a.base(&s, &format!("Delete {}", title_of(&t, &item))),
        context! {
            ty => context! { name => t.name, label => t.label },
            item => context! { id => item.id, title => title_of(&t, &item), version => item.version,
                               status => item.status.as_str(), url => a.site().item_path(&item) },
        },
    )
}

pub(crate) async fn delete(
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
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Delete) {
        return a.forbidden(&s, &format!("delete {} items", t.name));
    }
    let Some(version) = form.get("_version").and_then(|v| v.parse::<i32>().ok()) else {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Form incomplete",
            "The form did not say which version to delete.",
        );
    };
    match a
        .store()
        .delete(&t.name, id, Some(version), Some(&Admin::author(&s)))
        .await
    {
        Ok(()) | Err(SaveError::NotFound { .. }) => {
            see_other(&format!("/admin/content/{ty}/?notice=deleted"))
        }
        Err(SaveError::Conflict { current, .. }) => a.page(
            StatusCode::CONFLICT,
            "admin/message.html",
            a.base(&s, "Not deleted"),
            context! {
                message => format!("Someone saved this item (now version {current}) after you opened the delete page, so it was not deleted. Look at the current version first."),
                link => context! { url => edit_url(&ty, id), label => "Open the current version" },
            },
        ),
        Err(e) => a.error(&s, e),
    }
}

/// Fields of a stored item the user may see, for read-only display.
fn readable_fields(t: &TypeDef, access: &Access, item: &Item) -> Vec<Value> {
    let values = forms::item_values(t, item);
    t.fields
        .iter()
        .filter(|f| access.field(&t.name, &f.name, Action::View) != FieldAccess::None)
        .map(|f| {
            context! {
                label => forms::label_of(f),
                kind => f.kind.as_str(),
                value => forms::display(f.kind, values.get(&f.name).unwrap_or(&Json::Null)),
            }
        })
        .collect()
}

pub(crate) async fn revisions(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id)): Path<(String, i64)>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    let revs = match a.store().revisions(&t.name, id).await {
        Ok(r) => r,
        Err(e) => return a.error(&s, e),
    };
    if revs.is_empty() {
        return a.not_found(&s);
    }
    let item = a
        .store()
        .get(&t.name, &Key::Id(id), None)
        .await
        .ok()
        .flatten();
    let title = item
        .as_ref()
        .map(|i| title_of(&t, i))
        .unwrap_or_else(|| format!("{} #{id}", t.label));
    let rows: Vec<Value> = revs
        .iter()
        .map(|r| {
            context! {
                version => r.version, op => r.op, author => r.author, created_at => r.created_at,
                current => item.as_ref().is_some_and(|i| i.version == r.version),
                href => format!("/admin/content/{ty}/{id}/revisions/{}/", r.version),
            }
        })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/revisions.html",
        a.base(&s, &format!("History: {title}")),
        context! {
            ty => context! { name => t.name, label => t.label },
            id, item_title => title, exists => item.is_some(), rows,
        },
    )
}

pub(crate) async fn revision(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id, version)): Path<(String, i64, i32)>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    let rev = match a.store().revision(&t.name, id, version).await {
        Ok(Some(r)) => r,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let Ok(old) = serde_json::from_value::<Item>(rev.data.clone()) else {
        return a.error(&s, "revision data does not match the item format");
    };
    let current = a
        .store()
        .get(&t.name, &Key::Id(id), None)
        .await
        .ok()
        .flatten();
    a.page(
        StatusCode::OK,
        "admin/revision.html",
        a.base(&s, &format!("{} version {version}", title_of(&t, &old))),
        context! {
            ty => context! { name => t.name, label => t.label },
            id, version, op => rev.op, author => rev.author, created_at => rev.created_at,
            slug => old.slug, status => old.status.as_str(),
            fields => readable_fields(&t, &s.access, &old),
            current_version => current.as_ref().map(|c| c.version),
            can_restore => current.as_ref().is_some_and(|c| c.version != version)
                && s.access.can(&t.name, Action::Edit) && rev.op != "delete",
        },
    )
}

pub(crate) async fn restore(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((ty, id, version)): Path<(String, i64, i32)>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let t = match viewable(&a, &s, &ty) {
        Ok(t) => t.clone(),
        Err(r) => return r,
    };
    if !s.access.can(&t.name, Action::Edit) {
        return a.forbidden(&s, &format!("edit {} items", t.name));
    }
    let Some(expected) = form.get("_version").and_then(|v| v.parse::<i32>().ok()) else {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Form incomplete",
            "The form did not say which version it replaces.",
        );
    };
    let rev = match a.store().revision(&t.name, id, version).await {
        Ok(Some(r)) => r,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let Ok(old) = serde_json::from_value::<Item>(rev.data) else {
        return a.error(&s, "revision data does not match the item format");
    };
    // Only fields the user may write come back; the rest stay as they are.
    let mut ch = engine_content::store::Changes::default();
    for (name, v) in old.input_fields(&t) {
        if s.access.field(&t.name, &name, Action::Edit) == FieldAccess::Write {
            ch.fields.push((name, v));
        }
    }
    match a
        .store()
        .update(&t.name, id, expected, &ch, Some(&Admin::author(&s)))
        .await
    {
        Ok(_) => see_other(&format!("{}?notice=restored", edit_url(&ty, id))),
        Err(SaveError::Conflict { current, .. }) => a.page(
            StatusCode::CONFLICT,
            "admin/message.html",
            a.base(&s, "Not restored"),
            context! {
                message => format!("Someone saved this item (now version {current}) after you opened this page, so version {version} was not restored. Look at the current version first."),
                link => context! { url => format!("/admin/content/{ty}/{id}/revisions/{version}/"), label => "Back to this version" },
            },
        ),
        Err(SaveError::Invalid(e)) => a.message(
            &s,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Not restored",
            &format!("Version {version} is no longer valid for this type: {e}"),
        ),
        Err(e) => a.error(&s, e),
    }
}

// ----- taxonomy --------------------------------------------------------------

fn taxonomy_allowed(a: &Admin, s: &Session) -> Result<(), Response> {
    if s.access.can_manage_taxonomy() {
        Ok(())
    } else {
        Err(a.forbidden(s, "manage taxonomy terms"))
    }
}

pub(crate) async fn vocabularies(State(a): State<Admin>, headers: HeaderMap, uri: Uri) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = taxonomy_allowed(&a, &s) {
        return r;
    }
    let mut vocabs = Vec::new();
    for v in a.store().registry().vocabularies() {
        let n = a.store().terms(v).await.map(|t| t.len()).unwrap_or(0);
        vocabs.push(context! { name => v, count => n });
    }
    a.page(
        StatusCode::OK,
        "admin/vocabularies.html",
        a.base(&s, "Taxonomy"),
        context! { vocabs },
    )
}

pub(crate) async fn terms(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(vocab): Path<String>,
    Query(q): Query<NoticeQ>,
) -> Response {
    let s = match a.require(&headers, &uri).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = taxonomy_allowed(&a, &s) {
        return r;
    }
    if !a.store().registry().is_vocabulary(&vocab) {
        return a.not_found(&s);
    }
    let terms = match a.store().terms(&vocab).await {
        Ok(t) => t,
        Err(e) => return a.error(&s, e),
    };
    let rows: Vec<Value> = terms
        .iter()
        .map(|t| context! { id => t.id, name => t.name, slug => t.slug, version => t.version, url => a.site().term_path(t) })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/terms.html",
        a.base(&s, &format!("Taxonomy: {vocab}")),
        context! { vocab, rows, notice => q.notice },
    )
}

fn term_result(
    a: &Admin,
    s: &Session,
    vocab: &str,
    r: Result<(), SaveError>,
    done: &str,
) -> Response {
    match r {
        Ok(()) => see_other(&format!("/admin/taxonomy/{vocab}/?notice={done}")),
        Err(SaveError::Conflict { .. }) => a.page(
            StatusCode::CONFLICT,
            "admin/message.html",
            a.base(s, "Not saved"),
            context! {
                message => "Someone changed this term after you opened the page, so your change was not saved.",
                link => context! { url => format!("/admin/taxonomy/{vocab}/"), label => "See the current terms" },
            },
        ),
        Err(SaveError::NotFound { .. }) => a.not_found(s),
        Err(SaveError::Invalid(e)) => a.message(s, StatusCode::UNPROCESSABLE_ENTITY, "Not saved", &e.to_string()),
        Err(e) => a.error(s, e),
    }
}

pub(crate) async fn rename_term(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((vocab, id)): Path<(String, i64)>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = taxonomy_allowed(&a, &s) {
        return r;
    }
    let version = form
        .get("_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let r = a
        .store()
        .rename_term(&vocab, id, version, form.get("name").unwrap_or(""))
        .await
        .map(|_| ());
    term_result(&a, &s, &vocab, r, "renamed")
}

pub(crate) async fn delete_term(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((vocab, id)): Path<(String, i64)>,
    Form(form): Form<FormData>,
) -> Response {
    let s = match a.require_post(&headers, &uri, &form).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = taxonomy_allowed(&a, &s) {
        return r;
    }
    let version = form
        .get("_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let r = a.store().delete_term(&vocab, id, version).await;
    term_result(&a, &s, &vocab, r, "deleted")
}
