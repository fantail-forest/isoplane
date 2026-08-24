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
    if !in_container() {
        return Provenance::Host;
    }
    if sysboxfs_over_proc() {
        return Provenance::Sandbox;
    }
    Provenance::Container
}

/// Is `sysboxfs` mounted over part of `/proc` or `/sys`?
///
/// `mountinfo` fields: `id parent major:minor root mountpoint options... - fstype source super-options`.
/// The mount point is field 5 (1-indexed); the filesystem type is the first
/// field after the `-` separator. Both are parsed rather than substring-matched
/// so that a *path* containing "sysboxfs" cannot masquerade as a *filesystem*
/// of that type.
fn sysboxfs_over_proc() -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|mi| mountinfo_has_sysboxfs_over_proc(&mi))
        .unwrap_or(false)
}

fn mountinfo_has_sysboxfs_over_proc(mountinfo: &str) -> bool {
    mountinfo.lines().any(|line| {
        let Some((before, after)) = line.split_once(" - ") else { return false };
        let fstype = after.split_whitespace().next().unwrap_or("");
        if fstype != "sysboxfs" {
            return false;
        }
        let mountpoint = before.split_whitespace().nth(4).unwrap_or("");
        mountpoint.starts_with("/proc") || mountpoint.starts_with("/sys")
    })
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
    fn a_host_serving_sysbox_containers_is_not_the_sandbox() {
        const HOST: &str = "3262 31 0:123 / /var/lib/sysboxfs/c8f30b8e5736 rw,nosuid,nodev,relatime shared:1624 - fuse sysboxfs rw,user_id=0,group_id=0,default_permissions,allow_other";
        const INSIDE: &str = "412 388 0:98 / /proc/sys rw,relatime - sysboxfs sysboxfs rw,user_id=0,group_id=0";

        assert!(
            !mountinfo_has_sysboxfs_over_proc(HOST),
            "the host's own sysboxfs mounts are not evidence of being inside one"
        );
        assert!(mountinfo_has_sysboxfs_over_proc(INSIDE));

        // Parse filesystem names independently of mount paths.
        const DECOY: &str = "1 2 0:1 / /home/me/sysboxfs-notes rw - ext4 /dev/sda1 rw";
        assert!(!mountinfo_has_sysboxfs_over_proc(DECOY));

        // Container-specific classification also requires a container signal.
        assert!(!(detect() == Provenance::Sandbox) || in_container());
    }


    #[test]
    fn the_environment_banner_names_the_provenance() {
        let e = Environment::detect();
        assert!(e.banner().contains(e.provenance.label()));
        assert!(e.cpus > 0);
    }
}
