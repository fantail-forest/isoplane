//! Linux CPU affinity helpers for CPU sets and selected threads.
//!
//! Thread-name matching accepts prefixes because Linux truncates comm names.

use std::io;

/// Pin the calling thread to `cpus`.
pub fn pin_current(cpus: &[usize]) -> io::Result<()> {
    pin_tid(0, cpus)
}

/// Pin an arbitrary task by tid (`0` means the caller).
pub fn pin_tid(tid: libc::pid_t, cpus: &[usize]) -> io::Result<()> {
    if cpus.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty cpu set"));
    }
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for &c in cpus {
            if c >= libc::CPU_SETSIZE as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("cpu {c} out of range")));
            }
            libc::CPU_SET(c, &mut set);
        }
        if libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The cpus the calling thread is currently allowed to run on.
pub fn current_affinity() -> io::Result<Vec<usize>> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((0..libc::CPU_SETSIZE as usize).filter(|c| libc::CPU_ISSET(*c, &set)).collect())
    }
}

/// Which cpu the caller is executing on right now.
pub fn current_cpu() -> usize {
    unsafe { libc::sched_getcpu().max(0) as usize }
}

/// Pin every thread of this process whose `comm` starts with `prefix`.
///
/// Returns the tids it moved so callers can check which threads were selected.
pub fn pin_by_comm(prefix: &str, cpus: &[usize]) -> io::Result<Vec<libc::pid_t>> {
    let mut moved = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let Ok(tid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else { continue };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim_end().starts_with(prefix) {
            pin_tid(tid, cpus)?;
            moved.push(tid);
        }
    }
    Ok(moved)
}

/// Every thread of this process, as `(tid, comm)`.
pub fn threads() -> io::Result<Vec<(libc::pid_t, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let Ok(tid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else { continue };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        out.push((tid, comm.trim_end().to_string()));
    }
    out.sort_unstable();
    Ok(out)
}

// ---------------------------------------------------------------------------
// NUMA memory policy
// ---------------------------------------------------------------------------

const MPOL_DEFAULT: i32 = 0;
const MPOL_BIND: i32 = 2;
const MPOL_INTERLEAVE: i32 = 3;

/// Where a thread's *subsequent* allocations come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemPolicy {
    /// The kernel's default: first-touch local.
    Default,
    /// Allocate only from these nodes; fail (or reclaim) rather than spill.
    Bind(Vec<usize>),
    /// Round-robin across these nodes.
    Interleave(Vec<usize>),
}

fn nodemask(nodes: &[usize]) -> (Vec<u64>, u64) {
    let maxnode = nodes.iter().copied().max().map(|m| m + 1).unwrap_or(1);
    let words = maxnode.div_ceil(64).max(1);
    let mut mask = vec![0u64; words];
    for &n in nodes {
        mask[n / 64] |= 1u64 << (n % 64);
    }
    // The kernel's `maxnode` is a *bit count*, and it wants room for the
    // whole last word: passing exactly `max+1` works, but passing the full
    // allocated width is what the libnuma wrappers do and is less fragile.
    (mask, (words * 64) as u64)
}

/// Set the calling thread's NUMA allocation policy.
///
/// Threads subsequently created by the caller inherit this policy.
pub fn set_mempolicy(p: &MemPolicy) -> io::Result<()> {
    let (mode, nodes): (i32, &[usize]) = match p {
        MemPolicy::Default => (MPOL_DEFAULT, &[]),
        MemPolicy::Bind(n) => (MPOL_BIND, n),
        MemPolicy::Interleave(n) => (MPOL_INTERLEAVE, n),
    };
    let rc = if nodes.is_empty() {
        unsafe { libc::syscall(libc::SYS_set_mempolicy, mode, std::ptr::null::<u64>(), 0u64) }
    } else {
        let (mask, maxnode) = nodemask(nodes);
        unsafe { libc::syscall(libc::SYS_set_mempolicy, mode, mask.as_ptr(), maxnode) }
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read back the calling thread's policy mode, so a bind can be *verified*
/// rather than assumed. Returns `(mode, nodes)`.
pub fn get_mempolicy() -> io::Result<(i32, Vec<usize>)> {
    let mut mode: i32 = -1;
    let words = 8usize; // 512 nodes is far past anything real.
    let mut mask = vec![0u64; words];
    let rc = unsafe {
        libc::syscall(
            libc::SYS_get_mempolicy,
            &mut mode as *mut i32,
            mask.as_mut_ptr(),
            (words * 64) as u64,
            std::ptr::null::<libc::c_void>(),
            0u64,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut nodes = Vec::new();
    for (w, word) in mask.iter().enumerate() {
        for b in 0..64 {
            if word & (1u64 << b) != 0 {
                nodes.push(w * 64 + b);
            }
        }
    }
    Ok((mode, nodes))
}

/// Per-node mapping page counts multiplied by the system page size.
///
/// Reads every `N<node>=<pages>` field in `/proc/self/numa_maps`.
pub fn numa_residency() -> io::Result<std::collections::BTreeMap<usize, u64>> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let mut out = std::collections::BTreeMap::new();
    let text = std::fs::read_to_string("/proc/self/numa_maps")?;
    for line in text.lines() {
        for tok in line.split_whitespace() {
            let Some(rest) = tok.strip_prefix('N') else { continue };
            let Some((node, pages)) = rest.split_once('=') else { continue };
            let (Ok(node), Ok(pages)) = (node.parse::<usize>(), pages.parse::<u64>()) else {
                continue;
            };
            *out.entry(node).or_insert(0) += pages * page;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nodemask_sets_the_right_bit() {
        let (m, maxnode) = nodemask(&[0]);
        assert_eq!(m[0], 1);
        assert_eq!(maxnode, 64);
        let (m, _) = nodemask(&[4]);
        assert_eq!(m[0], 1 << 4);
        let (m, _) = nodemask(&[0, 4, 7]);
        assert_eq!(m[0], 1 | (1 << 4) | (1 << 7));
        let (m, maxnode) = nodemask(&[65]);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1], 1 << 1);
        assert_eq!(maxnode, 128);
    }

    /// Pinning has to be checked by *observing where the thread runs*, not by
    /// the syscall returning 0. A `sched_setaffinity` that silently applies to
    /// the wrong task — the mistake `pin_by_comm` is one typo away from — also
    /// returns 0.
    #[test]
    fn pinning_actually_moves_the_thread() {
        let all = current_affinity().expect("affinity readable");
        assert!(!all.is_empty());
        // Pick a cpu we are allowed to use that is not the one we are on, so
        // a no-op would fail this.
        let here = current_cpu();
        let Some(&target) = all.iter().find(|c| **c != here) else { return };
        let h = std::thread::spawn(move || {
            pin_current(&[target]).expect("pin");
            // One yield so the scheduler has an excuse to move us.
            std::thread::yield_now();
            let mut seen = current_cpu();
            for _ in 0..1000 {
                seen = current_cpu();
                if seen == target {
                    break;
                }
                std::thread::yield_now();
            }
            (seen, current_affinity().unwrap())
        });
        let (seen, aff) = h.join().unwrap();
        assert_eq!(aff, vec![target], "affinity mask should be exactly the pinned cpu");
        assert_eq!(seen, target, "a pinned thread must actually run on its cpu");
        // And the pin must not have leaked to the parent.
        assert_eq!(current_affinity().unwrap(), all);
    }

    #[test]
    fn comm_matching_finds_our_own_named_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let h = std::thread::Builder::new()
            .name("wispbenchprobe".into())
            .spawn(move || {
                tx.send(()).unwrap();
                let _ = stop_rx.recv();
            })
            .unwrap();
        rx.recv().unwrap();
        let names: Vec<String> = threads().unwrap().into_iter().map(|(_, c)| c).collect();
        assert!(
            names.iter().any(|c| c.starts_with("wispbenchprobe")),
            "expected our named thread among {names:?}"
        );
        let _ = stop_tx.send(());
        h.join().unwrap();
    }
}
