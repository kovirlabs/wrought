//! Forms in the admin (features `admin` + `forms`): every form with its
//! submissions (list, view, delete, CSV export), and a form builder for
//! forms made here. Forms from the site's `forms/*.toml` (and the built-in
//! ones) are shown read-only: their file is the source of truth.
//! Needs the `forms` right (submissions hold personal data).

use std::collections::BTreeMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use engine_auth::Session;
use engine_auth::web::{FormData, see_other};
use engine_forms::def::{Field, FormDef, Kind, Notify, Receipt, Spam};
use engine_forms::{Forms, SaveError, Source};
use minijinja::context;
use serde::Deserialize;

use crate::{Admin, Pager};

#[derive(Deserialize, Default)]
pub(crate) struct PageQ {
    page: Option<i64>,
    notice: Option<String>,
}

async fn guard(a: &Admin, headers: &HeaderMap, uri: &Uri) -> Result<(Session, Forms), Response> {
    let s = a.require(headers, uri).await?;
    let Some(f) = a.forms().cloned() else {
        return Err(a.not_found(&s));
    };
    if !s.access.can_manage_forms() {
        return Err(a.forbidden(&s, "manage forms"));
    }
    Ok((s, f))
}

async fn guard_post(
    a: &Admin,
    headers: &HeaderMap,
    uri: &Uri,
    form: &FormData,
) -> Result<(Session, Forms), Response> {
    let s = a.require_post(headers, uri, form).await?;
    let Some(f) = a.forms().cloned() else {
        return Err(a.not_found(&s));
    };
    if !s.access.can_manage_forms() {
        return Err(a.forbidden(&s, "manage forms"));
    }
    Ok((s, f))
}

pub(crate) async fn list(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Query(q): Query<PageQ>,
) -> Response {
    let (s, forms) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    match forms.list().await {
        Ok(list) => a.page(
            StatusCode::OK,
            "admin/forms.html",
            a.base(&s, "Forms"),
            context! { forms => list, notice => q.notice },
        ),
        Err(e) => a.error(&s, e),
    }
}

/// Plain text of a stored value.
fn text(v: Option<&serde_json::Value>) -> String {
    match v {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::Bool(b)) => if *b { "Yes" } else { "No" }.into(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

pub(crate) async fn submissions(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(name): Path<String>,
    Query(q): Query<PageQ>,
) -> Response {
    let (s, forms) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let found = match forms.get(&name).await {
        Ok(Some(f)) => f,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    let counts: BTreeMap<String, i64> = match store.counts().await {
        Ok(c) => c.into_iter().collect(),
        Err(e) => return a.error(&s, e),
    };
    let total = counts.get(&name).copied().unwrap_or(0);
    let pager = Pager::new(q.page.unwrap_or(1), total, |p| {
        format!("/admin/forms/{name}/?page={p}")
    });
    let subs = match store
        .submissions(&name, crate::PER_PAGE, pager.offset())
        .await
    {
        Ok(v) => v,
        Err(e) => return a.error(&s, e),
    };
    let def = &found.def;
    let shown: Vec<&Field> = def.fields.iter().take(3).collect();
    let rows: Vec<minijinja::Value> = subs
        .iter()
        .map(|sub| {
            let cells: Vec<String> = shown
                .iter()
                .map(|f| {
                    let t = text(sub.data.get(&f.name));
                    let short: String = t.chars().take(80).collect();
                    if short.len() < t.len() {
                        format!("{short}…")
                    } else {
                        short
                    }
                })
                .collect();
            context! { id => sub.id, at => sub.created_at, cells, signed_in => sub.user_id.is_some() }
        })
        .collect();
    a.page(
        StatusCode::OK,
        "admin/form_submissions.html",
        a.base(&s, &format!("Form: {}", def.title)),
        context! {
            form => context! {
                name => name, title => def.title, source => found.source,
                builtin => engine_forms::is_builtin(&name) && found.source == Source::File,
                file => format!("forms/{name}.toml"),
                receipt => def.receipt.as_ref().map(|r| r.to_field.clone()),
                notify => def.notify.as_ref().map(|n| n.to.join(", ")),
            },
            columns => shown.iter().map(|f| f.label()).collect::<Vec<_>>(),
            rows, pager, notice => q.notice,
        },
    )
}

pub(crate) async fn submission(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((name, id)): Path<(String, i64)>,
) -> Response {
    let (s, forms) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    let sub = match store.submission(id).await {
        Ok(Some(sub)) if sub.form == name => sub,
        Ok(_) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let def = match forms.get(&name).await {
        Ok(f) => f.map(|f| f.def),
        Err(e) => return a.error(&s, e),
    };
    // Fields in the form's order; values of fields no longer in the form
    // after them.
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    if let Some(d) = &def {
        for f in &d.fields {
            if let Some(v) = sub.data.get(&f.name) {
                rows.push(row(&f.label(), &text(Some(v))));
                seen.push(f.name.clone());
            }
        }
    }
    if let serde_json::Value::Object(m) = &sub.data {
        for (k, v) in m {
            if !seen.contains(k) {
                rows.push(row(k, &text(Some(v))));
            }
        }
    }
    let user = match sub.user_id {
        Some(uid) => a.auth().user(uid).await.ok().flatten().map(|u| u.email),
        None => None,
    };
    a.page(
        StatusCode::OK,
        "admin/form_submission.html",
        a.base(&s, &format!("Submission #{id}")),
        context! { form => name, sub => context! { id => sub.id, at => sub.created_at, ip => sub.ip, user }, rows },
    )
}

/// A label and its value's lines (shown with line breaks).
fn row(label: &str, value: &str) -> minijinja::Value {
    let lines: Vec<&str> = value.lines().collect();
    context! { label => label, value => value, lines => lines }
}

pub(crate) async fn delete_submission(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path((name, id)): Path<(String, i64)>,
    Form(form): Form<FormData>,
) -> Response {
    let (s, forms) = match guard_post(&a, &headers, &uri, &form).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    match store.submission(id).await {
        Ok(Some(sub)) if sub.form == name => {}
        Ok(_) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    }
    match store.delete_submission(id).await {
        Ok(_) => see_other(&format!("/admin/forms/{name}/?notice=submission-deleted")),
        Err(e) => a.error(&s, e),
    }
}

/// A CSV cell. Values that a spreadsheet would run as a formula are
/// prefixed with `'` (CSV injection).
fn csv(v: &str) -> String {
    let v = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_string()
    };
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

pub(crate) async fn export(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(name): Path<String>,
) -> Response {
    let (s, forms) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let def = match forms.get(&name).await {
        Ok(Some(f)) => f.def,
        Ok(None) => return a.not_found(&s),
        Err(e) => return a.error(&s, e),
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    let mut out = String::new();
    let mut head = vec!["id".to_string(), "received".into()];
    head.extend(def.fields.iter().map(|f| f.name.clone()));
    out.push_str(&head.iter().map(|h| csv(h)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    let mut offset = 0;
    loop {
        let batch = match store.submissions(&name, 500, offset).await {
            Ok(b) => b,
            Err(e) => return a.error(&s, e),
        };
        if batch.is_empty() {
            break;
        }
        offset += batch.len() as i64;
        for sub in batch {
            let mut row = vec![sub.id.to_string(), sub.created_at.clone()];
            row.extend(def.fields.iter().map(|f| text(sub.data.get(&f.name))));
            out.push_str(&row.iter().map(|c| csv(c)).collect::<Vec<_>>().join(","));
            out.push_str("\r\n");
        }
    }
    let mut r = (StatusCode::OK, out).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}.csv\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

// ----- the builder ---------------------------------------------------------

/// One field row of the builder, as typed.
#[derive(Debug, Clone, Default, serde::Serialize)]
struct Row {
    name: String,
    label: String,
    kind: String,
    required: bool,
    options: String,
    max_length: String,
    help: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct Draft {
    name: String,
    title: String,
    intro: String,
    submit: String,
    success: String,
    redirect: String,
    store: bool,
    unique: String,
    receipt_to: String,
    notify_to: String,
    min_seconds: String,
    max_per_hour: String,
    rows: Vec<Row>,
}

impl Draft {
    fn from_def(d: &FormDef) -> Self {
        Draft {
            name: d.name.clone(),
            title: d.title.clone(),
            intro: d.intro.clone().unwrap_or_default(),
            submit: d.submit.clone(),
            success: d.success.clone(),
            redirect: d.redirect.clone().unwrap_or_default(),
            store: d.store,
            unique: d.unique.clone().unwrap_or_default(),
            receipt_to: d
                .receipt
                .as_ref()
                .map(|r| r.to_field.clone())
                .unwrap_or_default(),
            notify_to: d
                .notify
                .as_ref()
                .map(|n| n.to.join(", "))
                .unwrap_or_default(),
            min_seconds: d.spam.min_seconds.to_string(),
            max_per_hour: d.spam.max_per_hour.to_string(),
            rows: d
                .fields
                .iter()
                .map(|f| Row {
                    name: f.name.clone(),
                    label: f.label.clone().unwrap_or_default(),
                    kind: f.kind.as_str().into(),
                    required: f.required,
                    options: f.options.join("\n"),
                    max_length: f.max_length.map(|m| m.to_string()).unwrap_or_default(),
                    help: f.help.clone().unwrap_or_default(),
                })
                .collect(),
        }
    }

    fn blank() -> Self {
        Draft {
            submit: "Send".into(),
            success: "Thank you. We received your message.".into(),
            store: true,
            min_seconds: "3".into(),
            max_per_hour: "10".into(),
            rows: vec![Row {
                kind: "text".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn from_form(form: &FormData) -> Self {
        let g = |k: &str| form.get(k).unwrap_or("").trim().to_string();
        let n: usize = form
            .get("_rows")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            .min(100);
        Draft {
            name: g("name"),
            title: g("title"),
            intro: g("intro"),
            submit: g("submit"),
            success: g("success"),
            redirect: g("redirect"),
            store: form.get("store") == Some("true"),
            unique: g("unique"),
            receipt_to: g("receipt_to"),
            notify_to: g("notify_to"),
            min_seconds: g("min_seconds"),
            max_per_hour: g("max_per_hour"),
            rows: (0..n)
                .map(|i| Row {
                    name: g(&format!("f{i}.name")),
                    label: g(&format!("f{i}.label")),
                    kind: g(&format!("f{i}.type")),
                    required: form.get(&format!("f{i}.required")) == Some("true"),
                    options: g(&format!("f{i}.options")),
                    max_length: g(&format!("f{i}.max_length")),
                    help: g(&format!("f{i}.help")),
                })
                .collect(),
        }
    }

    /// The definition, or what is wrong with it.
    fn to_def(&self) -> Result<FormDef, String> {
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
        let num = |s: &str, what: &str| -> Result<Option<i64>, String> {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<i64>()
                    .ok()
                    .filter(|n| *n >= 0)
                    .map(Some)
                    .ok_or_else(|| format!("{what} must be a whole number"))
            }
        };
        let mut fields = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            let kind =
                Kind::parse(&r.kind).ok_or_else(|| format!("field {}: choose a type", i + 1))?;
            fields.push(Field {
                name: r.name.clone(),
                label: opt(&r.label),
                kind,
                required: r.required,
                min_length: None,
                max_length: num(&r.max_length, &format!("field {}: longest", i + 1))?
                    .map(|n| n as usize),
                min: None,
                max: None,
                options: r
                    .options
                    .split(['\n', ','])
                    .map(str::trim)
                    .filter(|o| !o.is_empty())
                    .map(str::to_string)
                    .collect(),
                help: opt(&r.help),
                placeholder: None,
                autocomplete: None,
            });
        }
        let notify: Vec<String> = self
            .notify_to
            .split([',', '\n', ' '])
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        let d = FormDef {
            name: self.name.clone(),
            title: self.title.clone(),
            intro: opt(&self.intro),
            submit: opt(&self.submit).unwrap_or_else(|| "Send".into()),
            success: opt(&self.success).unwrap_or_else(|| "Thank you.".into()),
            redirect: opt(&self.redirect),
            store: self.store,
            unique: opt(&self.unique),
            receipt: opt(&self.receipt_to).map(|to_field| Receipt {
                to_field,
                template: "form_receipt".into(),
            }),
            notify: (!notify.is_empty()).then(|| Notify {
                to: notify,
                template: "form_notification".into(),
            }),
            spam: Spam {
                min_seconds: num(&self.min_seconds, "Minimum seconds")?.unwrap_or(3) as u64,
                max_per_hour: num(&self.max_per_hour, "Per hour")?.unwrap_or(10),
                ..Default::default()
            },
            fields,
            steps: Vec::new(),
        };
        d.check().map_err(|e| e.to_string())?;
        Ok(d)
    }

    /// Apply an add/remove/move button. `true` if one was pressed.
    fn apply(&mut self, action: &str) -> bool {
        let at = |s: &str| s.parse::<usize>().ok();
        match action.split_once(':') {
            None if action == "add" => {
                self.rows.push(Row {
                    kind: "text".into(),
                    ..Default::default()
                });
                true
            }
            Some(("remove", i)) => {
                if let Some(i) = at(i).filter(|i| *i < self.rows.len()) {
                    self.rows.remove(i);
                }
                true
            }
            Some(("up", i)) => {
                if let Some(i) = at(i).filter(|i| *i > 0 && *i < self.rows.len()) {
                    self.rows.swap(i, i - 1);
                }
                true
            }
            Some(("down", i)) => {
                if let Some(i) = at(i).filter(|i| i + 1 < self.rows.len()) {
                    self.rows.swap(i, i + 1);
                }
                true
            }
            _ => false,
        }
    }
}

fn builder_page(
    a: &Admin,
    s: &Session,
    status: StatusCode,
    draft: &Draft,
    version: Option<i32>,
    error: Option<String>,
) -> Response {
    let editing = version.is_some();
    let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.as_str()).collect();
    a.page(
        status,
        "admin/form_edit.html",
        a.base(
            s,
            &if editing {
                format!("Edit form: {}", draft.title)
            } else {
                "New form".into()
            },
        ),
        context! {
            draft, version, editing, error, kinds,
            action_url => if editing { format!("/admin/forms/{}/edit/", draft.name) } else { "/admin/forms/new/".into() },
        },
    )
}

pub(crate) async fn new_form(State(a): State<Admin>, headers: HeaderMap, uri: Uri) -> Response {
    let (s, _) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    builder_page(&a, &s, StatusCode::OK, &Draft::blank(), None, None)
}

pub(crate) async fn create(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Form(form): Form<FormData>,
) -> Response {
    let (s, forms) = match guard_post(&a, &headers, &uri, &form).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let mut draft = Draft::from_form(&form);
    if let Some(act) = form.get("_action")
        && draft.apply(act)
    {
        return builder_page(&a, &s, StatusCode::OK, &draft, None, None);
    }
    let def = match draft.to_def() {
        Ok(d) => d,
        Err(e) => {
            return builder_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                &draft,
                None,
                Some(e),
            );
        }
    };
    if forms.is_file_form(&def.name) {
        return builder_page(
            &a,
            &s,
            StatusCode::UNPROCESSABLE_ENTITY,
            &draft,
            None,
            Some(format!(
                "`{}` is the name of a form file; choose another name",
                def.name
            )),
        );
    }
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    match store.create_form(&def.name, &def.to_json()).await {
        Ok(()) => see_other(&format!("/admin/forms/{}/?notice=form-created", def.name)),
        Err(SaveError::Exists(n)) => builder_page(
            &a,
            &s,
            StatusCode::UNPROCESSABLE_ENTITY,
            &draft,
            None,
            Some(format!("A form named `{n}` already exists.")),
        ),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn edit_form(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(name): Path<String>,
) -> Response {
    let (s, forms) = match guard(&a, &headers, &uri).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    match forms.get(&name).await {
        Ok(Some(f)) if f.source == Source::Admin => {
            builder_page(&a, &s, StatusCode::OK, &Draft::from_def(&f.def), Some(f.version), None)
        }
        Ok(Some(_)) => a.message(
            &s,
            StatusCode::CONFLICT,
            "Defined in a file",
            &format!("This form is defined in the site's forms/{name}.toml (or built in). Change it there."),
        ),
        Ok(None) => a.not_found(&s),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn save(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(name): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let (s, forms) = match guard_post(&a, &headers, &uri, &form).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(version) = form.get("_version").and_then(|v| v.parse::<i32>().ok()) else {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Form incomplete",
            "Reload the page and try again.",
        );
    };
    let mut draft = Draft::from_form(&form);
    draft.name = name.clone();
    if let Some(act) = form.get("_action")
        && draft.apply(act)
    {
        return builder_page(&a, &s, StatusCode::OK, &draft, Some(version), None);
    }
    let def = match draft.to_def() {
        Ok(d) => d,
        Err(e) => {
            return builder_page(
                &a,
                &s,
                StatusCode::UNPROCESSABLE_ENTITY,
                &draft,
                Some(version),
                Some(e),
            );
        }
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    match store.update_form(&name, version, &def.to_json()).await {
        Ok(_) => see_other(&format!("/admin/forms/{name}/?notice=form-saved")),
        Err(SaveError::Conflict { current, .. }) => builder_page(
            &a,
            &s,
            StatusCode::CONFLICT,
            &draft,
            Some(current),
            Some(format!(
                "Someone else saved this form meanwhile (it is now version {current}). Your changes are below; save again to replace theirs, or reload to see them."
            )),
        ),
        Err(SaveError::NotFound) => a.not_found(&s),
        Err(e) => a.error(&s, e),
    }
}

pub(crate) async fn delete(
    State(a): State<Admin>,
    headers: HeaderMap,
    uri: Uri,
    Path(name): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let (s, forms) = match guard_post(&a, &headers, &uri, &form).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(version) = form.get("_version").and_then(|v| v.parse::<i32>().ok()) else {
        return a.message(
            &s,
            StatusCode::BAD_REQUEST,
            "Form incomplete",
            "Reload the page and try again.",
        );
    };
    let Some(store) = forms.store() else {
        return a.not_found(&s);
    };
    match store.delete_form(&name, version).await {
        Ok(()) => see_other("/admin/forms/?notice=form-deleted"),
        Err(SaveError::Conflict { .. }) => a.message(
            &s,
            StatusCode::CONFLICT,
            "Changed meanwhile",
            "Someone changed this form after you opened it, so it was not deleted. Reload and try again.",
        ),
        Err(SaveError::NotFound) => a.not_found(&s),
        Err(e) => a.error(&s, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_are_quoted_and_defused() {
        assert_eq!(csv("plain"), "plain");
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv("-1"), "'-1");
    }

    #[test]
    fn builder_round_trip_and_actions() {
        let form = FormData(vec![
            ("name".into(), "feedback".into()),
            ("title".into(), "Feedback".into()),
            ("store".into(), "true".into()),
            ("receipt_to".into(), "email".into()),
            ("_rows".into(), "2".into()),
            ("f0.name".into(), "email".into()),
            ("f0.type".into(), "email".into()),
            ("f0.required".into(), "true".into()),
            ("f1.name".into(), "mood".into()),
            ("f1.type".into(), "radio".into()),
            ("f1.options".into(), "Good\nBad".into()),
        ]);
        let mut d = Draft::from_form(&form);
        let def = d.to_def().unwrap();
        assert_eq!(def.fields[1].options, ["Good", "Bad"]);
        assert_eq!(def.receipt.as_ref().unwrap().to_field, "email");
        assert_eq!(Draft::from_def(&def).rows.len(), 2);
        assert!(d.apply("down:0"));
        assert_eq!(d.rows[0].name, "mood");
        assert!(d.apply("add") && d.rows.len() == 3);
        assert!(d.apply("remove:2") && d.rows.len() == 2);
        assert!(!d.apply("save"));
        d.rows[0].kind = "nope".into();
        assert!(d.to_def().unwrap_err().contains("type"));
    }
}
