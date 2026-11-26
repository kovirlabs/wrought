//! `[mail]` in `site.toml`.

use anyhow::{Result, bail};
use serde::Deserialize;

/// Environment variable that overrides `[mail] smtp_url`.
pub const SMTP_URL_ENV: &str = "ENGINE_SMTP_URL";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    /// Sender, `Name <address>` or `address`.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    /// SMTP server; `ENGINE_SMTP_URL` wins.
    #[serde(default)]
    pub smtp_url: Option<String>,
    /// Delivery attempts before a message's job is dead.
    #[serde(default = "d_attempts")]
    pub max_attempts: i32,
    #[serde(default = "d_timeout")]
    pub timeout_seconds: f64,
}

fn d_attempts() -> i32 {
    8
}
fn d_timeout() -> f64 {
    20.0
}

impl std::fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailConfig")
            .field("from", &self.from)
            .field("reply_to", &self.reply_to)
            .field("smtp_url", &self.smtp_url.as_deref().map(redact_url))
            .field("max_attempts", &self.max_attempts)
            .finish()
    }
}

impl Default for MailConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl MailConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        Self::from_parts(section, std::env::var(SMTP_URL_ENV).ok())
    }

    fn from_parts(section: Option<&toml::Table>, env_url: Option<String>) -> Result<Self> {
        let mut cfg: MailConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[mail]: {e}"))?,
            None => MailConfig::default(),
        };
        if let Some(u) = env_url.filter(|u| !u.trim().is_empty()) {
            cfg.smtp_url = Some(u);
        }
        if let Some(u) = &cfg.smtp_url
            && !(u.starts_with("smtp://") || u.starts_with("smtps://"))
        {
            bail!(
                "SMTP URL {} must start with smtp:// or smtps://",
                redact_url(u)
            );
        }
        if !(1..=100).contains(&cfg.max_attempts) {
            bail!("[mail] max_attempts must be between 1 and 100");
        }
        if !(cfg.timeout_seconds.is_finite() && cfg.timeout_seconds > 0.0) {
            bail!("[mail] timeout_seconds must be positive");
        }
        if cfg.smtp_url.is_some() && cfg.from.is_none() {
            bail!("[mail] from is required when an SMTP server is configured");
        }
        Ok(cfg)
    }
}

/// The URL with any password replaced by `***`.
pub fn redact_url(url: &str) -> String {
    engine_db::config::redact(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_and_passwords_hide() {
        let t: toml::Table =
            toml::from_str("from = 'a@example.org'\nsmtp_url = 'smtp://h:25'").unwrap();
        let c = MailConfig::from_parts(Some(&t), Some("smtps://u:secret@mx:465".into())).unwrap();
        assert_eq!(c.smtp_url.as_deref(), Some("smtps://u:secret@mx:465"));
        assert!(!format!("{c:?}").contains("secret"));
        let t: toml::Table = toml::from_str("smtp_url = 'smtp://h:25'").unwrap();
        assert!(
            MailConfig::from_parts(Some(&t), None).is_err(),
            "from needed"
        );
        let t: toml::Table = toml::from_str("from = 'a@x.org'\nsmtp_url = 'http://h'").unwrap();
        assert!(MailConfig::from_parts(Some(&t), None).is_err());
        assert!(
            MailConfig::from_parts(None, None)
                .unwrap()
                .smtp_url
                .is_none()
        );
    }
}
