//! CPU-time and context-switch measurement helpers.

use std::io;
use std::time::Instant;

/// `CLOCK_PROCESS_CPUTIME_ID` — CPU consumed by every thread of this process.
///
/// This is the right clock for "what does a parked instance cost the cell",
/// because a per-thread clock would miss a reactor thread spinning on behalf
/// of parked instances — precisely the failure mode C3 is claiming does not
/// exist.
pub fn process_cpu_ns() -> u64 {
    clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID)
}

/// `CLOCK_THREAD_CPUTIME_ID` — the dispatch CPU clock. Mirrors
/// `wisp_host::meters::read_thread_cpu_ns`; duplicated here deliberately so
/// that a bug in the platform's meter cannot also be a bug in the instrument
/// measuring the meter.
pub fn thread_cpu_ns() -> u64 {
    clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
}

fn clock_ns(which: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, aligned `timespec` owned for the call's
    // duration; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(which, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

/// Voluntary/involuntary context switches, summed over **every thread** in the
/// process.
///
/// Included because a busy-wait and a correct park can both look cheap over a
/// short window, but they have completely different switch profiles: a parked
/// cell should be almost entirely *voluntary* switches (threads blocking on
/// epoll), while a spinner racks up involuntary ones as the kernel preempts
/// it. This is corroborating evidence for C3 and for S7's "the spinner
/// occupies only its own thread".
///
/// ## Why it walks `/proc/self/task/`
///
/// `/proc/self/status` reports the **thread group leader's** counters, not the
/// sum over the process. Reading it from a worker thread therefore reports the
/// main thread's numbers, which for a multi-threaded cell means watching a
/// thread that is doing nothing. The first version of this did exactly that,
/// and a test asserting "20 sleeps produce voluntary switches" failed on a
/// perfectly working machine, because the sleeps happened on a test thread and
/// the counter being read belonged to the main one.
///
/// Summing `/proc/self/task/*/status` gives the process-wide figure, which is
/// also the scope [`process_cpu_ns`] uses — the two numbers now describe the
/// same population, which they must if they are going to be read side by side.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Switches {
    pub voluntary: u64,
    pub involuntary: u64,
    /// How many threads were summed. Printed so a reader can tell a quiet
    /// single-threaded run from a busy cell.
    pub threads: u32,
}

impl Switches {
    pub fn now() -> io::Result<Self> {
        let mut out = Switches::default();
        for entry in std::fs::read_dir("/proc/self/task")? {
            let Ok(entry) = entry else { continue };
            // A thread can exit between the readdir and the read; that is not
            // an error, it is one fewer thread.
            let Ok(s) = std::fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            out.threads += 1;
            for line in s.lines() {
                let Some((k, v)) = line.split_once(':') else {
                    continue;
                };
                let n = v.trim().parse::<u64>().unwrap_or(0);
                match k {
                    "voluntary_ctxt_switches" => out.voluntary += n,
                    "nonvoluntary_ctxt_switches" => out.involuntary += n,
                    _ => {}
                }
            }
        }
        Ok(out)
    }
}

/// A wall-clock window over which process CPU was accumulated.
///
/// The pair is the point. CPU alone cannot distinguish "cheap" from "short";
/// only the ratio to wall time says whether anything was burning.
#[derive(Debug, Clone, Copy)]
pub struct CpuWindow {
    start_cpu_ns: u64,
    start: Instant,
    start_switches: Switches,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CpuUsage {
    pub cpu_ns: u64,
    pub wall_ns: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

impl CpuUsage {
    /// Fraction of one core consumed over the window. 1.0 = one core pegged.
    pub fn cores(&self) -> f64 {
        if self.wall_ns == 0 {
            return 0.0;
        }
        self.cpu_ns as f64 / self.wall_ns as f64
    }

    /// The number C3 actually claims: CPU-seconds per wall-second **per parked
    /// instance**. If this is flat as `parked` grows, waits are free; if it
    /// rises, they are not.
    pub fn cores_per_instance(&self, parked: u64) -> f64 {
        if parked == 0 {
            return self.cores();
        }
        self.cores() / parked as f64
    }

    /// Total dispatch CPU-ms over the window.
    pub fn cpu_ms(&self) -> f64 {
        self.cpu_ns as f64 / 1e6
    }
}

impl CpuWindow {
    pub fn start() -> Self {
        Self {
            start_cpu_ns: process_cpu_ns(),
            start: Instant::now(),
            start_switches: Switches::now().unwrap_or_default(),
        }
    }

    pub fn read(&self) -> CpuUsage {
        let end = Switches::now().unwrap_or_default();
        CpuUsage {
            cpu_ns: process_cpu_ns().saturating_sub(self.start_cpu_ns),
            wall_ns: self.start.elapsed().as_nanos() as u64,
            voluntary_switches: end.voluntary.saturating_sub(self.start_switches.voluntary),
            involuntary_switches: end
                .involuntary
                .saturating_sub(self.start_switches.involuntary),
        }
    }
}

/// What this process costs per wall-second with the workload *absent*.
///
/// C3's assertion is about the marginal cost of a parked instance, and the
/// only way to isolate that is to know what the process burns when nothing is
/// parked. Measuring the floor also makes the claim falsifiable in the honest
/// direction: if the floor is 0.30 cores because a reactor is polling, S3
/// reports that number rather than subtracting it into invisibility.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct NoiseFloor {
    pub cores: f64,
    pub sampled_ms: u64,
}

impl NoiseFloor {
    /// Sample for `ms` with the process doing whatever it does at rest.
    pub fn measure(ms: u64) -> Self {
        Self::measure_settled(0, ms)
    }

    /// Sleep `settle_ms` first, then sample for `ms`.
    ///
    /// The settle is not politeness, it is the difference between a floor and
    /// a tail. Measured immediately after a template is compiled and loaded,
    /// the floor picks up cranelift's thread pool winding down and reads an
    /// order of magnitude high — on a real run, 0.0598 cores against a true
    /// rest cost near 0.0002. A floor that is too *high* is the dangerous
    /// direction: [`net_cores`](Self::net_cores) clamps at zero, so an
    /// inflated floor subtracts a real cost into an apparent zero and the
    /// scenario reports a pass it did not earn.
    pub fn measure_settled(settle_ms: u64, ms: u64) -> Self {
        if settle_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(settle_ms));
        }
        let w = CpuWindow::start();
        std::thread::sleep(std::time::Duration::from_millis(ms));
        let u = w.read();
        Self {
            cores: u.cores(),
            sampled_ms: ms,
        }
    }

    /// A window's cost with the floor removed, clamped at zero — a negative
    /// marginal cost is measurement noise, not a discovery.
    ///
    /// **The clamp is why this must never be the only number reported.** If
    /// the floor was over-measured, every window under it reads as exactly
    /// 0.00000 cores, which looks like the strongest possible result and is in
    /// fact the absence of one. Report [`CpuUsage::cores`] alongside it.
    pub fn net_cores(&self, u: &CpuUsage) -> f64 {
        (u.cores() - self.cores).max(0.0)
    }

    /// True when [`net_cores`](Self::net_cores) hit the clamp — i.e. the
    /// window cost *less* than the floor, so the subtraction told us nothing.
    pub fn clamped(&self, u: &CpuUsage) -> bool {
        u.cores() < self.cores
    }

    /// Whether the floor is quiet enough for a marginal measurement to mean
    /// anything. A process already burning a third of a core cannot
    /// demonstrate that 1,000 parked instances cost nothing.
    pub fn is_quiet(&self) -> bool {
        self.cores < 0.05
    }
}

/// Burn approximately `ms` of CPU on this thread. Used to calibrate the
/// instrument and to give S7's healthy-neighbour measurement something real to
/// contend with.
pub fn burn_cpu_ms(ms: u64) {
    let target = thread_cpu_ns() + ms * 1_000_000;
    let mut x = 0u64;
    while thread_cpu_ns() < target {
        for i in 0..10_000u64 {
            x = x.wrapping_add(i ^ x).rotate_left(7);
        }
    }
    std::hint::black_box(x);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The instrument check for C3. If sleeping registered as CPU, the whole
    /// free-waits measurement would be meaningless — so this is asserted
    /// before any scenario is allowed to rely on it.
    ///
    /// It is asserted on the *thread* clock rather than the process clock,
    /// because `cargo test` runs these tests on parallel threads and sibling
    /// tests burning CPU land in a process-clock window. That contamination is
    /// real and is the reason S3 runs from the single-threaded runner binary
    /// and reports net of a [`NoiseFloor`]; see the module docs.
    #[test]
    fn sleeping_costs_no_cpu() {
        let before = thread_cpu_ns();
        let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        let burned = thread_cpu_ns() - before;
        let wall = start.elapsed().as_nanos() as u64;
        assert!(wall >= 250_000_000, "the window did not actually elapse");
        assert!(
            (burned as f64 / wall as f64) < 0.05,
            "300 ms of sleep read as {burned} ns of thread CPU over {wall} ns wall"
        );
    }

    /// The process clock must dominate the thread clock — it is the sum over
    /// threads. This is the sanity check that the two clocks were not
    /// transposed, which would silently turn every free-waits number into a
    /// per-thread one and hide exactly the reactor-spin failure C3 denies.
    #[test]
    fn the_process_clock_includes_every_thread() {
        let p0 = process_cpu_ns();
        let t0 = thread_cpu_ns();
        let h = std::thread::spawn(|| burn_cpu_ms(80));
        burn_cpu_ms(80);
        h.join().unwrap();
        let dp = process_cpu_ns() - p0;
        let dt = thread_cpu_ns() - t0;
        assert!(
            dp > dt,
            "two threads burned 80 ms each; process clock {dp} ns did not exceed \
             this thread's {dt} ns"
        );
    }

    /// The floor is a measurement, not an assumption: it is allowed to be
    /// noisy (this test binary is), and the only thing asserted is that the
    /// type reports honestly about it.
    #[test]
    fn the_noise_floor_reports_itself() {
        let f = NoiseFloor::measure(50);
        assert_eq!(f.sampled_ms, 50);
        assert!(f.cores >= 0.0);
        let quiet = CpuUsage {
            cpu_ns: 0,
            wall_ns: 1_000_000_000,
            voluntary_switches: 0,
            involuntary_switches: 0,
        };
        // Subtracting a floor can never manufacture negative CPU.
        assert!(f.net_cores(&quiet) >= 0.0);
        assert_eq!(
            NoiseFloor {
                cores: 0.5,
                sampled_ms: 10
            }
            .is_quiet(),
            false
        );
    }

    #[test]
    fn burning_costs_process_cpu() {
        let w = CpuWindow::start();
        burn_cpu_ms(100);
        let u = w.read();
        assert!(
            u.cpu_ns > 50_000_000,
            "burning 100 ms read as {} ns",
            u.cpu_ns
        );
        assert!(
            u.cores() > 0.5,
            "a busy loop should approach one core, got {:.3}",
            u.cores()
        );
    }

    /// A sleeper yields voluntarily; a spinner gets preempted. S7 leans on the
    /// distinction, so check the instrument can see it.
    ///
    /// This is also the regression test for the scope bug: reading
    /// `/proc/self/status` from a test thread reports the *main* thread's
    /// counters, which do not move while this thread sleeps. Summing over
    /// `/proc/self/task/` is what makes the number describe the process.
    #[test]
    fn switch_counters_move() {
        let before = Switches::now().unwrap();
        assert!(
            before.threads >= 1,
            "at least this thread should have been counted"
        );
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(1));
        }
        let after = Switches::now().unwrap();
        assert!(
            after.voluntary > before.voluntary,
            "20 sleeps produced no voluntary context switches \
             (before {} over {} threads, after {} over {} threads)",
            before.voluntary,
            before.threads,
            after.voluntary,
            after.threads,
        );
    }

    /// The scope must match `process_cpu_ns`'s, or the two numbers printed
    /// side by side in S3 would describe different populations.
    #[test]
    fn switches_are_summed_over_every_thread() {
        let before = Switches::now().unwrap();
        let h = std::thread::spawn(|| {
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        h.join().unwrap();
        let after = Switches::now().unwrap();
        assert!(
            after.voluntary > before.voluntary,
            "a *different* thread's 50 sleeps must show up in the process total"
        );
    }

    #[test]
    fn per_instance_rate_divides_by_the_population() {
        let u = CpuUsage {
            cpu_ns: 1_000_000,
            wall_ns: 10_000_000_000,
            voluntary_switches: 0,
            involuntary_switches: 0,
        };
        assert!((u.cores() - 0.0001).abs() < 1e-9);
        assert!((u.cores_per_instance(1000) - 1e-7).abs() < 1e-12);
    }

    /// The failure this module was corrected for. An over-measured floor makes
    /// `net_cores` clamp, and a clamped zero is indistinguishable in the
    /// report from a genuine zero unless the clamp is announced. S3 reads a
    /// real 1.8e-6 cores/instance as "0.000e0" if it trusts the subtraction
    /// alone, which is a measurement bug in the flattering direction.
    #[test]
    fn an_over_measured_floor_clamps_and_says_so() {
        let floor = NoiseFloor {
            cores: 0.0598,
            sampled_ms: 500,
        };
        let window = CpuUsage {
            cpu_ns: 18_161_000,
            wall_ns: 10_018_000_000,
            voluntary_switches: 1,
            involuntary_switches: 0,
        };
        // The window really did cost something: 1.81e-3 cores gross.
        assert!(
            (window.cores() - 0.001813).abs() < 1e-5,
            "gross was {}",
            window.cores()
        );
        // ...which the subtraction erases entirely.
        assert_eq!(floor.net_cores(&window), 0.0);
        assert!(
            floor.clamped(&window),
            "the clamp must be visible to the caller"
        );

        // A floor genuinely below the window subtracts honestly and does not
        // report itself as clamped.
        let quiet = NoiseFloor {
            cores: 0.0002,
            sampled_ms: 2000,
        };
        assert!(!quiet.clamped(&window));
        assert!(quiet.net_cores(&window) > 0.0015);
    }

    #[test]
    fn a_settled_floor_still_measures_a_window() {
        let f = NoiseFloor::measure_settled(20, 60);
        assert_eq!(f.sampled_ms, 60);
        assert!(f.cores >= 0.0);
    }
}
