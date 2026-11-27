//! The machine's resources, for limits that scale with it: how many CPUs
//! this process may use and how much memory it may take, honouring
//! container limits (cgroup v2 `cpu.max` and `memory.max`, v1 equivalents).
//!
//! Limits are derived from these figures rather than fixed small caps, so a
//! large server uses its capacity and a small one is not overrun. Every
//! derived limit can still be overridden where it is configured.

use std::sync::OnceLock;

/// What this process may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// CPUs: the affinity mask, reduced by a cgroup CPU quota.
    pub cpus: usize,
    /// Bytes of memory: the machine's, or the container's limit if lower.
    pub memory: u64,
}

/// The figures, read once.
pub fn get() -> Resources {
    static R: OnceLock<Resources> = OnceLock::new();
    *R.get_or_init(detect)
}

fn detect() -> Resources {
    // `available_parallelism` already honours the affinity mask and a
    // cgroup quota.
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let machine = meminfo_total().unwrap_or(1 << 30);
    let memory = cgroup_memory_limit()
        .map(|l| l.min(machine))
        .unwrap_or(machine);
    Resources { cpus, memory }
}

/// Where the figures come from, for `engine doctor`.
pub fn describe() -> (String, String) {
    let cpus = format!(
        "{} (CPUs this process may run on, after any container CPU quota; {} online)",
        get().cpus,
        std::fs::read_to_string("/sys/devices/system/cpu/online")
            .ok()
            .and_then(|s| count_cpu_list(s.trim()))
            .map_or_else(|| "?".to_string(), |n| n.to_string())
    );
    let machine = meminfo_total();
    let mem = match (cgroup_memory_limit(), machine) {
        (Some(l), Some(m)) if l < m => format!(
            "{} MiB (container memory limit; the machine has {} MiB)",
            l >> 20,
            m >> 20
        ),
        (_, Some(m)) => format!("{} MiB (the machine's; no lower container limit)", m >> 20),
        (_, None) => format!(
            "{} MiB (assumed: /proc/meminfo unreadable)",
            get().memory >> 20
        ),
    };
    (cpus, mem)
}

/// Count a CPU list such as `0-3,8,10-11`.
fn count_cpu_list(s: &str) -> Option<usize> {
    let mut n = 0;
    for part in s.split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                n += b
                    .parse::<usize>()
                    .ok()?
                    .checked_sub(a.parse::<usize>().ok()?)?
                    + 1
            }
            None => {
                part.parse::<usize>().ok()?;
                n += 1;
            }
        }
    }
    Some(n)
}

fn meminfo_total() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The memory limit of this process's cgroup, if it has one.
fn cgroup_memory_limit() -> Option<u64> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in cg.lines() {
        let mut parts = line.splitn(3, ':');
        let (_, controllers, path) = (parts.next()?, parts.next()?, parts.next()?);
        let path = path.trim_start_matches('/');
        let candidates: Vec<String> = if controllers.is_empty() {
            // cgroup v2: the limit may be set on this group or a parent.
            let mut v = Vec::new();
            let mut p = std::path::PathBuf::from("/sys/fs/cgroup").join(path);
            loop {
                v.push(p.join("memory.max").to_string_lossy().to_string());
                if !p.pop() || p.as_os_str() == "/sys/fs" {
                    break;
                }
            }
            v
        } else if controllers.split(',').any(|c| c == "memory") {
            vec![
                format!("/sys/fs/cgroup/memory/{path}/memory.limit_in_bytes"),
                "/sys/fs/cgroup/memory/memory.limit_in_bytes".to_string(),
            ]
        } else {
            continue;
        };
        let limit = candidates
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .filter_map(|s| s.trim().parse::<u64>().ok())
            // v1 reports "no limit" as a huge number.
            .filter(|&l| l < (1 << 60))
            .min();
        if limit.is_some() {
            return limit;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn cpu_lists() {
        assert_eq!(super::count_cpu_list("0-3,8,10-11"), Some(7));
        assert_eq!(super::count_cpu_list("0"), Some(1));
        assert_eq!(super::count_cpu_list("x"), None);
    }

    #[test]
    fn detects_something_sane() {
        let r = super::get();
        assert!(r.cpus >= 1);
        assert!(r.memory >= 16 << 20, "{r:?}");
    }
}
