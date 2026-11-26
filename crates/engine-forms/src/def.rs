//! Form definitions: `forms/<name>.toml` files in a site, or forms made in
//! the admin (stored as JSON of the same shape).
//!
//! ```toml
//! title = "Contact us"
//! submit = "Send"
//! success = "Thank you, we will answer within two working days."
//! # redirect = "/thanks/"          # instead of showing `success`
//! # store = true                   # keep submissions (default)
//! # unique = "email"               # store each value of this field once (newsletters)
//!
//! [receipt]                        # email the sender a copy (`mail` feature)
//! to_field = "email"
//! # template = "form_receipt"      # templates/mail/<template>.txt/.html
//!
//! [notify]                         # email the site's people
//! to = ["office@example.org"]
//!
//! [spam]
//! min_seconds = 3                  # time trap: faster than this is a bot
//! max_per_hour = 10                # per form and client address
//!
//! [[fields]]
//! name = "email"
//! label = "Your email address"
//! type = "email"                   # text, email, textarea, number, select,
//!                                  # radio, checkbox, date, url, tel
//! required = true
//!
//! [[steps]]                        # optional: a multi-step form
//! title = "About you"
//! fields = ["name", "email"]
//! ```

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Field kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Email,
    Textarea,
    Number,
    Select,
    Radio,
    Checkbox,
    Date,
    Url,
    Tel,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Text,
        Kind::Email,
        Kind::Textarea,
        Kind::Number,
        Kind::Select,
        Kind::Radio,
        Kind::Checkbox,
        Kind::Date,
        Kind::Url,
        Kind::Tel,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Email => "email",
            Kind::Textarea => "textarea",
            Kind::Number => "number",
            Kind::Select => "select",
            Kind::Radio => "radio",
            Kind::Checkbox => "checkbox",
            Kind::Date => "date",
            Kind::Url => "url",
            Kind::Tel => "tel",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn has_options(self) -> bool {
        matches!(self, Kind::Select | Kind::Radio)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Kind,
    #[serde(default)]
    pub required: bool,
    /// Shortest text (characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// Longest text (characters). Default 200, textarea 5000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    /// Number range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Choices of `select` and `radio`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// `autocomplete` attribute (e.g. `name`, `email`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autocomplete: Option<String>,
}

impl Field {
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| {
            let mut s = self.name.replace('_', " ");
            if let Some(f) = s.get_mut(..1) {
                f.make_ascii_uppercase();
            }
            s
        })
    }

    pub fn max_len(&self) -> usize {
        self.max_length.unwrap_or(if self.kind == Kind::Textarea {
            5000
        } else {
            200
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// The `email` field whose address gets the receipt.
    pub to_field: String,
    #[serde(default = "d_receipt")]
    pub template: String,
}

fn d_receipt() -> String {
    "form_receipt".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    pub to: Vec<String>,
    #[serde(default = "d_notify")]
    pub template: String,
}

fn d_notify() -> String {
    "form_notification".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spam {
    #[serde(default = "d_min_seconds")]
    pub min_seconds: u64,
    #[serde(default = "d_per_hour")]
    pub max_per_hour: i64,
    /// Accept forms opened up to this long ago.
    #[serde(default = "d_max_age")]
    pub max_age_hours: u64,
}

fn d_min_seconds() -> u64 {
    3
}
fn d_per_hour() -> i64 {
    10
}
fn d_max_age() -> u64 {
    24
}

impl Default for Spam {
    fn default() -> Self {
        Spam {
            min_seconds: d_min_seconds(),
            max_per_hour: d_per_hour(),
            max_age_hours: d_max_age(),
        }
    }
}

/// A form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormDef {
    /// From the file name or the admin; not in the file.
    #[serde(default, skip_serializing)]
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<String>,
    #[serde(default = "d_submit")]
    pub submit: String,
    #[serde(default = "d_success")]
    pub success: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
    #[serde(default = "yes")]
    pub store: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    #[serde(default)]
    pub spam: Spam,
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

fn d_submit() -> String {
    "Send".into()
}
fn d_success() -> String {
    "Thank you. We received your message.".into()
}
fn yes() -> bool {
    true
}

/// Where a form's definition lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    File,
    Admin,
}

/// Form names: `[a-z][a-z0-9_-]{0,39}`.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

fn valid_field_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Names the engine uses in form posts.
pub const RESERVED: &[&str] = &["website", "homepage_url"];

impl FormDef {
    pub fn parse_toml(name: &str, text: &str) -> Result<Self> {
        let mut d: FormDef =
            toml::from_str(text).map_err(|e| anyhow::anyhow!("forms/{name}.toml: {e}"))?;
        d.name = name.to_string();
        d.check()
            .map_err(|e| anyhow::anyhow!("forms/{name}.toml: {e}"))?;
        Ok(d)
    }

    pub fn from_json(name: &str, v: &serde_json::Value) -> Result<Self> {
        let mut d: FormDef =
            serde_json::from_value(v.clone()).map_err(|e| anyhow::anyhow!("form `{name}`: {e}"))?;
        d.name = name.to_string();
        d.check()
            .map_err(|e| anyhow::anyhow!("form `{name}`: {e}"))?;
        Ok(d)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("serialisable")
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The honeypot's name: an inviting field name the form does not use.
    pub fn honeypot(&self) -> &'static str {
        if self.field("website").is_some() {
            "homepage_url"
        } else {
            "website"
        }
    }

    /// Check the definition.
    pub fn check(&self) -> Result<()> {
        if !valid_name(&self.name) {
            bail!(
                "the name must be lower-case letters, digits, `-` or `_`, starting with a letter"
            );
        }
        if self.title.trim().is_empty() {
            bail!("`title` is empty");
        }
        if self.fields.is_empty() {
            bail!("a form needs at least one field");
        }
        if self.fields.len() > 100 {
            bail!("at most 100 fields");
        }
        let mut seen = BTreeSet::new();
        for f in &self.fields {
            if !valid_field_name(&f.name) || f.name.starts_with('_') {
                bail!(
                    "field `{}`: names are lower-case letters, digits and `_`, starting with a letter",
                    f.name
                );
            }
            if !seen.insert(f.name.as_str()) {
                bail!("field `{}` is listed twice", f.name);
            }
            if f.kind.has_options() && f.options.is_empty() {
                bail!("field `{}`: a {} needs `options`", f.name, f.kind.as_str());
            }
            if !f.kind.has_options() && !f.options.is_empty() {
                bail!(
                    "field `{}`: only select and radio fields have options",
                    f.name
                );
            }
            if let (Some(a), Some(b)) = (f.min_length, f.max_length)
                && a > b
            {
                bail!("field `{}`: min_length is above max_length", f.name);
            }
            if f.max_len() > 100_000 {
                bail!("field `{}`: max_length is at most 100000", f.name);
            }
        }
        if let Some(r) = &self.receipt {
            match self.field(&r.to_field) {
                Some(f) if f.kind == Kind::Email => {}
                _ => bail!(
                    "[receipt] to_field `{}` must name an `email` field",
                    r.to_field
                ),
            }
        }
        if let Some(n) = &self.notify
            && (n.to.is_empty() || n.to.iter().any(|a| !a.contains('@')))
        {
            bail!("[notify] to must list email addresses");
        }
        if let Some(u) = &self.unique
            && self.field(u).is_none()
        {
            bail!("unique: no field `{u}`");
        }
        if let Some(r) = &self.redirect
            && engine_core::web::safe_path(Some(r)).is_none()
        {
            bail!("redirect must be a path on this site, like `/thanks/`");
        }
        if self.spam.max_per_hour < 1 {
            bail!("[spam] max_per_hour must be at least 1");
        }
        if !self.steps.is_empty() {
            let mut covered = BTreeSet::new();
            for s in &self.steps {
                if s.fields.is_empty() {
                    bail!("step `{}` has no fields", s.title);
                }
                for f in &s.fields {
                    if self.field(f).is_none() {
                        bail!("step `{}`: no field `{f}`", s.title);
                    }
                    if !covered.insert(f.as_str()) {
                        bail!("field `{f}` is in two steps");
                    }
                }
            }
            if covered.len() != self.fields.len() {
                bail!("with `steps`, every field must be in exactly one step");
            }
        }
        Ok(())
    }

    /// Fields of step `i` (all fields for single-step forms).
    pub fn step_fields(&self, i: usize) -> Vec<&Field> {
        match self.steps.get(i) {
            Some(s) => s.fields.iter().filter_map(|n| self.field(n)).collect(),
            None => self.fields.iter().collect(),
        }
    }

    pub fn step_count(&self) -> usize {
        self.steps.len().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTACT: &str = r#"
title = "Contact"
[receipt]
to_field = "email"
[[fields]]
name = "email"
type = "email"
required = true
[[fields]]
name = "topic"
type = "select"
options = ["Sales", "Support"]
[[fields]]
name = "message"
type = "textarea"
"#;

    #[test]
    fn parses_and_checks() {
        let d = FormDef::parse_toml("contact", CONTACT).unwrap();
        assert_eq!(d.fields.len(), 3);
        assert_eq!(d.fields[2].max_len(), 5000);
        assert_eq!(d.field("email").unwrap().label(), "Email");
        assert_eq!(d.honeypot(), "website");
        assert!(d.store && d.spam.min_seconds == 3);
        let back = FormDef::from_json("contact", &d.to_json()).unwrap();
        assert_eq!(back, d);
        for (bad, why) in [
            ("title = 'x'\nfields = []", "at least one"),
            (
                "title = 'x'\n[[fields]]\nname = 'a'\ntype = 'select'",
                "options",
            ),
            ("title = 'x'\n[[fields]]\nname = 'Bad'", "lower-case"),
            (
                "title = 'x'\n[[fields]]\nname = 'a'\n[[fields]]\nname = 'a'",
                "twice",
            ),
            (
                "title = 'x'\n[receipt]\nto_field = 'a'\n[[fields]]\nname = 'a'",
                "email",
            ),
            (
                "title = 'x'\nredirect = 'https://evil'\n[[fields]]\nname = 'a'",
                "path",
            ),
            (
                "title = 'x'\n[[fields]]\nname = 'a'\n[[fields]]\nname = 'b'\n[[steps]]\ntitle = 's'\nfields = ['a']",
                "exactly one",
            ),
            (
                "title = 'x'\ncolour = 1\n[[fields]]\nname = 'a'",
                "unknown field",
            ),
        ] {
            let e = FormDef::parse_toml("x", bad).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert!(FormDef::parse_toml("Bad", CONTACT).is_err());
    }
}
