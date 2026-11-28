//! The per-hour rate limit while the database is unavailable. Normally the
//! limit is counted in `engine_form_events`; while the database cannot be
//! read, posts that go to the spool are counted here instead, in memory,
//! per form and client, with the form's own `max_per_hour`. At replay each
//! post is counted again in the database (`web::replay`), so the hour's
//! total stays within the limit across an outage.
//!
//! Memory is bounded: at most [`MAX_CLIENTS`] entries; when full, expired
//! entries are dropped, and if none are, a new client is refused (the
//! spool is a fallback, not a place for a flood).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most (form, client) pairs tracked at once (~100 bytes each).
pub const MAX_CLIENTS: usize = 4096;
const WINDOW: Duration = Duration::from_secs(3600);

#[derive(Debug, Default)]
pub struct Limiter {
    seen: Mutex<HashMap<(String, String), (Instant, i64)>>,
}

impl Limiter {
    /// Count a post of `form` from `client`; `false` if it is over
    /// `max_per_hour` (or no room is left to track a new client).
    pub fn allow(&self, form: &str, client: &str, max_per_hour: i64) -> bool {
        self.allow_at(form, client, max_per_hour, Instant::now())
    }

    fn allow_at(&self, form: &str, client: &str, max: i64, now: Instant) -> bool {
        let mut m = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let k = (form.to_string(), client.to_string());
        if !m.contains_key(&k) && m.len() >= MAX_CLIENTS {
            m.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
            if m.len() >= MAX_CLIENTS {
                return false;
            }
        }
        let e = m.entry(k).or_insert((now, 0));
        if now.duration_since(e.0) >= WINDOW {
            *e = (now, 0);
        }
        if e.1 >= max {
            return false;
        }
        e.1 += 1;
        true
    }

    pub fn len(&self) -> usize {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_client_limit_and_bounded_memory() {
        let l = Limiter::default();
        let t = Instant::now();
        for _ in 0..5 {
            assert!(l.allow_at("contact", "a", 5, t));
        }
        assert!(!l.allow_at("contact", "a", 5, t), "sixth refused");
        assert!(l.allow_at("contact", "b", 5, t), "others unaffected");
        assert!(l.allow_at("contact", "a", 5, t + WINDOW), "next hour");
        for i in 0..MAX_CLIENTS * 2 {
            l.allow_at("contact", &format!("c{i}"), 5, t);
        }
        assert!(l.len() <= MAX_CLIENTS);
        // Once the old entries expire, new clients fit again.
        assert!(l.allow_at("contact", "late", 5, t + WINDOW * 2));
    }
}
