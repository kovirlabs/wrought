//! Mail kept instead of sent: `engine dev` hands every outgoing message to
//! a [`Capture`] (shown at `/_engine/dev/mail`) rather than to an SMTP
//! server. Only `engine dev` sets one ([`crate::Mailer::capture_into`]);
//! `engine serve` and `engine jobs work` always send.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// One captured message.
#[derive(Debug, Clone)]
pub struct Captured {
    pub id: u64,
    /// When it was captured (RFC 3339, UTC).
    pub at: String,
    pub template: String,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub reply_to: Option<String>,
    /// The message's headers as they would have been sent.
    pub headers: Vec<(String, String)>,
    pub text: String,
    pub html: Option<String>,
}

/// The captured messages, newest last; at most [`Capture::KEEP`]. Cheap to
/// clone.
#[derive(Clone, Default)]
pub struct Capture {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    list: Mutex<VecDeque<Captured>>,
    next: AtomicU64,
}

impl Capture {
    /// Messages kept (older ones are dropped).
    pub const KEEP: usize = 200;

    pub fn new() -> Capture {
        Capture::default()
    }

    pub(crate) fn push(&self, mut m: Captured) -> u64 {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed) + 1;
        m.id = id;
        let mut l = self.inner.list.lock().unwrap_or_else(|p| p.into_inner());
        if l.len() >= Self::KEEP {
            l.pop_front();
        }
        l.push_back(m);
        id
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Captured> {
        let l = self.inner.list.lock().unwrap_or_else(|p| p.into_inner());
        l.iter().rev().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Captured> {
        let l = self.inner.list.lock().unwrap_or_else(|p| p.into_inner());
        l.iter().find(|m| m.id == id).cloned()
    }

    pub fn clear(&self) {
        self.inner
            .list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

/// The header lines of a formatted message (up to the blank line), with
/// folded lines joined.
pub(crate) fn headers_of(raw: &[u8]) -> Vec<(String, String)> {
    let s = String::from_utf8_lossy(raw);
    let head = s.split("\r\n\r\n").next().unwrap_or("");
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_and_parses_headers() {
        let c = Capture::new();
        for i in 0..(Capture::KEEP + 3) {
            c.push(Captured {
                id: 0,
                at: String::new(),
                template: "t".into(),
                subject: format!("s{i}"),
                from: String::new(),
                to: String::new(),
                reply_to: None,
                headers: vec![],
                text: String::new(),
                html: None,
            });
        }
        let l = c.list();
        assert_eq!(l.len(), Capture::KEEP);
        assert_eq!(l[0].subject, format!("s{}", Capture::KEEP + 2));
        assert!(c.get(1).is_none());
        assert!(c.get(l[0].id).is_some());
        let h = headers_of(b"From: a@x.org\r\nSubject: Hello\r\n there\r\n\r\nbody");
        assert_eq!(h[1], ("Subject".into(), "Hello there".into()));
    }
}
