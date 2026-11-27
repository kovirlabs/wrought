//! Heavy background work (image variants, compressing a bulk publish) runs
//! on threads of its own at a lower scheduling priority, at most
//! [`crate::limits::Limits::background_threads`] at once. On an idle
//! machine it still gets every CPU it may use; when requests come in, the
//! kernel runs the serving threads first, so a bulk job cannot starve
//! serving. Nothing is slowed by a fixed delay or a smaller pool.

use std::sync::{Arc, OnceLock};

/// The nice value of background threads (0 is normal, 19 the lowest). At
/// 10 a busy serving thread gets about nine times its CPU share.
pub const NICE: i32 = 10;

/// Lower the calling thread's scheduling priority to [`NICE`]. Only for
/// threads made for background work: an unprivileged process cannot raise
/// it again. Linux keeps a nice value per thread; elsewhere a no-op.
pub fn lower_this_thread() {
    #[cfg(target_os = "linux")]
    // SAFETY: setpriority with `who = 0` changes only the calling thread
    // (a per-thread attribute on Linux); it touches no memory.
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, NICE);
    }
}

/// The calling thread's nice value (for tests and `engine doctor`).
pub fn this_thread_nice() -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: getpriority only reads the calling thread's attribute.
    unsafe {
        *libc::__errno_location() = 0;
        libc::getpriority(libc::PRIO_PROCESS, 0)
    }
    #[cfg(not(target_os = "linux"))]
    0
}

fn slots() -> Arc<tokio::sync::Semaphore> {
    static S: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    S.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            crate::limits::current().background_threads,
        ))
    })
    .clone()
}

/// Run `f` on a background thread (lower priority, within the limit of
/// background work at once) and wait for it without blocking the async
/// runtime.
pub async fn run<T, F>(f: F) -> std::io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = slots()
        .acquire_owned()
        .await
        .map_err(|_| std::io::Error::other("background work is shutting down"))?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("engine-background".into())
        .spawn(move || {
            lower_this_thread();
            let _ = tx.send(f());
            drop(permit);
        })?;
    rx.await
        .map_err(|_| std::io::Error::other("background work failed (the thread panicked)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_threads_run_at_lower_priority() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let outside = this_thread_nice();
        let inside = rt.block_on(run(this_thread_nice)).unwrap();
        #[cfg(target_os = "linux")]
        assert!(inside >= NICE, "background nice {inside}, caller {outside}");
        // The caller's own priority is unchanged.
        assert_eq!(this_thread_nice(), outside);
        assert_eq!(rt.block_on(run(|| 2 + 2)).unwrap(), 4);
    }
}
