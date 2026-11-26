//! How account messages reach people.
//!
//! The engine asks an [`AccountNotifier`] to deliver a [`Message`]: a
//! template name (`password_reset`, `welcome`, `workflow_review`, ...) plus
//! its data. With the `mail` feature the notifier renders
//! `templates/mail/<template>.txt` / `.html` and queues the email
//! (`engine_mail::MailNotifier`). Without it, [`LogNotifier`] writes the
//! message to the server's log (stderr) for an operator to pass on.
//!
//! The log line of a password reset contains a live secret (the link):
//! treat the server log accordingly until mail is configured.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value as Json;

/// One message to one person.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub to: String,
    pub to_name: String,
    /// Mail template name (`templates/mail/<template>.txt`).
    pub template: String,
    /// Template data.
    pub data: Json,
}

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// Delivers account messages. Implementations should not block for long:
/// queue the message (the mail notifier enqueues a job) instead of sending
/// it while the visitor waits.
pub trait AccountNotifier: Send + Sync {
    fn send(&self, msg: Message) -> SendFuture<'_>;

    /// Whether messages really reach people (mail is configured), as
    /// opposed to the server log.
    fn delivers(&self) -> bool {
        false
    }
}

/// Writes messages to stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogNotifier;

impl AccountNotifier for LogNotifier {
    fn send(&self, msg: Message) -> SendFuture<'_> {
        Box::pin(async move {
            if msg.template == "password_reset" {
                eprintln!(
                    "auth: password reset requested for {}. No mail in this build (or no SMTP \
                     server configured); pass this link on (valid {} minutes, single use): {}",
                    msg.to,
                    msg.data["valid_minutes"],
                    msg.data["link"].as_str().unwrap_or("")
                );
            } else {
                eprintln!(
                    "notify: `{}` message for {} (no mail configured): {}",
                    msg.template, msg.to, msg.data
                );
            }
            Ok(())
        })
    }
}

/// Keeps messages in memory (tests).
#[derive(Debug, Default)]
pub struct MemoryNotifier {
    pub sent: std::sync::Mutex<Vec<Message>>,
}

impl AccountNotifier for MemoryNotifier {
    fn send(&self, msg: Message) -> SendFuture<'_> {
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(msg);
        Box::pin(async { Ok(()) })
    }
}
