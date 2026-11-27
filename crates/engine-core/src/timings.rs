//! Phase timings for `engine build --timings` (and `engine rebuild
//! --timings`): where a build spends its wall-clock time.
//!
//! Off by default and then free: [`phase`] checks one atomic flag. When on,
//! each phase records its name and duration once, in the order phases end,
//! and [`report`] prints them with the share of the total. Phases may nest
//! (the nested ones are indented), so "render" can sit inside "build".

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static DEPTH: AtomicUsize = AtomicUsize::new(0);
static PHASES: Mutex<Vec<(usize, String, Duration)>> = Mutex::new(Vec::new());

/// Turn recording on (for the rest of the process).
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Records the time from [`phase`] until it is dropped.
pub struct Phase {
    name: Option<&'static str>,
    depth: usize,
    started: Instant,
}

/// Start timing `name`; the phase ends when the returned guard drops.
pub fn phase(name: &'static str) -> Phase {
    if !enabled() {
        return Phase {
            name: None,
            depth: 0,
            started: Instant::now(),
        };
    }
    Phase {
        name: Some(name),
        depth: DEPTH.fetch_add(1, Ordering::Relaxed),
        started: Instant::now(),
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some(name) = self.name {
            DEPTH.fetch_sub(1, Ordering::Relaxed);
            let took = self.started.elapsed();
            PHASES.lock().unwrap_or_else(|p| p.into_inner()).push((
                self.depth,
                name.to_string(),
                took,
            ));
        }
    }
}

/// Time `f` as phase `name`.
pub fn time<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    let _p = phase(name);
    f()
}

/// The recorded phases, nested ones before the phase that holds them.
pub fn recorded() -> Vec<(usize, String, Duration)> {
    PHASES.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The phases as a table (empty if recording is off), against `total`.
pub fn report(total: Duration) -> String {
    let phases = recorded();
    if phases.is_empty() {
        return String::new();
    }
    let mut out = String::from("timings:\n");
    let top: Duration = phases
        .iter()
        .filter(|(d, _, _)| *d == 0)
        .map(|(_, _, t)| *t)
        .sum();
    // Print parents before their children: a phase is recorded when it
    // ends, so walk the list and hoist each parent above its nested run.
    let mut ordered: Vec<(usize, String, Duration)> = Vec::new();
    let mut pending: Vec<Vec<(usize, String, Duration)>> = vec![Vec::new()];
    for (depth, name, took) in phases {
        while pending.len() <= depth + 1 {
            pending.push(Vec::new());
        }
        let children = std::mem::take(&mut pending[depth + 1]);
        let mut group = vec![(depth, name, took)];
        group.extend(children);
        if depth == 0 {
            ordered.extend(group);
        } else {
            pending[depth].extend(group);
        }
    }
    for rest in pending {
        ordered.extend(rest);
    }
    for (depth, name, took) in &ordered {
        let pct = if total.is_zero() {
            0.0
        } else {
            took.as_secs_f64() * 100.0 / total.as_secs_f64()
        };
        out.push_str(&format!(
            "  {:indent$}{:<width$} {:>9.1} ms {:>5.1} %\n",
            "",
            name,
            took.as_secs_f64() * 1000.0,
            pct,
            indent = depth * 2,
            width = 28usize.saturating_sub(depth * 2),
        ));
    }
    let other = total.saturating_sub(top);
    out.push_str(&format!(
        "  {:<28} {:>9.1} ms\n  {:<28} {:>9.1} ms\n",
        "(outside the phases)",
        other.as_secs_f64() * 1000.0,
        "total",
        total.as_secs_f64() * 1000.0
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_then_nested_phases() {
        assert!(report(Duration::from_secs(1)).is_empty());
        enable();
        {
            let _outer = phase("outer");
            time("inner", || std::thread::sleep(Duration::from_millis(2)));
        }
        let r = report(Duration::from_millis(10));
        let outer = r.find("outer").unwrap();
        let inner = r.find("inner").unwrap();
        assert!(outer < inner, "parent first: {r}");
        assert!(r.contains("total"), "{r}");
    }
}
