use std::collections::BTreeMap;

use super::*;
use crate::memory_pressure::{MemoryPressureSource, SystemMemoryPressure};

fn files() -> BTreeMap<PathBuf, String> {
    [
        ("/proc/meminfo", "MemTotal: 100 kB\nMemAvailable: 60 kB\n"),
        ("/proc/self/cgroup", "0::/tenant/app\n"),
        (
            "/proc/self/mountinfo",
            "1 0 0:1 / /cg rw - cgroup2 cgroup rw\n",
        ),
        ("/cg/cgroup.controllers", "memory cpu\n"),
        ("/cg/tenant/memory.max", "max\n"),
        ("/cg/tenant/app/memory.max", "max\n"),
    ]
    .into_iter()
    .map(|(path, value)| (PathBuf::from(path), value.to_owned()))
    .collect()
}

fn sample_files(files: &BTreeMap<PathBuf, String>) -> Result<MemorySample, MemoryPressureError> {
    sample(&|path| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    })
}

#[test]
fn an_unlimited_hierarchy_preserves_host_available_memory() {
    let files = files();
    assert_eq!(
        sample_files(&files).unwrap(),
        MemorySample {
            available: MemoryBytes(60 * 1024),
            total: MemoryBytes(100 * 1024)
        },
        "unlimited cgroups must preserve host memory headroom"
    );
}

#[test]
fn parent_usage_can_exhaust_headroom_before_the_leaf_limit() {
    let mut files = files();
    files.insert("/cg/tenant/memory.max".into(), "50000".into());
    files.insert("/cg/tenant/memory.current".into(), "49000".into());
    files.insert("/cg/tenant/app/memory.max".into(), "20000".into());
    files.insert("/cg/tenant/app/memory.current".into(), "2000".into());
    assert_eq!(
        sample_files(&files).unwrap(),
        MemorySample {
            available: MemoryBytes(1000),
            total: MemoryBytes(20000)
        },
        "sibling memory usage under a parent limit must constrain available memory"
    );
}

#[test]
fn host_pressure_remains_visible_under_a_loose_container_limit() {
    let mut files = files();
    files.insert("/cg/tenant/app/memory.max".into(), "200000".into());
    files.insert("/cg/tenant/app/memory.current".into(), "1000".into());
    assert_eq!(
        sample_files(&files).unwrap().available,
        MemoryBytes(60 * 1024),
        "a loose container limit must not hide host pressure"
    );
}

#[test]
fn usage_at_or_above_a_limit_has_no_headroom() {
    for (limit, usage) in [(100, 100), (100, 101), (0, 0)] {
        let mut files = files();
        files.insert("/cg/tenant/app/memory.max".into(), limit.to_string());
        files.insert("/cg/tenant/app/memory.current".into(), usage.to_string());
        assert_eq!(
            sample_files(&files).unwrap().available,
            MemoryBytes(0),
            "exhausted cgroups must report zero headroom without underflow"
        );
    }
}

#[test]
fn missing_or_malformed_limit_data_never_becomes_unlimited() {
    for value in [
        None,
        Some("broken"),
        Some("-1"),
        Some("18446744073709551616"),
    ] {
        let mut files = files();
        files.remove(Path::new("/cg/tenant/app/memory.max"));
        if let Some(value) = value {
            files.insert("/cg/tenant/app/memory.max".into(), value.into());
        }
        assert!(
            matches!(
                sample_files(&files),
                Err(MemoryPressureError::Read { .. } | MemoryPressureError::Invalid { .. })
            ),
            "unreadable limits must not invent available memory"
        );
    }
}

#[test]
fn a_finite_limit_requires_a_readable_current_usage() {
    let mut files = files();
    files.insert("/cg/tenant/app/memory.max".into(), "20000".into());
    assert!(
        matches!(sample_files(&files), Err(MemoryPressureError::Read { .. })),
        "missing current usage must make finite headroom unknown"
    );
    files.insert("/cg/tenant/app/memory.current".into(), "broken".into());
    assert!(
        matches!(
            sample_files(&files),
            Err(MemoryPressureError::Invalid { .. })
        ),
        "malformed current usage must make finite headroom unknown"
    );
}

#[test]
fn malformed_host_memory_is_rejected() {
    for input in [
        "MemTotal: 1 kB",
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 1 kB\nMemAvailable: 2 kB",
        "MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB",
        "MemTotal: 2 MB\nMemAvailable: 1 kB",
        "MemTotal: 2 kB\nMemTotal: 2 kB\nMemAvailable: 1 kB",
    ] {
        assert!(
            matches!(
                parse_meminfo(input),
                Err(MemoryPressureError::Invalid { .. })
            ),
            "invalid host memory data must not invent a sample"
        );
    }
}

#[test]
fn cgroup_v1_memory_accounting_is_explicitly_unsupported() {
    assert!(
        matches!(
            membership("2:cpu,memory:/tenant\n"),
            Err(MemoryPressureError::Unsupported(_))
        ),
        "a legacy finite memory limit must not be silently ignored"
    );
}

#[test]
fn unrelated_legacy_controllers_do_not_hide_unified_memory() {
    assert_eq!(
        membership("2:cpu:/tenant\n0::/tenant\n").unwrap(),
        Some(PathBuf::from("/tenant")),
        "hybrid mounts must retain unified membership"
    );
}

#[test]
fn malformed_membership_is_rejected() {
    for input in ["0", "0::relative", "0::/../other", "0::/one\n0::/two"] {
        assert!(
            matches!(membership(input), Err(MemoryPressureError::Invalid { .. })),
            "invalid membership must not escape its cgroup mount"
        );
    }
}

#[test]
fn mount_roots_and_escaped_paths_resolve_the_process_directory() {
    assert_eq!(
        locate_cgroup(
            Path::new("/tenant/app"),
            "1 0 0:1 /tenant /cgroup\\040mount rw - cgroup2 cgroup rw"
        )
        .unwrap(),
        (
            PathBuf::from("/cgroup mount"),
            PathBuf::from("/cgroup mount/app")
        ),
        "a subtree mount must resolve membership relative to its root"
    );
}

#[test]
fn a_namespaced_root_limit_is_included_in_the_sample() {
    let mut files = files();
    files.insert("/proc/self/cgroup".into(), "0::/".into());
    files.insert("/cg/memory.max".into(), "1000".into());
    files.insert("/cg/memory.current".into(), "900".into());
    assert_eq!(
        sample_files(&files).unwrap().available,
        MemoryBytes(100),
        "a container namespace root is still subject to its own limit"
    );
}

#[test]
fn the_widest_mount_preserves_visible_ancestor_limits() {
    assert_eq!(locate_cgroup(Path::new("/tenant/app"), "1 0 0:1 /tenant /short rw - cgroup2 cgroup rw\n2 0 0:1 / /long/mount rw - cgroup2 cgroup rw").unwrap(), (PathBuf::from("/long/mount"), PathBuf::from("/long/mount/tenant/app")), "mount selection must prefer ancestor visibility over a short mount path");
}

#[test]
fn malformed_or_inaccessible_mounts_are_rejected() {
    for mounts in [
        "missing separator",
        "1 0 0:1 /wrong /cg rw - cgroup2 cgroup rw",
        "1 0 - cgroup2 cgroup rw",
        "1 0 0:1 / /bad\\999 rw - cgroup2 cgroup rw",
    ] {
        assert!(
            matches!(
                locate_cgroup(Path::new("/tenant"), mounts),
                Err(MemoryPressureError::Invalid { .. })
            ),
            "a malformed or inaccessible hierarchy must not become unlimited"
        );
    }
}

#[test]
fn the_native_sampler_reads_memory_through_the_production_interface() {
    let source: &dyn MemoryPressureSource = &SystemMemoryPressure;
    let sample = source
        .sample()
        .expect("native memory pressure interfaces must be readable");
    assert!(
        sample.available <= sample.total,
        "available memory must fit effective capacity"
    );
}
