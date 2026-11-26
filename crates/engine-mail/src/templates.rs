//! Mail templates: `mail/<name>.txt` (blocks `subject` and `body`) and an
//! optional `mail/<name>.html`; a site's file wins over the built-in one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};
use minijinja::{AutoEscape, Environment, Value};

/// Built-in mail templates.
pub static BUILTIN: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// A rendered message.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
}

pub struct MailTemplates {
    env: Environment<'static>,
    site: Option<PathBuf>,
}

fn load(site: Option<&Path>, name: &str) -> Option<String> {
    engine_core::templates::validate_template_name(name).ok()?;
    if let Some(dir) = site {
        let p = dir.join(name);
        if p.is_file() {
            return std::fs::read_to_string(p).ok();
        }
    }
    BUILTIN
        .get_file(name)
        .and_then(|f| f.contents_utf8())
        .map(str::to_string)
}

impl MailTemplates {
    /// `site_templates`: the site's `templates/` folder.
    pub fn new(site_templates: Option<PathBuf>) -> Self {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|name| {
            if name.ends_with(".html") {
                AutoEscape::Html
            } else {
                AutoEscape::None
            }
        });
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
        env.set_keep_trailing_newline(true);
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        let dir = site_templates.clone();
        env.set_loader(move |name| Ok(load(dir.as_deref(), name)));
        MailTemplates {
            env,
            site: site_templates,
        }
    }

    /// Whether `mail/<name>.txt` exists (site or built-in).
    pub fn exists(&self, name: &str) -> bool {
        valid_name(name) && load(self.site.as_deref(), &format!("mail/{name}.txt")).is_some()
    }

    pub fn render(&self, name: &str, ctx: Value) -> Result<Rendered> {
        if !valid_name(name) {
            bail!("invalid mail template name `{name}`");
        }
        let txt = format!("mail/{name}.txt");
        let tmpl = self
            .env
            .get_template(&txt)
            .with_context(|| format!("mail template {txt}"))?;
        let mut captured = tmpl
            .render_captured(ctx.clone())
            .with_context(|| format!("rendering {txt}"))?;
        let (subject, text) = captured
            .with_state_mut(|state| (state.render_block("subject"), state.render_block("body")));
        let subject = subject.with_context(|| format!("{txt} needs a `subject` block"))?;
        let text = text.with_context(|| format!("{txt} needs a `body` block"))?;
        // One line, no control characters (header injection).
        let subject: String = subject
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        if subject.is_empty() {
            bail!("{txt}: the subject is empty");
        }
        let html_name = format!("mail/{name}.html");
        let html = if load(self.site.as_deref(), &html_name).is_some() {
            Some(
                self.env
                    .get_template(&html_name)
                    .and_then(|t| t.render(ctx))
                    .with_context(|| format!("rendering {html_name}"))?,
            )
        } else {
            None
        };
        Ok(Rendered {
            subject,
            text: text.trim().to_string() + "\n",
            html,
        })
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    #[test]
    fn builtin_reset_mail_renders_and_escapes_html() {
        let t = MailTemplates::new(None);
        let r = t
            .render(
                "password_reset",
                context! { site_name => "Site <b>", link => "https://x.example/r?token=a&b=1", valid_minutes => 60, to_name => "Ada" },
            )
            .unwrap();
        assert!(r.subject.contains("Site <b>"), "{}", r.subject);
        assert!(
            r.text.contains("https://x.example/r?token=a&b=1"),
            "{}",
            r.text
        );
        let html = r.html.unwrap();
        assert!(html.contains("Site &lt;b&gt;"), "{html}");
        assert!(html.contains("token=a&amp;b=1"), "{html}");
    }

    #[test]
    fn site_template_overrides_and_subject_is_one_line() {
        let d = tempfile_dir();
        std::fs::create_dir_all(d.join("mail")).unwrap();
        std::fs::write(
            d.join("mail/password_reset.txt"),
            "{% block subject %}Hi\r\nBcc: evil@x.org {{ to_name }}{% endblock %}\n{% block body %}custom {{ link }}{% endblock %}",
        )
        .unwrap();
        let t = MailTemplates::new(Some(d.clone()));
        let r = t
            .render("password_reset", context! { to_name => "Ada", link => "L" })
            .unwrap();
        assert_eq!(r.subject, "Hi Bcc: evil@x.org Ada");
        assert_eq!(r.text, "custom L\n");
        // The built-in HTML part still exists for this name.
        assert!(r.html.is_some());
        assert!(t.render("../x", context! {}).is_err());
        assert!(t.render("missing", context! {}).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wrought-mail-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
