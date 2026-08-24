//! Execution-environment hints and system metadata for measurements.

use std::fs;
use std::path::Path;

/// Environment hints inferred from available process and mount metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Provenance {
    /// Signals consistent with a sysbox container.
    Sandbox,
    /// Container signals without a recognized sysbox filesystem view.
    Container,
    /// No recognized container signals.
    Host,
}

impl Provenance {

    /// Returns a short label for the observed environment.
    pub fn label(self) -> &'static str {
        match self {
            Provenance::Sandbox => "sysbox sandbox",
            Provenance::Container => "container (not sysbox)",
            Provenance::Host => "host",
        }
    }
}

/// Classifies the environment using available process and mount signals.
pub fn detect() -> Provenance {
    if sysbox_signals() > 0 {
        return Provenance::Sandbox;
    }
    if in_container() {
        return Provenance::Container;
    }
    Provenance::Host
}

fn sysbox_signals() -> usize {
    let mut n = 0;
    if let Ok(mi) = fs::read_to_string("/proc/self/mountinfo") {
        if mi.contains("sysboxfs") {
            n += 1;
        }
    }
    // A non-identity user namespace mapping is another environment hint.
    if let Ok(uid_map) = fs::read_to_string("/proc/self/uid_map") {
        let f: Vec<&str> = uid_map.split_whitespace().collect();
        if f.len() >= 3 && f[1] != "0" {
            n += 1;
        }
    }
    n
}

fn in_container() -> bool {
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    if let Ok(cg) = fs::read_to_string("/proc/1/cgroup") {
        if cg.contains("docker") || cg.contains("containerd") || cg.contains("kubepods") {
            return true;
        }
    }
    false
}

/// System metadata captured alongside a measurement.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Environment {
    pub provenance: Provenance,
    pub kernel: String,
    pub cpus: usize,
    pub mem_total_bytes: u64,
    pub cgroup_memory_max: Option<u64>,
    pub cgroup_cpu_max: Option<String>,
}

impl Environment {
    pub fn detect() -> Self {
        Self {
            provenance: detect(),
            kernel: fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            mem_total_bytes: crate::mem::machine_total_bytes().unwrap_or(0),
            cgroup_memory_max: read_cgroup("memory.max").and_then(|s| s.trim().parse().ok()),
            cgroup_cpu_max: read_cgroup("cpu.max").map(|s| s.trim().to_string()),
        }
    }

    /// Formats the collected environment metadata.
    pub fn banner(&self) -> String {
        let mut s = format!(
            "environment: {} · kernel {} · {} cpus · {} RAM",
            self.provenance.label(),
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

fn read_cgroup(file: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/fs/cgroup/{file}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_terminates_and_reports_something() {
        let p = detect();
        assert!(!p.label().is_empty());
    }

    #[test]
    fn the_environment_banner_names_the_provenance() {
        let e = Environment::detect();
        assert!(e.banner().contains(e.provenance.label()));
        assert!(e.cpus > 0);
    }
}
