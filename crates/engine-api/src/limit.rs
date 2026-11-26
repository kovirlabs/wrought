//! A fixed-window rate limiter in memory (per process).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    per_window: u32,
    window: Duration,
    seen: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Limiter {
    pub fn per_minute(n: u32) -> Self {
        Limiter {
            per_window: n,
            window: Duration::from_secs(60),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Count a request for `key`. `Err(seconds)` until the window ends when
    /// over the limit.
    pub fn hit(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut m = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let e = m.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(e.0) >= self.window {
            *e = (now, 0);
        }
        e.1 += 1;
        if e.1 > self.per_window {
            let left = self.window.saturating_sub(now.duration_since(e.0));
            return Err(left.as_secs().max(1));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_key() {
        let l = Limiter::per_minute(2);
        assert!(l.hit("a").is_ok());
        assert!(l.hit("a").is_ok());
        let wait = l.hit("a").unwrap_err();
        assert!((1..=60).contains(&wait));
        assert!(l.hit("b").is_ok());
    }
}
