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
    /// Host memory reported by `/proc/meminfo`; this is not the process cgroup limit.
    #[serde(alias = "total_memory_bytes")]
    pub host_total_memory_bytes: u64,
    pub kernel: String,
    /// Environment metadata from the shared measurement helper.
    pub sandbox: SandboxDescriptor,
    /// The cgroup this process is accounted to, and its caps.
    pub cgroup: CgroupDescriptor,
    /// The evidence behind `physical_cores`, and every sysfs read that did not
    /// answer. See [`Topology`].
    #[serde(default)]
    pub topology: Topology,
}

/// The evidence behind [`Machine::physical_cores`], including failed reads.
///
/// Compare distinct package/core pairs with distinct sibling groups. Preserve
/// read errors so callers can refuse incomplete topology information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Topology {
    /// Distinct `(physical_package_id, core_id)` pairs over the online cpus.
    pub physical_cores_from_core_ids: Option<usize>,
    /// Distinct `thread_siblings_list` groups over the online cpus.
    pub physical_cores_from_thread_siblings: Option<usize>,
    pub sockets_from_package_ids: Option<usize>,
    /// Unreadable topology fields, unparseable cache sizes and disagreements
    /// between the two core-count derivations.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumaNode {
    pub node: usize,
    /// Every logical cpu on the node, SMT siblings included.
    pub cpus: Vec<usize>,
    /// One representative logical cpu per physical core, in ascending order.
    ///
    /// Empty when sibling topology could not be read; see [`Topology::errors`].
    pub physical_cpus: Vec<usize>,
    /// Host-scoped NUMA node memory; this is not the process cgroup limit.
    #[serde(alias = "memory_bytes")]
    pub host_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheDomain {
    /// Lowest logical cpu in the domain, used as its id.
    pub id: usize,
    pub level: u8,
    /// `0` when [`CacheDomain::size_known`] is false. Never treat a zero here
    /// as a measurement.
    pub size_bytes: u64,
    /// Whether `size_bytes` was read successfully from sysfs.
    #[serde(default = "yes")]
    pub size_known: bool,
    pub cpus: Vec<usize>,
    pub physical_cpus: Vec<usize>,
}

/// Default for a missing serialized [`CacheDomain::size_known`] field.
fn yes() -> bool {
    true
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
    /// `None` means the limit **could not be read**, which is a different
    /// statement from "there is no limit" — that is [`MemoryLimit::Max`].
    pub memory_max: Option<MemoryLimit>,
    pub memory_high: Option<MemoryLimit>,
    pub cpu_max: Option<String>,
    pub cpuset_cpus: Option<String>,
    pub cpuset_mems: Option<String>,
    /// Why a `None` above is `None`, and how an absent file was interpreted.
    #[serde(default)]
    pub errors: Vec<String>,
}

/// A cgroup v2 memory limit. The containing `Option` distinguishes an
/// unreadable limit from a known byte count or an unlimited controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimit {
    /// No memory limit reported by this cgroup controller.
    Max,
    Bytes(u64),
}

impl MemoryLimit {
    /// The limit in bytes, or `None` when there is no limit.
    ///
    /// Deliberately a named method rather than a `From`/`Into`: a caller that
    /// wants a plain number has to write down what it means for there not to
    /// be one.
    pub fn bytes(self) -> Option<u64> {
        match self {
            MemoryLimit::Max => None,
            MemoryLimit::Bytes(b) => Some(b),
        }
    }
}

impl std::fmt::Display for MemoryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryLimit::Max => write!(f, "max (no limit)"),
            MemoryLimit::Bytes(b) => write!(f, "{:.1} GiB", *b as f64 / (1u64 << 30) as f64),
        }
    }
}

impl Serialize for MemoryLimit {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MemoryLimit::Max => s.serialize_str("max"),
            MemoryLimit::Bytes(b) => s.serialize_u64(*b),
        }
    }
}

impl<'de> Deserialize<'de> for MemoryLimit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(MemoryLimit::Bytes(n)),
            Raw::S(s) if s.trim() == "max" => Ok(MemoryLimit::Max),
            Raw::S(s) => s
                .trim()
                .parse()
                .map(MemoryLimit::Bytes)
                .map_err(serde::de::Error::custom),
        }
    }
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

impl EffectiveClock {
    /// Estimate the clock rate while `busy_threads` selected CPUs are busy.
    ///
    /// The measuring thread counts toward the total. Restore the caller's
    /// affinity after sampling.
    pub fn measure(cpus: &[usize], busy_threads: usize, sample_ms: u64) -> EffectiveClock {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let busy = busy_threads.max(1);
        let restore = crate::affinity::current_affinity().ok();
        let stop = Arc::new(AtomicBool::new(false));
        let mut helpers = Vec::new();
        for i in 1..busy {
            let cpu = if cpus.is_empty() { None } else { Some(cpus[i % cpus.len()]) };
            let stop = stop.clone();
            helpers.push(std::thread::spawn(move || {
                if let Some(c) = cpu {
                    let _ = crate::affinity::pin_current(&[c]);
                }
                while !stop.load(Ordering::Relaxed) {
                    std::hint::black_box(dependent_chain(1 << 20));
                }
            }));
        }
        if let Some(c) = cpus.first() {
            let _ = crate::affinity::pin_current(&[*c]);
        }
        let measured_mhz = measure_clock_mhz(sample_ms);
        stop.store(true, Ordering::Relaxed);
        for h in helpers {
            let _ = h.join();
        }
        if let Some(r) = restore {
            let _ = crate::affinity::pin_current(&r);
        }
        EffectiveClock {
            scaling_cur_mhz: scaling_cur_mhz(cpus),
            measured_mhz,
            busy_threads: busy,
            cpus_sampled: cpus.to_vec(),
        }
    }
}

/// The kernel's live `scaling_cur_freq` view, averaged over `cpus`, in MHz.
///
/// `None` when no cpu answered — inside a container `cpufreq` is frequently
/// absent, and an average over nothing is not zero.
pub fn scaling_cur_mhz(cpus: &[usize]) -> Option<f64> {
    let vals: Vec<f64> = cpus
        .iter()
        .filter_map(|c| {
            read_u64(format!("/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_cur_freq"))
        })
        .map(|khz| khz as f64 / 1000.0)
        .collect();
    if vals.is_empty() {
        return None;
    }
    Some(vals.iter().sum::<f64>() / vals.len() as f64)
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read_trim(p).and_then(|s| s.parse().ok())
}

/// Read a sysfs field and retain the error if it is unavailable.
fn read_required(p: impl AsRef<Path>) -> Result<String, String> {
    let p = p.as_ref();
    fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("{} could not be read: {e}", p.display()))
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

        let online = online_cpus();
        let logical_cpus = online.len();
        let mut errors: Vec<String> = Vec::new();

        // Derive the physical core count independently from package/core pairs
        // and sibling groups.
        let mut cores: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        let mut ids_ok = true;
        for cpu in &online {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let pkg = read_required(format!("{base}/physical_package_id"))
                .and_then(|v| v.parse::<u32>().map_err(|e| format!("{base}/physical_package_id: {e}")));
            let core = read_required(format!("{base}/core_id"))
                .and_then(|v| v.parse::<u32>().map_err(|e| format!("{base}/core_id: {e}")));
            match (pkg, core) {
                (Ok(p), Ok(c)) => {
                    cores.entry((p, c)).or_default().push(*cpu);
                }
                (Err(e), _) | (_, Err(e)) => {
                    // One line, not two hundred and fifty-six: the first
                    // failure already makes the derivation inadmissible.
                    errors.push(format!(
                        "the (physical_package_id, core_id) derivation of the physical core \
                         count is unusable: {e}"
                    ));
                    ids_ok = false;
                    break;
                }
            }
        }

        // Derivation B: distinct `thread_siblings_list` groups.
        let from_siblings = match dedup_to_physical(&online) {
            Ok(v) => Some(v.len()),
            Err(e) => {
                errors.push(format!(
                    "the thread_siblings_list derivation of the physical core count is \
                     unusable: {e}"
                ));
                None
            }
        };
        let from_ids = ids_ok.then(|| cores.len());
        let sockets_from_package_ids = ids_ok
            .then(|| cores.keys().map(|(p, _)| *p).collect::<std::collections::BTreeSet<_>>().len());

        // The cross-check. Two derivations that were never compared is how a
        // doubled core count survives: each looks locally plausible.
        errors.extend(core_count_disagreement(from_ids, from_siblings, logical_cpus));

        let physical_cores = from_ids.or(from_siblings).unwrap_or(0);
        let sockets = sockets_from_package_ids.unwrap_or(1).max(1);
        let threads_per_core =
            if physical_cores == 0 { 1 } else { (logical_cpus / physical_cores).max(1) };

        let numa = probe_numa(&mut errors);
        let l3_domains = probe_l3(&mut errors);

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
            numa,
            numa_distances: probe_distances(),
            l3_domains,
            host_total_memory_bytes: read_meminfo_kb("MemTotal").unwrap_or(0) * 1024,
            kernel: read_trim("/proc/sys/kernel/osrelease").unwrap_or_default(),
            sandbox: SandboxDescriptor::probe(),
            cgroup: probe_cgroup(),
            topology: Topology {
                physical_cores_from_core_ids: from_ids,
                physical_cores_from_thread_siblings: from_siblings,
                sockets_from_package_ids,
                errors,
            },
        }
    }

    /// Return an error if topology collection recorded an incomplete or inconsistent result.
    pub fn refuse_unsound_topology(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.topology.errors.is_empty(),
            "this machine's topology did not read soundly, so no per-core figure taken on it \
             would mean anything:\n  - {}\nsysfs is virtualised under sysbox; check that \
             /sys/devices/system/cpu is mounted and readable in this container.",
            self.topology.errors.join("\n  - ")
        );
        Ok(())
    }

    /// The process cgroup memory limit in bytes.
    ///
    /// `None` means unlimited or unreadable; [`CgroupDescriptor::memory_max`]
    /// distinguishes those cases.
    pub fn cell_memory_bytes(&self) -> Option<u64> {
        self.cgroup.memory_max.and_then(|m| m.bytes())
    }

    /// The process cgroup memory limit divided by the supplied core count (at least one), in GiB.
    pub fn cell_gib_per_core(&self, physical_cores: usize) -> Option<f64> {
        let b = self.cell_memory_bytes()?;
        Some(b as f64 / physical_cores.max(1) as f64 / (1u64 << 30) as f64)
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

    /// Whether every L3 domain containing the selected CPUs reported a size.
    ///
    /// If false, [`Machine::l3_bytes_for`] omits unknown capacities.
    pub fn l3_size_is_known_for(&self, cpus: &[usize]) -> bool {
        let touched: Vec<&CacheDomain> = self
            .l3_domains
            .iter()
            .filter(|d| d.cpus.iter().any(|c| cpus.contains(c)))
            .collect();
        !touched.is_empty() && touched.iter().all(|d| d.size_known)
    }
}

/// Report disagreement between two available physical-core counts.
///
/// A missing derivation has its own collection error and is not reported again.
fn core_count_disagreement(
    from_ids: Option<usize>,
    from_siblings: Option<usize>,
    logical_cpus: usize,
) -> Option<String> {
    match (from_ids, from_siblings) {
        (Some(a), Some(b)) if a != b => Some(format!(
            "the two derivations of the physical core count disagree: (physical_package_id, \
             core_id) pairs give {a}, thread_siblings_list gives {b}, over {logical_cpus} \
             online cpus. `physical_cores` is the denominator of every per-core figure in the \
             report, so no run may be planned until they agree."
        )),
        _ => None,
    }
}

fn online_cpus() -> Vec<usize> {
    read_trim("/sys/devices/system/cpu/online").map(|s| parse_cpu_list(&s)).unwrap_or_else(|| {
        (0..std::thread::available_parallelism().map_or(1, |n| n.get())).collect()
    })
}

/// One representative logical cpu per physical core, from a set of cpus.
///
/// Fails if the sibling topology cannot be read.
pub fn dedup_to_physical(cpus: &[usize]) -> Result<Vec<usize>, String> {
    let mut seen: std::collections::BTreeSet<Vec<usize>> = Default::default();
    let mut out = Vec::new();
    for &cpu in cpus {
        let raw =
            read_required(format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"))?;
        let sibs = parse_cpu_list(&raw);
        if sibs.is_empty() {
            return Err(format!(
                "/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list reads {raw:?}, \
                 which is not a cpu list"
            ));
        }
        if seen.insert(sibs) {
            out.push(cpu);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The SMT sibling of `cpu`.
///
/// `Ok(None)` means no sibling was found; `Err` means the topology could not
/// be read.
pub fn sibling_of(cpu: usize) -> Result<Option<usize>, String> {
    let raw =
        read_required(format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"))?;
    let sibs = parse_cpu_list(&raw);
    if sibs.is_empty() {
        return Err(format!(
            "/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list reads {raw:?}, \
             which is not a cpu list"
        ));
    }
    Ok(sibs.into_iter().find(|c| *c != cpu))
}

fn probe_numa(errors: &mut Vec<String>) -> Vec<NumaNode> {
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
        // An unreadable sibling list leaves the physical-core list empty;
        // the collection error is retained.
        let physical_cpus = match dedup_to_physical(&cpus) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("node {node}'s physical cpus could not be derived: {e}"));
                Vec::new()
            }
        };
        out.push(NumaNode { node, cpus, physical_cpus, host_memory_bytes: memory_bytes });
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

fn probe_l3(errors: &mut Vec<String>) -> Vec<CacheDomain> {
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
            // Retain the read error and mark this cache size unknown.
            let (size_bytes, size_known) = match read_required(format!("{d}/size"))
                .and_then(|v| {
                    parse_cache_size(&v).ok_or(format!("{d}/size reads {v:?}, which is not a cache size"))
                }) {
                Ok(b) => (b, true),
                Err(e) => {
                    errors.push(format!("L3 domain at cpu{cpu} has no readable size: {e}"));
                    (0, false)
                }
            };
            let physical_cpus = match dedup_to_physical(&cpus) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!(
                        "L3 domain at cpu{cpu}: its physical cpus could not be derived: {e}"
                    ));
                    Vec::new()
                }
            };
            by_id.entry(id).or_insert(CacheDomain {
                id,
                level: 3,
                size_bytes,
                size_known,
                cpus,
                physical_cpus,
            });
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
    let mut errors: Vec<String> = Vec::new();
    let memory_max = read_limit(root, "memory.max", &mut errors);
    let memory_high = read_limit(root, "memory.high", &mut errors);
    CgroupDescriptor {
        path,
        memory_max,
        memory_high,
        cpu_max: opt("cpu.max"),
        cpuset_cpus: opt("cpuset.cpus.effective"),
        cpuset_mems: opt("cpuset.mems.effective"),
        errors,
    }
}

/// Read one cgroup v2 limit file into a [`MemoryLimit`], distinguishing all
/// three of its states.
///
/// `Ok(Max)` for the literal `max` **and** for an absent file, because the
/// root cgroup has no `memory.max` and that genuinely means "nothing will
/// refuse an allocation". `None` for anything else — a permission error, a
/// truncated read, a value that is neither `max` nor a number — because those
/// mean *we do not know*, and the caller must not treat not knowing as not
/// limited. Every branch that returns something other than a byte count says
/// why in `errors`.
fn read_limit(root: &str, file: &str, errors: &mut Vec<String>) -> Option<MemoryLimit> {
    let path = format!("{root}/{file}");
    match fs::read_to_string(&path) {
        Ok(s) => match s.trim() {
            "max" => Some(MemoryLimit::Max),
            v => match v.parse::<u64>() {
                Ok(b) => Some(MemoryLimit::Bytes(b)),
                Err(e) => {
                    errors.push(format!(
                        "{path} reads {v:?}, which is neither `max` nor a byte count ({e}); \
                         the limit is unknown"
                    ));
                    None
                }
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            errors.push(format!(
                "{path} does not exist, so this process sits at a cgroup root (or on a v1 \
                 hierarchy, which this descriptor does not read): treated as no limit"
            ));
            Some(MemoryLimit::Max)
        }
        Err(e) => {
            errors.push(format!("{path} could not be read: {e}; the limit is unknown"));
            None
        }
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
    // `a` is written only by the asm block, through an `inout` operand, so
    // rustc sees no assignment and warns; the `#[allow]` is narrower than
    // dropping `mut`, which the operand requires.
    // `a` and `n` are written only by the asm block, through `inout`
    // operands, so rustc sees no assignment and warns; the narrow `#[allow]`
    // is preferable to dropping `mut`, which the operands require.
    #[allow(unused_mut)]
    let mut a: u64 = 1;
    #[allow(unused_mut)]
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

    /// An unavailable CPU topology entry returns an error.
    #[test]
    fn an_unreadable_siblings_list_is_an_error_and_not_one_core_per_cpu() {
        let e = dedup_to_physical(&[999_999]).unwrap_err();
        assert!(e.contains("thread_siblings_list"), "got: {e}");
        let e = sibling_of(999_999).unwrap_err();
        assert!(e.contains("thread_siblings_list"), "got: {e}");
        // …and the readable case still answers, so the guard is not simply
        // refusing everything.
        let cpu0 = dedup_to_physical(&[0]).expect("cpu0's siblings must be readable here");
        assert_eq!(cpu0, vec![0]);
    }

    /// Compare independent physical-core counts.
    #[test]
    fn two_core_counts_that_disagree_are_refused() {
        // 128 physical cores by (package, core_id); 256 by a siblings list
        // that came back one-cpu-per-group, which is exactly the shape the
        // old `unwrap_or_else(|| vec![cpu])` produced.
        let e = core_count_disagreement(Some(128), Some(256), 256).expect("must disagree");
        assert!(e.contains("128") && e.contains("256"), "got: {e}");

        let mut m = Machine::probe();
        assert!(
            m.refuse_unsound_topology().is_ok(),
            "this host's topology should read soundly; errors: {:?}",
            m.topology.errors
        );
        m.topology.errors.push(e);
        let err = format!("{:#}", m.refuse_unsound_topology().unwrap_err());
        assert!(err.contains("would mean anything"), "got: {err}");

        // Agreement, and a derivation that could not be made at all, are both
        // quiet here — the latter has already reported itself.
        assert_eq!(core_count_disagreement(Some(128), Some(128), 256), None);
        assert_eq!(core_count_disagreement(None, Some(256), 256), None);
    }

    /// Check consistency of topology data reported by the current machine.
    #[test]
    fn this_machines_two_core_counts_agree_and_its_l3_sizes_are_known() {
        let m = Machine::probe();
        assert_eq!(
            m.topology.physical_cores_from_core_ids, m.topology.physical_cores_from_thread_siblings,
            "the two derivations disagree on this host: {:?}",
            m.topology
        );
        assert!(m.physical_cores > 0);
        for d in &m.l3_domains {
            assert!(d.size_known && d.size_bytes > 0, "L3 domain {} has no size", d.id);
        }
        if let Some(n) = m.node(0) {
            assert!(m.l3_size_is_known_for(&n.physical_cpus));
        }
        // …and a cpu set no L3 domain covers is *not* "known to be zero".
        assert!(!m.l3_size_is_known_for(&[999_999]));
    }

    /// Distinguish a byte limit, an unlimited controller and an unreadable value.
    #[test]
    fn a_cgroup_limit_of_max_is_not_the_same_none_as_an_unreadable_one() {
        let dir = std::env::temp_dir().join(format!("wisp-cg-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let root = dir.display().to_string();
        fs::write(dir.join("memory.max"), "max
").unwrap();
        fs::write(dir.join("memory.high"), "51539607552
").unwrap();
        fs::write(dir.join("memory.low"), "not-a-number
").unwrap();

        let mut errs = Vec::new();
        assert_eq!(read_limit(&root, "memory.max", &mut errs), Some(MemoryLimit::Max));
        assert_eq!(
            read_limit(&root, "memory.high", &mut errs),
            Some(MemoryLimit::Bytes(51_539_607_552))
        );
        // Neither of those is a complaint about the *limit*, so nothing is
        // logged for them.
        assert!(errs.is_empty(), "got: {errs:?}");

        // Unparseable: unknown, and it says so.
        assert_eq!(read_limit(&root, "memory.low", &mut errs), None);
        assert!(errs[0].contains("neither `max` nor a byte count"), "got: {errs:?}");

        // Absent: no limit, and it says how it decided.
        let mut errs2 = Vec::new();
        assert_eq!(read_limit(&root, "memory.nope", &mut errs2), Some(MemoryLimit::Max));
        assert!(errs2[0].contains("does not exist"), "got: {errs2:?}");

        // `max` is not a byte count, and a caller asking for one gets `None`
        // — but from a value that is *known*.
        assert_eq!(MemoryLimit::Max.bytes(), None);
        assert_eq!(MemoryLimit::Bytes(7).bytes(), Some(7));

        // Round-trips, including the old shape: a bare number.
        let j = serde_json::to_string(&MemoryLimit::Max).unwrap();
        assert_eq!(j, "\"max\"");
        assert_eq!(serde_json::from_str::<MemoryLimit>(&j).unwrap(), MemoryLimit::Max);
        assert_eq!(
            serde_json::from_str::<MemoryLimit>("48000000000").unwrap(),
            MemoryLimit::Bytes(48_000_000_000)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Process memory limits come from the cgroup rather than host memory totals.
    #[test]
    fn the_cells_memory_budget_is_not_the_hosts() {
        let mut m = Machine::probe();
        m.cgroup.memory_max = Some(MemoryLimit::Bytes(51_539_607_552));
        assert_eq!(m.cell_memory_bytes(), Some(51_539_607_552));
        let gib = m.cell_gib_per_core(16).unwrap();
        assert!((gib - 3.0).abs() < 1e-9, "16 cores of a 48 GiB cell is 3 GiB/core, got {gib}");

        // No limit is not a budget of zero, and neither is an unreadable one.
        m.cgroup.memory_max = Some(MemoryLimit::Max);
        assert_eq!(m.cell_memory_bytes(), None);
        assert_eq!(m.cell_gib_per_core(16), None);
        m.cgroup.memory_max = None;
        assert_eq!(m.cell_gib_per_core(16), None);
    }

    /// Sample clock estimates under the requested CPU load.
    #[test]
    fn the_effective_clock_is_measured_at_two_loads_and_restores_the_caller() {
        let before = crate::affinity::current_affinity().unwrap();
        let cpus: Vec<usize> = before.iter().copied().take(4).collect();
        if cpus.len() < 2 {
            return;
        }
        let idle = EffectiveClock::measure(&cpus, 1, 30);
        let loaded = EffectiveClock::measure(&cpus, cpus.len(), 30);
        for e in [&idle, &loaded] {
            assert!(
                e.measured_mhz > 500.0 && e.measured_mhz < 8_000.0,
                "implausible clock: {e:?}"
            );
            assert_eq!(e.cpus_sampled, cpus);
        }
        assert_eq!(idle.busy_threads, 1);
        assert_eq!(loaded.busy_threads, cpus.len());
        assert_eq!(
            crate::affinity::current_affinity().unwrap(),
            before,
            "a descriptor probe must not repin the thread that asked for it"
        );
    }

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
