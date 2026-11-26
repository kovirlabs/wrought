//! Wrought's `mail` feature: email through SMTP, written as minijinja
//! templates and delivered by the job queue (`jobs`).
//!
//! [`Mailer::send`] renders a message when it is asked for (so template
//! errors show up at once, in the request that caused them) and queues a
//! `mail.send` job holding the rendered text. A worker hands the job to the
//! SMTP server; a failure is retried with backoff and, after `max_attempts`,
//! the job is `dead` (`engine jobs list --status dead`). On success the
//! job's payload is cleared, so links in messages (password resets) do not
//! stay in the database.
//!
//! Templates are `templates/mail/<name>.txt` (required: blocks `subject` and
//! `body`) and optionally `templates/mail/<name>.html` (the HTML part). A
//! site's file wins over the built-in one with the same name. Templates get
//! the message's data plus `site_name` and `site_url`.
//!
//! `[mail]` in `site.toml`:
//!
//! ```toml
//! [mail]
//! from = "Example <noreply@example.org>"
//! reply_to = "office@example.org"        # optional
//! smtp_url = "smtp://127.0.0.1:1025"     # better: ENGINE_SMTP_URL
//! max_attempts = 8
//! timeout_seconds = 20
//! ```
//!
//! SMTP URLs: `smtp://host:25` (no TLS, local relays and test sinks only),
//! `smtp://user:password@host:587?tls=required` (STARTTLS),
//! `smtps://user:password@host:465` (TLS). `ENGINE_SMTP_URL` overrides
//! `smtp_url`, so credentials stay out of the site folder.

mod config;
#[cfg(feature = "auth")]
mod notifier;
mod templates;
pub mod testing;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use engine_jobs::{Enqueue, Handlers, JobContext, Jobs};
use lettre::message::{Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub use config::{MailConfig, SMTP_URL_ENV, redact_url};
#[cfg(feature = "auth")]
pub use notifier::MailNotifier;
pub use templates::{MailTemplates, Rendered};

/// The job kind that sends one message.
pub const SEND_JOB: &str = "mail.send";

/// What a `mail.send` job carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Outgoing {
    pub to: String,
    #[serde(default)]
    pub to_name: String,
    pub subject: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    /// Template name, for logs.
    #[serde(default)]
    pub template: String,
}

/// Sends a site's email. Cheap to clone.
#[derive(Clone)]
pub struct Mailer {
    inner: Arc<Inner>,
}

struct Inner {
    jobs: Jobs,
    cfg: MailConfig,
    templates: MailTemplates,
    site_name: String,
    public_url: String,
    /// The SMTP URL, checked at start; the transport (a connection pool,
    /// which needs the async runtime) is made on first use.
    smtp_url: Option<String>,
    transport: std::sync::OnceLock<AsyncSmtpTransport<Tokio1Executor>>,
    from: Option<Mailbox>,
    reply_to: Option<Mailbox>,
}

impl Mailer {
    /// `site_dir` holds the site's `templates/mail/` overrides.
    /// `public_url` is the site's address for links (no trailing slash).
    pub fn new(
        jobs: Jobs,
        cfg: MailConfig,
        site_dir: &Path,
        site_name: &str,
        public_url: &str,
    ) -> Result<Self> {
        if let Some(url) = &cfg.smtp_url {
            AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                .with_context(|| format!("[mail] SMTP URL {}", redact_url(url)))?;
        }
        let smtp_url = cfg.smtp_url.clone();
        let from = cfg
            .from
            .as_deref()
            .map(|f| f.parse::<Mailbox>())
            .transpose()
            .context("[mail] from is not an email address (`Name <address>` or `address`)")?;
        let reply_to = cfg
            .reply_to
            .as_deref()
            .map(|f| f.parse::<Mailbox>())
            .transpose()
            .context("[mail] reply_to is not an email address")?;
        let tdir = site_dir.join("templates");
        let templates = MailTemplates::new(tdir.is_dir().then_some(tdir));
        Ok(Mailer {
            inner: Arc::new(Inner {
                jobs,
                cfg,
                templates,
                site_name: site_name.to_string(),
                public_url: public_url.trim_end_matches('/').to_string(),
                smtp_url,
                transport: std::sync::OnceLock::new(),
                from,
                reply_to,
            }),
        })
    }

    /// Whether an SMTP server is configured (otherwise messages are queued
    /// and fail until one is).
    pub fn configured(&self) -> bool {
        self.inner.smtp_url.is_some() && self.inner.from.is_some()
    }

    pub fn config(&self) -> &MailConfig {
        &self.inner.cfg
    }

    pub fn templates(&self) -> &MailTemplates {
        &self.inner.templates
    }

    /// Render `template` with `data` and queue it for `to`. Returns the job
    /// id.
    pub async fn send(&self, to: &str, to_name: &str, template: &str, data: Json) -> Result<i64> {
        let msg = self.render(to, to_name, template, data)?;
        self.inner
            .jobs
            .enqueue_with(
                SEND_JOB,
                serde_json::to_value(&msg)?,
                Enqueue {
                    max_attempts: self.inner.cfg.max_attempts,
                    ..Default::default()
                },
            )
            .await
    }

    /// Render a message without sending it.
    pub fn render(&self, to: &str, to_name: &str, template: &str, data: Json) -> Result<Outgoing> {
        to.parse::<lettre::Address>()
            .with_context(|| format!("`{to}` is not an email address"))?;
        let mut ctx = match data {
            Json::Object(m) => m,
            Json::Null => Default::default(),
            other => anyhow::bail!("mail data must be an object, not {other}"),
        };
        ctx.entry("site_name")
            .or_insert_with(|| Json::String(self.inner.site_name.clone()));
        ctx.entry("site_url")
            .or_insert_with(|| Json::String(self.inner.public_url.clone()));
        ctx.entry("to_name")
            .or_insert_with(|| Json::String(to_name.to_string()));
        let r = self
            .inner
            .templates
            .render(template, minijinja::Value::from_serialize(&ctx))?;
        Ok(Outgoing {
            to: to.to_string(),
            to_name: to_name.to_string(),
            subject: r.subject,
            text: r.text,
            html: r.html,
            template: template.to_string(),
        })
    }

    /// Add the `mail.send` handler.
    pub fn register(&self, handlers: &mut Handlers) {
        let me = self.clone();
        handlers.register(SEND_JOB, move |ctx: JobContext| {
            let me = me.clone();
            async move { me.deliver(&ctx).await }
        });
    }

    async fn deliver(&self, ctx: &JobContext) -> Result<()> {
        let out: Outgoing =
            serde_json::from_value(ctx.payload.clone()).context("bad mail.send payload")?;
        let (Some(url), Some(from)) = (&self.inner.smtp_url, &self.inner.from) else {
            anyhow::bail!(
                "no SMTP server configured: set {SMTP_URL_ENV} (or [mail] smtp_url) and [mail] from"
            );
        };
        let timeout = self.inner.cfg.timeout_seconds;
        let transport = match self.inner.transport.get() {
            Some(t) => t,
            None => {
                let t = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                    .with_context(|| format!("SMTP URL {}", redact_url(url)))?
                    .timeout(Some(Duration::from_secs_f64(timeout)))
                    .build();
                let _ = self.inner.transport.set(t);
                self.inner.transport.get().expect("just set")
            }
        };
        let to_addr: lettre::Address = out.to.parse().context("bad recipient")?;
        let to = Mailbox::new(
            (!out.to_name.trim().is_empty()).then(|| out.to_name.clone()),
            to_addr,
        );
        let mut b = Message::builder()
            .from(from.clone())
            .to(to)
            .subject(out.subject.clone());
        if let Some(r) = &self.inner.reply_to {
            b = b.reply_to(r.clone());
        }
        let text = SinglePart::builder()
            .header(ContentType::TEXT_PLAIN)
            .body(out.text.clone());
        let msg = match &out.html {
            Some(html) => b.multipart(
                MultiPart::alternative().singlepart(text).singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.clone()),
                ),
            )?,
            None => b.singlepart(text)?,
        };
        transport
            .send(msg)
            .await
            .map_err(|e| anyhow::anyhow!("SMTP: {e}"))?;
        // The body may hold a live link; don't keep it once delivered.
        ctx.scrub_payload_on_success();
        Ok(())
    }
}
