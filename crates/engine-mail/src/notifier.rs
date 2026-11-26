//! Account messages (password reset, welcome, workflow notices) as email.

use engine_auth::notify::{AccountNotifier, Message, SendFuture};

use crate::Mailer;

/// Sends account messages through a [`Mailer`] (queued as jobs).
#[derive(Clone)]
pub struct MailNotifier {
    pub mailer: Mailer,
}

impl AccountNotifier for MailNotifier {
    fn send(&self, msg: Message) -> SendFuture<'_> {
        Box::pin(async move {
            self.mailer
                .send(&msg.to, &msg.to_name, &msg.template, msg.data)
                .await
                .map(|_| ())
        })
    }

    fn delivers(&self) -> bool {
        self.mailer.configured()
    }
}
