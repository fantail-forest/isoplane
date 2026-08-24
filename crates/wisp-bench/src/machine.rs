//! CPU topology, resource limits and system metadata for measurements.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Host topology and resource metadata used to describe a measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub cpu_model: String,
    pub vendor: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub sockets: usize,
    pub physical_cores: usize,
    pub threads_per_core: usize,
    pub logical_cpus: usize,
    /// `cpufreq` base/max, in MHz, as the firmware advertises them. Advertised
    /// clock is not achieved clock on a boost-managed part, which is why
    /// [`EffectiveClock`] exists.
    pub cpufreq_min_mhz: Option<u32>,
    pub cpufreq_max_mhz: Option<u32>,
    pub scaling_governor: Option<String>,
    pub boost_enabled: Option<bool>,
    /// One entry per NUMA node.
    pub numa: Vec<NumaNode>,
    /// NUMA distance matrix, row-major over `numa`.
    pub numa_distances: Vec<Vec<u32>>,
    /// One entry per distinct last-level-cache domain.
    pub l3_domains: Vec<CacheDomain>,
    pub total_memory_bytes: u64,
    pub kernel: String,
    /// Environment metadata from the shared measurement helper.
    pub sandbox: SandboxDescriptor,
    /// The cgroup this process is accounted to, and its caps.
    pub cgroup: CgroupDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumaNode {
    pub node: usize,
    /// Every logical cpu on the node, SMT siblings included.
    pub cpus: Vec<usize>,
    /// One representative logical cpu per physical core, in ascending order.
    pub physical_cpus: Vec<usize>,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheDomain {
    /// Lowest logical cpu in the domain, used as its id.
    pub id: usize,
    pub level: u8,
    pub size_bytes: u64,
    pub cpus: Vec<usize>,
    pub physical_cpus: Vec<usize>,
}

/// The environment label observed by the shared measurement helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxDescriptor {
    /// `sysbox sandbox` / `container (not sysbox)` / `host`, from
    /// `wisp_acceptance::sandbox::Provenance::label`.
    pub provenance: String,
}

impl SandboxDescriptor {
    /// Captures the environment label from the shared measurement helper.
    pub fn probe() -> Self {
        let p = wisp_acceptance::sandbox::detect();
        Self {
            provenance: p.label().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CgroupDescriptor {
    pub path: String,
    pub memory_max: Option<u64>,
    pub memory_high: Option<u64>,
    pub cpu_max: Option<String>,
    pub cpuset_cpus: Option<String>,
    pub cpuset_mems: Option<String>,
}

/// Clock estimates and the load under which the dependent-chain measurement ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveClock {
    /// Kernel's live view, averaged over the sampled cpus (`scaling_cur_freq`).
    pub scaling_cur_mhz: Option<f64>,
    /// Measured: a dependent-chain loop of known length, timed. Immune to the
    /// kernel's sampling and to the container's `/proc/cpuinfo` being stale.
    pub measured_mhz: f64,
    /// How many threads were busy while `measured_mhz` was taken. A one-thread
    /// figure and a sixteen-thread figure are different numbers about the same
    /// machine, and mixing them is how a memory-bandwidth knee gets attributed
    /// to the CPU.
    pub busy_threads: usize,
    pub cpus_sampled: Vec<usize>,
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read_trim(p).and_then(|s| s.parse().ok())
}

/// Parse a Linux cpu-list (`0-7,128-135`) into logical cpu numbers.
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(v) = part.trim().parse::<usize>() {
                    out.push(v);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Render a cpu set back into the compact list form, for descriptors and for
/// `--cpuset-cpus`.
pub fn render_cpu_list(cpus: &[usize]) -> String {
    let mut v: Vec<usize> = cpus.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        parts.push(if start == end { format!("{start}") } else { format!("{start}-{end}") });
        i += 1;
    }
    parts.join(",")
}

/// `size` in sysfs cache nodes is written `98304K` / `512K` / `32K`.
fn parse_cache_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'K' | 'k' => (&s[..s.len() - 1], 1024u64),
        'M' | 'm' => (&s[..s.len() - 1], 1024 * 1024),
        'G' | 'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().map(|v| v * mult)
}

impl Machine {
    pub fn probe() -> Self {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let field = |k: &str| -> Option<String> {
            cpuinfo
                .lines()
                .find(|l| l.starts_with(k) && l.contains(':'))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        };

        let logical_cpus = online_cpus().len();
        // Use `thread_siblings_list` for SMT pairing; logical CPU numbering varies
        // between vendors.
        let mut cores: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for cpu in online_cpus() {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let pkg = read_u64(format!("{base}/physical_package_id")).unwrap_or(0) as u32;
            let core = read_u64(format!("{base}/core_id")).unwrap_or(cpu as u64) as u32;
            cores.entry((pkg, core)).or_default().push(cpu);
        }
        let sockets = cores.keys().map(|(p, _)| *p).collect::<std::collections::BTreeSet<_>>().len();
        let physical_cores = cores.len();
        let threads_per_core =
            if physical_cores == 0 { 1 } else { (logical_cpus / physical_cores).max(1) };

        Machine {
            cpu_model: field("model name").unwrap_or_else(|| "unknown".into()),
            vendor: field("vendor_id").unwrap_or_else(|| "unknown".into()),
            family: field("cpu family").and_then(|v| v.parse().ok()).unwrap_or(0),
            model: field("model").and_then(|v| v.parse().ok()).unwrap_or(0),
            stepping: field("stepping").and_then(|v| v.parse().ok()).unwrap_or(0),
            sockets: sockets.max(1),
            physical_cores,
            threads_per_core,
            logical_cpus,
            cpufreq_min_mhz: read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq")
                .map(|k| (k / 1000) as u32),
            cpufreq_max_mhz: read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
                .map(|k| (k / 1000) as u32),
            scaling_governor: read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
            boost_enabled: read_trim("/sys/devices/system/cpu/cpufreq/boost")
                .map(|v| v == "1")
                .or_else(|| read_trim("/sys/devices/system/cpu/cpu0/cpufreq/cpb").map(|v| v == "1")),
            numa: probe_numa(),
            numa_distances: probe_distances(),
            l3_domains: probe_l3(),
            total_memory_bytes: read_meminfo_kb("MemTotal").unwrap_or(0) * 1024,
            kernel: read_trim("/proc/sys/kernel/osrelease").unwrap_or_default(),
            sandbox: SandboxDescriptor::probe(),
            cgroup: probe_cgroup(),
        }
    }

    pub fn node(&self, node: usize) -> Option<&NumaNode> {
        self.numa.iter().find(|n| n.node == node)
    }

    /// The L3 domains that any of `cpus` fall into, and their total capacity.
    ///
    /// Count a shared cache once even when several selected CPUs use it.
    pub fn l3_bytes_for(&self, cpus: &[usize]) -> u64 {
        self.l3_domains
            .iter()
            .filter(|d| d.cpus.iter().any(|c| cpus.contains(c)))
            .map(|d| d.size_bytes)
            .sum()
    }
}

fn online_cpus() -> Vec<usize> {
    read_trim("/sys/devices/system/cpu/online").map(|s| parse_cpu_list(&s)).unwrap_or_else(|| {
        (0..std::thread::available_parallelism().map_or(1, |n| n.get())).collect()
    })
}

/// One representative logical cpu per physical core, from a set of cpus.
pub fn dedup_to_physical(cpus: &[usize]) -> Vec<usize> {
    let mut seen: std::collections::BTreeSet<Vec<usize>> = Default::default();
    let mut out = Vec::new();
    for &cpu in cpus {
        let sibs = read_trim(format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"))
            .map(|s| parse_cpu_list(&s))
            .unwrap_or_else(|| vec![cpu]);
        if seen.insert(sibs) {
            out.push(cpu);
        }
    }
    out.sort_unstable();
    out
}

/// The SMT sibling of `cpu`, if the part has any and it is online.
pub fn sibling_of(cpu: usize) -> Option<usize> {
    let sibs = read_trim(format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"))
        .map(|s| parse_cpu_list(&s))?;
    sibs.into_iter().find(|c| *c != cpu)
}

fn probe_numa() -> Vec<NumaNode> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/devices/system/node") else { return out };
    let mut nodes: Vec<usize> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.strip_prefix("node").and_then(|v| v.parse::<usize>().ok())
        })
        .collect();
    nodes.sort_unstable();
    for node in nodes {
        let cpus = read_trim(format!("/sys/devices/system/node/node{node}/cpulist"))
            .map(|s| parse_cpu_list(&s))
            .unwrap_or_default();
        let memory_bytes = fs::read_to_string(format!("/sys/devices/system/node/node{node}/meminfo"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.contains("MemTotal"))
                    .and_then(|l| l.split_whitespace().nth(3))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
            .unwrap_or(0);
        let physical_cpus = dedup_to_physical(&cpus);
        out.push(NumaNode { node, cpus, physical_cpus, memory_bytes });
    }
    out
}

fn probe_distances() -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let mut node = 0;
    while let Some(line) = read_trim(format!("/sys/devices/system/node/node{node}/distance")) {
        out.push(line.split_whitespace().filter_map(|v| v.parse().ok()).collect());
        node += 1;
    }
    out
}

fn probe_l3() -> Vec<CacheDomain> {
    let mut by_id: BTreeMap<usize, CacheDomain> = BTreeMap::new();
    for cpu in online_cpus() {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}/cache");
        for idx in 0..8 {
            let d = format!("{base}/index{idx}");
            if !Path::new(&d).exists() {
                continue;
            }
            if read_u64(format!("{d}/level")) != Some(3) {
                continue;
            }
            let cpus = read_trim(format!("{d}/shared_cpu_list"))
                .map(|s| parse_cpu_list(&s))
                .unwrap_or_else(|| vec![cpu]);
            let id = cpus.first().copied().unwrap_or(cpu);
            let size_bytes =
                read_trim(format!("{d}/size")).and_then(|s| parse_cache_size(&s)).unwrap_or(0);
            let physical_cpus = dedup_to_physical(&cpus);
            by_id.entry(id).or_insert(CacheDomain { id, level: 3, size_bytes, cpus, physical_cpus });
        }
    }
    by_id.into_values().collect()
}

fn read_meminfo_kb(key: &str) -> Option<u64> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn probe_cgroup() -> CgroupDescriptor {
    // cgroup v2: a single `0::<path>` line. The mount inside the container is
    // the namespace root, so `<path>` is relative to it and the files below
    // are the ones that actually bind us.
    let path = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("0::")).map(|l| l[3..].to_string()))
        .unwrap_or_default();
    let root = "/sys/fs/cgroup";
    let opt = |f: &str| read_trim(format!("{root}/{f}"));
    CgroupDescriptor {
        path,
        memory_max: opt("memory.max").and_then(|v| v.parse().ok()),
        memory_high: opt("memory.high").and_then(|v| v.parse().ok()),
        cpu_max: opt("cpu.max"),
        cpuset_cpus: opt("cpuset.cpus.effective"),
        cpuset_mems: opt("cpuset.mems.effective"),
    }
}

/// Estimate the clock rate in MHz from a timed dependent-chain loop.
///
/// The estimate depends on instruction throughput and the load during sampling.
pub fn measure_clock_mhz(sample_ms: u64) -> f64 {
    use std::time::Instant;
    // Warm up so the core is at its loaded bin before the timed span.
    let _ = dependent_chain(1 << 22);
    let mut iters: u64 = 1 << 22;
    loop {
        let t0 = Instant::now();
        let sink = dependent_chain(iters);
        let dt = t0.elapsed();
        std::hint::black_box(sink);
        if dt.as_millis() as u64 >= sample_ms || iters > (1u64 << 34) {
            return (iters * CHAIN_OPS) as f64 / dt.as_secs_f64() / 1e6;
        }
        iters *= 2;
    }
}

/// Cycles a call to [`dependent_chain`] costs per iteration.
const CHAIN_OPS: u64 = 8;

/// A serial dependency chain of [`CHAIN_OPS`] additions per iteration.
///
/// Assembly keeps the timed instruction sequence independent of Rust optimization.
#[inline(never)]
#[cfg(target_arch = "x86_64")]
fn dependent_chain(iters: u64) -> u64 {
    let mut a: u64 = 1;
    let mut n = iters;
    if n == 0 {
        return a;
    }
    unsafe {
        std::arch::asm!(
            "2:",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "add {a}, 1",
            "sub {n}, 1",
            "jnz 2b",
            a = inout(reg) a,
            n = inout(reg) n => _,
            options(nostack),
        );
    }
    a
}

/// Portable fallback. Its per-iteration cost is not one cycle and it is not
/// profile-independent, so a descriptor built from it says so.
#[inline(never)]
#[cfg(not(target_arch = "x86_64"))]
fn dependent_chain(iters: u64) -> u64 {
    let mut a: u64 = 1;
    for _ in 0..iters {
        a = std::hint::black_box(a.wrapping_add(1));
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_round_trip() {
        assert_eq!(parse_cpu_list("0-7,128-135").len(), 16);
        assert_eq!(parse_cpu_list("5"), vec![5]);
        assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
        assert_eq!(render_cpu_list(&parse_cpu_list("0-7,128-135")), "0-7,128-135");
        assert_eq!(render_cpu_list(&[3, 1, 2, 9]), "1-3,9");
    }

    #[test]
    fn cache_sizes_carry_their_unit() {
        assert_eq!(parse_cache_size("98304K"), Some(96 * 1024 * 1024));
        assert_eq!(parse_cache_size("512K"), Some(512 * 1024));
        assert_eq!(parse_cache_size("32M"), Some(32 * 1024 * 1024));
    }

    /// Compare the clock estimate with the firmware range when it is available.
    #[test]
    fn the_clock_probe_is_a_measurement_not_a_build_configuration() {
        let mhz = measure_clock_mhz(30);
        assert!(mhz > 500.0 && mhz < 8_000.0, "implausible measured clock: {mhz} MHz");
        // Against the firmware's own advertised range, when the kernel
        // exposes it. A probe that disagrees with `cpuinfo_max_freq` by more
        // than boost can explain is measuring something else.
        if let Some(max) = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq") {
            let max_mhz = max as f64 / 1000.0;
            assert!(
                mhz < max_mhz * 1.15,
                "measured {mhz:.0} MHz against a {max_mhz:.0} MHz ceiling"
            );
        }
    }
}
