//! How account messages reach people.
//!
//! v0.3 has no `mail` feature, so the default [`LogNotifier`] prints the
//! password-reset link to the server's log (stderr) for an operator to pass
//! on. The `mail` feature (roadmap v0.4) implements [`AccountNotifier`] by
//! sending email; nothing else changes.
//!
//! The log line contains a live secret (the reset link): treat the server
//! log accordingly until mail is configured.

/// Delivers account messages. Implementations must not block for long
/// (queue the message instead).
pub trait AccountNotifier: Send + Sync {
    /// Send `link`, which resets the password of `email`'s account and
    /// expires in `valid_minutes`.
    fn password_reset(
        &self,
        email: &str,
        name: &str,
        link: &str,
        valid_minutes: u32,
    ) -> anyhow::Result<()>;
}

/// Writes messages to stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogNotifier;

impl AccountNotifier for LogNotifier {
    fn password_reset(
        &self,
        email: &str,
        _name: &str,
        link: &str,
        valid_minutes: u32,
    ) -> anyhow::Result<()> {
        eprintln!(
            "auth: password reset requested for {email}. No mail feature in this build; pass \
             this link on (valid {valid_minutes} minutes, single use): {link}"
        );
        Ok(())
    }
}

/// Keeps messages in memory (tests).
#[derive(Debug, Default)]
pub struct MemoryNotifier {
    pub sent: std::sync::Mutex<Vec<(String, String)>>,
}

impl AccountNotifier for MemoryNotifier {
    fn password_reset(&self, email: &str, _: &str, link: &str, _: u32) -> anyhow::Result<()> {
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((email.to_string(), link.to_string()));
        Ok(())
    }
}
