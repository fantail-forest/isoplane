//! System and cgroup metadata captured alongside measurements.

use std::fs;

/// Environment facts worth stamping on a report, so a number read a year from
/// now can be placed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Environment {
    pub kernel: String,
    pub cpus: usize,
    pub mem_total_bytes: u64,
    pub cgroup_memory_max: Option<u64>,
    pub cgroup_cpu_max: Option<String>,
}

impl Environment {
    pub fn detect() -> Self {
        Self {
            kernel: fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            mem_total_bytes: crate::mem::machine_total_bytes().unwrap_or(0),
            cgroup_memory_max: read_cgroup("memory.max").and_then(|s| s.trim().parse().ok()),
            cgroup_cpu_max: read_cgroup("cpu.max").map(|s| s.trim().to_string()),
        }
    }

    /// The banner line every report opens with.
    pub fn banner(&self) -> String {
        let mut s = format!(
            "environment: kernel {} · {} cpus · {} RAM",
            self.kernel,
            self.cpus,
            crate::mem::human(self.mem_total_bytes as f64),
        );
        if let Some(max) = self.cgroup_memory_max {
            s.push_str(&format!(" · cgroup memory.max {}", crate::mem::human(max as f64)));
        }
        if let Some(cpu) = &self.cgroup_cpu_max {
            if cpu != "max 100000" {
                s.push_str(&format!(" · cgroup cpu.max {cpu}"));
            }
        }
        s
    }
}

/// Read a cgroup v2 file for this process's own cgroup.
///
/// `None` covers both "no limit" and "could not read"; the banner treats them
/// alike because neither is a number to print.
fn read_cgroup(file: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/fs/cgroup/{file}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_kernel_and_the_cpu_count() {
        let e = Environment::detect();
        let b = e.banner();
        assert!(b.contains(&e.kernel), "the kernel must be on the banner: {b}");
        assert!(b.contains(&format!("{} cpus", e.cpus)), "the cpu count must be on the banner: {b}");
        assert!(e.cpus > 0);
    }
}
