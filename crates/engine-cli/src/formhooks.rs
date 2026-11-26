//! What form posts need from the rest of the engine (feature `forms`): the
//! signed-in visitor, re-rendering the form's page with errors, and (with
//! `mail`) the receipt and notification emails.

use axum::http::HeaderMap;
use engine_forms::web::{BoxFuture, Hooks, Visitor};
use engine_forms::{FormDef, Submission};
use minijinja::Value;

use crate::request::RequestMode;

pub struct FormHooks {
    pub rm: RequestMode,
    #[cfg(feature = "mail")]
    pub mailer: Option<engine_mail::Mailer>,
    pub public_url: String,
}

/// `[{label, value}]` of a submission, in the form's field order.
fn fields(def: &FormDef, sub: &Submission) -> Vec<serde_json::Value> {
    def.fields
        .iter()
        .filter_map(|f| {
            let v = sub.data.get(&f.name)?;
            let text = match v {
                serde_json::Value::Bool(true) => "Yes".to_string(),
                serde_json::Value::Bool(false) => "No".to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some(serde_json::json!({ "label": f.label(), "value": text }))
        })
        .collect()
}

impl Hooks for FormHooks {
    fn visitor<'a>(&'a self, headers: &'a HeaderMap) -> BoxFuture<'a, Option<Visitor>> {
        Box::pin(async move {
            #[cfg(feature = "auth")]
            {
                self.rm.session(headers).await.map(|s| Visitor {
                    user_id: s.user.id,
                    csrf: s.csrf.clone(),
                    email: s.user.email.clone(),
                    name: s.user.display_name().to_string(),
                })
            }
            #[cfg(not(feature = "auth"))]
            {
                let _ = headers;
                None
            }
        })
    }

    fn page<'a>(
        &'a self,
        path: &'a str,
        headers: &'a HeaderMap,
        form_state: Value,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let extra = minijinja::context! { form_state => form_state };
            match self.rm.render_path(path, headers, extra).await? {
                Ok(r) => Some(r.html),
                Err(e) => {
                    eprintln!("forms: showing {path} again: {e:#}");
                    None
                }
            }
        })
    }

    #[allow(unused_variables)]
    fn submitted<'a>(
        &'a self,
        def: &'a FormDef,
        sub: &'a Submission,
        visitor: Option<&'a Visitor>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Fragments listing this account's submissions are stale now.
            if let (Some(uid), Some(c)) = (sub.user_id, (self.rm.current)().fragments().cloned()) {
                c.evict(&[format!("i:submissions:{uid}")]);
            }
            #[cfg(feature = "mail")]
            if let Some(m) = &self.mailer {
                let data = serde_json::json!({
                    "form_title": def.title,
                    "submission_id": sub.id,
                    "received_at": sub.created_at,
                    "fields": fields(def, sub),
                    "admin_url": format!("{}/admin/forms/{}/submissions/{}/",
                        self.public_url.trim_end_matches('/'), def.name, sub.id),
                });
                if let Some(r) = &def.receipt
                    && let Some(to) = sub.data.get(&r.to_field).and_then(|v| v.as_str())
                {
                    let name = sub
                        .data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or(visitor.map(|v| v.name.as_str()))
                        .unwrap_or("");
                    m.send(to, name, &r.template, data.clone()).await?;
                }
                if let Some(n) = &def.notify {
                    for to in &n.to {
                        m.send(to, "", &n.template, data.clone()).await?;
                    }
                }
            }
            let _ = (&self.public_url, fields(def, sub));
            Ok(())
        })
    }
}
