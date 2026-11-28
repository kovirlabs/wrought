//! The allocator of a long-running server: arenas sized from the CPUs this
//! process may use, and freed memory handed back when the server goes idle.
//!
//! glibc gives each thread that allocates concurrently an arena of its own,
//! up to eight per *online* CPU: in a one-CPU container on a 24-CPU host,
//! up to 192. A load test spreads a burst of short-lived allocations over
//! dozens of threads and arenas, and when it is over the freed memory stays
//! with them, fragmented: the benchmark's engine went from 4 MiB of heap at
//! start to 30-50 MiB after its load tests and stayed there. Two remedies,
//! neither costing anything while requests are being served:
//!
//! * [`tune`] caps the arenas at eight per CPU the process may use (glibc's
//!   own rule, applied to the right count), before any thread starts;
//! * [`trim_when_idle`] returns free memory to the system (`malloc_trim`)
//!   once requests stop: checked every [`IDLE_CHECK`], it trims after a
//!   quiet interval that followed a busy one, so a steady load never pays
//!   for it and a server that stays quiet is trimmed once.
//!
//! Other platforms and allocators: both are no-ops.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// How often the server checks whether it has gone quiet (a quiet interval
/// of this length after a busy one triggers a trim).
pub const IDLE_CHECK: Duration = Duration::from_secs(10);

/// Count a request (for idle detection).
pub fn note_request() {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
}

/// Requests counted so far (for idle detection elsewhere: the idle exit).
pub fn requests() -> u64 {
    REQUESTS.load(Ordering::Relaxed)
}

/// Cap the allocator's arenas at `arenas`. Call before starting threads.
pub fn tune(arenas: usize) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only sets an allocator parameter.
    unsafe {
        let _ = libc::mallopt(libc::M_ARENA_MAX, arenas.clamp(1, 1024) as libc::c_int);
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = arenas;
}

/// Hand free memory back to the system now. Returns whether any was.
pub fn release() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim walks the allocator's own free lists.
    unsafe {
        libc::malloc_trim(0) == 1
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    false
}

/// Whether to trim now: requests came in the interval before this one and
/// none since.
fn quiet_after_busy(before: u64, last: u64, now: u64) -> bool {
    now == last && last != before
}

/// Every `every`, release free memory if the server has gone quiet after
/// serving (runs until the task is dropped).
pub async fn trim_when_idle(every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.tick().await;
    let mut before = REQUESTS.load(Ordering::Relaxed);
    let mut last = before;
    loop {
        tick.tick().await;
        let now = REQUESTS.load(Ordering::Relaxed);
        if quiet_after_busy(before, last, now) {
            // Off the async threads: it can take a few milliseconds.
            let _ = tokio::task::spawn_blocking(release).await;
            // Trimmed: wait for the next busy spell.
            before = now;
        } else {
            before = last;
        }
        last = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_once_after_a_busy_spell() {
        // (before, last, now): busy then quiet → trim; quiet then quiet →
        // not again; busy → no.
        assert!(quiet_after_busy(0, 5, 5));
        assert!(!quiet_after_busy(5, 5, 5));
        assert!(!quiet_after_busy(5, 5, 9));
        assert!(!quiet_after_busy(0, 0, 0));
    }

    #[test]
    fn tune_and_release_are_safe_to_call() {
        tune(8);
        let v: Vec<Vec<u8>> = (0..1000).map(|i| vec![0u8; 1000 + i]).collect();
        drop(v);
        release();
    }
}
