//! Timestamp-counter marks and calibration for x86-64 host measurements.
//! Inspect the reported cross-core and resolution limits when interpreting intervals.

// Timestamp-counter intrinsics require this module's scoped unsafe-code allowance.
#![allow(unsafe_code, unused_unsafe)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("tsc_marks requires x86_64: it uses the RDTSC/RDTSCP instructions");

use core::arch::x86_64::{__cpuid, __rdtscp, _mm_lfence};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Counter-rate, resolution and mark-overhead observations, with CPU and clocksource flags.
/// Calibrate outside request execution and reuse the resulting readings.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    tsc_hz: f64,
    mark_overhead: u64,
    resolution_ticks: u64,
    invariant: bool,
    clocksource_tsc: bool,
}

impl Calibration {
    /// Calibrates with the default sampling window.
    pub fn measure() -> Self {
        Self::measure_for(Duration::from_millis(200))
    }

    /// Measures the counter against the OS monotonic clock for the requested window,
    /// then estimates mark overhead after warmup.
    pub fn measure_for(window: Duration) -> Self {
        let invariant = read_invariant_tsc();
        let clocksource_tsc = kernel_clocksource_is_tsc();

        // Bracket the requested sleep with counter and monotonic-clock readings.
        // The rate estimate includes endpoint and scheduling effects.
        let mut aux = 0u32;
        let wall_start = Instant::now();
        let tsc_start = unsafe { __rdtscp(&mut aux) };
        std::thread::sleep(window);
        let tsc_end = unsafe { __rdtscp(&mut aux) };
        let wall = wall_start.elapsed().as_secs_f64();
        let tsc_hz = tsc_end.wrapping_sub(tsc_start) as f64 / wall;

        // Estimate overhead from the smallest observed mark gap, with bounded warmup
        // to reduce startup effects. Report the greatest common divisor of observed
        // gaps as a counter-step estimate.
        let mut buf: Vec<(u64, u32)> = Vec::with_capacity(4096);
        let mut mark_overhead = u64::MAX;
        let mut step = 0u64;
        // Warm up before sampling to reduce startup effects on the overhead estimate.
        let spin_floor = Instant::now() + Duration::from_millis(100);
        let hard_cap = Instant::now() + Duration::from_millis(400);
        loop {
            buf.clear();
            for _ in 0..4096 {
                let mut aux = 0u32;
                let t = unsafe { __rdtscp(&mut aux) };
                buf.push((t, aux));
            }
            let mut pass_min = u64::MAX;
            for w in buf.windows(2) {
                let gap = w[1].0.wrapping_sub(w[0].0);
                pass_min = pass_min.min(gap);
                step = gcd(step, gap);
            }
            let improved = pass_min < mark_overhead;
            mark_overhead = mark_overhead.min(pass_min);
            let now = Instant::now();
            if (now >= spin_floor && !improved) || now >= hard_cap {
                break;
            }
        }
        let resolution_ticks = step.max(1);

        Self { tsc_hz, mark_overhead, resolution_ticks, invariant, clocksource_tsc }
    }

    /// The measured tick rate in Hz. Typically close to the CPU's advertised
    /// base frequency, e.g. `2.995e9` on a "3.0 GHz" part.
    pub fn tsc_hz(&self) -> f64 {
        self.tsc_hz
    }

    /// Smallest mark gap observed during calibration, in counter ticks.
    /// Interval reports subtract this estimate from measured spans.
    pub fn mark_overhead_ticks(&self) -> u64 {
        self.mark_overhead
    }

    /// Observed counter-step estimate from the greatest common divisor of positive calibration gaps.
    pub fn resolution_ticks(&self) -> u64 {
        self.resolution_ticks
    }

    /// [`resolution_ticks`](Calibration::resolution_ticks) converted to
    /// nanoseconds.
    pub fn resolution_nanos(&self) -> f64 {
        self.ticks_to_nanos(self.resolution_ticks)
    }

    /// Returns the recorded CPU invariant-TSC capability flag.
    pub fn has_invariant_tsc(&self) -> bool {
        self.invariant
    }

    /// Whether both the recorded invariant-TSC and kernel clocksource flags are set.
    /// These flags do not independently verify agreement between counters on different cores.
    pub fn cross_core_ok(&self) -> bool {
        self.invariant && self.clocksource_tsc
    }

    /// Convert a tick count to nanoseconds using the calibrated rate.
    pub fn ticks_to_nanos(&self, ticks: u64) -> f64 {
        ticks as f64 * 1e9 / self.tsc_hz
    }
}

/// Ask the CPU (via `CPUID` leaf `0x8000_0007`, bit 8 of EDX) whether the
/// TSC is invariant.
fn read_invariant_tsc() -> bool {
    unsafe {
        if __cpuid(0x8000_0000).eax < 0x8000_0007 {
            return false;
        }
        __cpuid(0x8000_0007).edx & (1 << 8) != 0
    }
}

/// Greatest common divisor; `gcd(0, x) == x`, so it can be folded over a
/// stream of gaps starting from zero.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Ask the Linux kernel whether it trusts the TSC enough to use it as the
/// system clocksource. Its boot-time sync test across all CPUs is the
/// authority on whether cross-core/cross-socket deltas are meaningful.
#[cfg(target_os = "linux")]
fn kernel_clocksource_is_tsc() -> bool {
    std::fs::read_to_string(
        "/sys/devices/system/clocksource/clocksource0/current_clocksource",
    )
    .map(|s| s.trim() == "tsc")
    .unwrap_or(false)
}

/// On non-Linux systems we cannot verify cross-core synchronization, so be
/// conservative.
#[cfg(not(target_os = "linux"))]
fn kernel_clocksource_is_tsc() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct StampRec<L> {
    label: L,
    ticks: u64,
    core: u32,
}

/// A reusable buffer of labeled, core-tagged timestamps for one run.
///
/// Usage per run: [`start`](Marks::start), then any number of
/// [`mark`](Marks::mark) calls, then [`stop`](Marks::stop), then
/// [`report`](Marks::report). Calling `start` again begins a fresh run and
/// reuses the same buffer — no allocation after construction.
///
/// The label you pass to `mark` / `stop` names **the work since the
/// previous timestamp**, i.e. the stage that just finished.
///
/// A `Marks` is single-owner but `Send`: in a pipeline it travels *with*
/// the request, and each stage — on whichever thread runs it — calls
/// `mark`. Sequential per-request use across threads is exactly the
/// intended pattern; concurrent use of one `Marks` from two threads is not
/// possible without `unsafe` and would be wrong. For pooled per-request
/// checkout, see [`MarksPool`].
pub struct Marks<L> {
    cal: Calibration,
    start: u64,
    start_core: u32,
    stamps: Vec<StampRec<L>>,
}

impl<L: Copy> Marks<L> {
    /// Creates a buffer with capacity timestamp slots, including the final stop mark.
    pub fn new(cal: Calibration, capacity: usize) -> Self {
        Self {
            cal,
            start: 0,
            start_core: 0,
            stamps: Vec::with_capacity(capacity),
        }
    }

    /// Begin a run. Clears any previous run's timestamps.
    ///
    /// Reads the counter with `RDTSCP`, which waits for all the setup
    /// before this call to actually finish, then issues a fence so the code
    /// being measured can't begin before the read. Setup cost is recorded
    /// *before* the first stage, where it belongs.
    #[inline(always)]
    pub fn start(&mut self) {
        self.stamps.clear();
        let mut aux = 0u32;
        let t = unsafe { __rdtscp(&mut aux) };
        self.start = t;
        self.start_core = aux;
        unsafe { _mm_lfence() };
    }

    /// Record a timestamp; `label` names the stage that just ended.
    ///
    /// Uses `RDTSCP`: it waits until the previous stage's instructions have
    /// actually finished (a firm boundary behind it) and records which core
    /// it ran on, so [`report`](Marks::report) can flag intervals that
    /// crossed cores. The cost is a few nanoseconds, measured during
    /// calibration and subtracted from every reported interval.
    #[inline(always)]
    pub fn mark(&mut self, label: L) {
        debug_assert!(self.start != 0, "mark() called before start()");
        debug_assert!(
            self.stamps.len() < self.stamps.capacity(),
            "Marks buffer full: raise `capacity` (a reallocation here would wreck the run)"
        );
        let mut aux = 0u32;
        let t = unsafe { __rdtscp(&mut aux) };
        self.stamps.push(StampRec { label, ticks: t, core: aux });
    }

    /// End the run; `label` names the final stage.
    ///
    /// Uses `RDTSCP` (waits for the final stage to actually finish, records
    /// the core), then fences so nothing that runs after `stop()` sneaks
    /// into the measurement. Bookkeeping happens after the fence and is not
    /// attributed to the region.
    #[inline(always)]
    pub fn stop(&mut self, label: L) {
        let mut aux = 0u32;
        let t = unsafe { __rdtscp(&mut aux) };
        unsafe { _mm_lfence() };
        debug_assert!(self.start != 0, "stop() called before start()");
        debug_assert!(
            self.stamps.len() < self.stamps.capacity(),
            "Marks buffer full: raise `capacity`"
        );
        self.stamps.push(StampRec { label, ticks: t, core: aux });
    }

    /// Turn the raw timestamps of the last run into per-stage durations.
    ///
    /// For each interval this subtracts the calibrated cost of the mark
    /// that ended it, so what you see is the stage itself, not the
    /// measuring. Call this after `stop()`, never inside a run.
    pub fn report(&self) -> Report<L> {
        debug_assert!(!self.stamps.is_empty(), "report() called before stop()");
        let mut prev_t = self.start;
        let mut prev_core = self.start_core;
        let mut intervals = Vec::with_capacity(self.stamps.len());
        let mut total_ticks = 0u64;
        let mut crossed = false;
        for &StampRec { label, ticks: t, core } in &self.stamps {
            let raw = t.wrapping_sub(prev_t);
            let oh = self.cal.mark_overhead;
            let ticks = raw.saturating_sub(oh);
            let cross_core = core != prev_core;
            crossed |= cross_core;
            total_ticks += ticks;
            intervals.push(Interval {
                label,
                ticks_raw: raw,
                ticks,
                nanos: self.cal.ticks_to_nanos(ticks),
                at_floor: raw < 2 * oh.max(self.cal.resolution_ticks),
                cross_core,
                cores: (prev_core, core),
            });
            prev_t = t;
            prev_core = core;
        }
        Report {
            intervals,
            total_ticks,
            total_nanos: self.cal.ticks_to_nanos(total_ticks),
            crossed_cores: crossed,
            cross_core_ok: self.cal.cross_core_ok(),
            resolution_ticks: self.cal.resolution_ticks,
            resolution_nanos: self.cal.resolution_nanos(),
        }
    }

    /// The calibration this buffer was built with.
    pub fn calibration(&self) -> &Calibration {
        &self.cal
    }

    // -----------------------------------------------------------------------
    // Added for this repository. Nothing above this line is changed: these are
    // read-only accessors plus one alternative total, all of which belong to
    // the report-generation half rather than to the timing half.
    // -----------------------------------------------------------------------

    /// How many timestamps the current run has taken.
    ///
    /// Needed because the caller, not this module, has to decide what happens
    /// when a run wants more marks than the buffer holds. `mark` debug-asserts
    /// and would reallocate in release — inside the timed region — so the
    /// caller checks this and truncates instead.
    pub fn len(&self) -> usize {
        self.stamps.len()
    }

    /// Whether the current run has taken no timestamps yet.
    pub fn is_empty(&self) -> bool {
        self.stamps.is_empty()
    }

    /// The capacity this buffer was preallocated with.
    pub fn capacity(&self) -> usize {
        self.stamps.capacity()
    }

    /// **Last timestamp minus the first, uncorrected.**
    ///
    /// [`Report::total_ticks`] is the sum of the *overhead-corrected* spans,
    /// which is shorter than this by roughly one mark's cost per span. Both
    /// are useful and they answer different questions: the corrected sum is
    /// "how long did the work take", and this is "how long was the region" —
    /// the number that has to reconcile against a measurement taken from
    /// outside by a different clock.
    pub fn span_ticks(&self) -> u64 {
        match self.stamps.last() {
            Some(last) => last.ticks.wrapping_sub(self.start),
            None => 0,
        }
    }

    /// [`Marks::span_ticks`] in nanoseconds.
    pub fn span_nanos(&self) -> f64 {
        self.cal.ticks_to_nanos(self.span_ticks())
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// One measured stage: the label of the mark that ended it, its size in raw
/// ticks, overhead-corrected ticks and nanoseconds, and where it ran.
#[derive(Clone, Copy, Debug)]
pub struct Interval<L> {
    /// The label passed to the `mark()` / `stop()` that ended this stage.
    pub label: L,
    /// Ticks between the two timestamps, before any correction.
    pub ticks_raw: u64,
    /// Ticks with the per-mark cost subtracted. This is the number to use.
    pub ticks: u64,
    /// [`Interval::ticks`] converted to nanoseconds.
    pub nanos: f64,
    /// True if the raw interval was so small that it's mostly the cost of
    /// measuring itself or below the counter's step. Treat such values as
    /// "approximately zero".
    pub at_floor: bool,
    /// True if this interval's endpoints were taken on different cores.
    /// Meaningful only alongside [`Report::cross_core_ok`].
    pub cross_core: bool,
    /// The cores the two endpoint timestamps were taken on
    /// (`start_core`, `end_core`).
    pub cores: (u32, u32),
}

/// The processed result of one run: per-stage intervals in order, plus the
/// total.
///
/// Printing a report with `{}` gives an aligned table; intervals at the
/// measurement floor are starred, cross-core intervals are daggered with
/// the cores shown, a warning is appended if cross-core intervals exist
/// but the TSC was not verified synchronized across cores, and the last
/// line states the counter's resolution — the error bar every individual
/// interval carries.
#[derive(Clone, Debug)]
pub struct Report<L> {
    /// The stages, in the order they were marked.
    pub intervals: Vec<Interval<L>>,
    /// Sum of the corrected per-stage ticks.
    pub total_ticks: u64,
    /// [`Report::total_ticks`] in nanoseconds.
    pub total_nanos: f64,
    /// True if any interval crossed cores. Normal and fine in a
    /// work-stealing pipeline when [`Report::cross_core_ok`] is true.
    pub crossed_cores: bool,
    /// Whether both the recorded invariant-TSC and kernel clocksource flags are set.
    /// These flags do not independently verify agreement between counters on different cores.
    pub cross_core_ok: bool,
    /// Observed counter-step estimate from the greatest common divisor of positive calibration gaps.
    pub resolution_ticks: u64,
    /// [`Report::resolution_ticks`] in nanoseconds.
    pub resolution_nanos: f64,
}

impl<L: Copy> Report<L> {
    /// Merges corresponding intervals, retaining each interval's minimum.
    /// Intervals are matched by position; differing mark orders combine unrelated stages.
    pub fn keep_min(&mut self, other: &Report<L>) {
        debug_assert_eq!(
            self.intervals.len(),
            other.intervals.len(),
            "keep_min: runs must have the same sequence of marks"
        );
        let mut total_ticks = 0u64;
        let mut total_nanos = 0f64;
        let mut crossed = false;
        for (s, o) in self.intervals.iter_mut().zip(&other.intervals) {
            if o.ticks < s.ticks {
                *s = *o;
            }
            total_ticks += s.ticks;
            total_nanos += s.nanos;
            crossed |= s.cross_core;
        }
        self.total_ticks = total_ticks;
        self.total_nanos = total_nanos;
        self.crossed_cores = crossed;
        self.cross_core_ok &= other.cross_core_ok;
    }
}

impl<L: fmt::Debug> fmt::Display for Report<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .intervals
            .iter()
            .map(|iv| format!("{:?}", iv.label).len())
            .max()
            .unwrap_or(0)
            .max("TOTAL".len());
        for iv in &self.intervals {
            let mut suffix = String::new();
            if iv.at_floor {
                suffix.push_str("  *");
            }
            if iv.cross_core {
                suffix.push_str(&format!("  \u{2020}({}\u{2192}{})", iv.cores.0, iv.cores.1));
            }
            writeln!(
                f,
                "{:<width$}  {:>9} ticks  {:>10.1} ns{}",
                format!("{:?}", iv.label),
                iv.ticks,
                iv.nanos,
                suffix,
                width = width
            )?;
        }
        writeln!(
            f,
            "{:<width$}  {:>9} ticks  {:>10.1} ns",
            "TOTAL",
            self.total_ticks,
            self.total_nanos,
            width = width
        )?;
        if self.intervals.iter().any(|iv| iv.at_floor) {
            writeln!(f, "* at or below the sampling floor -- treat as noise")?;
        }
        if self.crossed_cores {
            if self.cross_core_ok {
                writeln!(
                    f,
                    "\u{2020} endpoints on different cores; TSC verified synchronized (budget +/- tens of ns)"
                )?;
            } else {
                writeln!(
                    f,
                    "\u{2020} endpoints on different cores and TSC sync NOT verified -- discard these intervals"
                )?;
            }
        }
        writeln!(
            f,
            "resolution: one counter step = {} ticks ({:.1} ns); each interval is +/- one step",
            self.resolution_ticks, self.resolution_nanos
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MarksPool
// ---------------------------------------------------------------------------

struct PoolInner<L> {
    free: Mutex<Vec<Marks<L>>>,
    exhausted: AtomicU64,
    cal: Calibration,
}

/// Pool of preallocated timestamp buffers.
/// Checkout and return use a mutex-protected free list; exhaustion returns no sample and is counted.
/// The caller chooses when to sample and where pool operations sit relative to timed regions.
pub struct MarksPool<L> {
    inner: Arc<PoolInner<L>>,
}

impl<L> Clone for MarksPool<L> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<L: Copy> MarksPool<L> {
    /// Preallocate `pool_size` [`Marks`], each with room for
    /// `capacity_per_marks` timestamps (`mark()` calls plus `stop()`).
    pub fn new(cal: Calibration, pool_size: usize, capacity_per_marks: usize) -> Self {
        let free: Vec<Marks<L>> = (0..pool_size)
            .map(|_| Marks::new(cal, capacity_per_marks))
            .collect();
        Self {
            inner: Arc::new(PoolInner {
                free: Mutex::new(free),
                exhausted: AtomicU64::new(0),
                cal,
            }),
        }
    }

    /// Take a `Marks` from the pool, or `None` if all are in use (counted,
    /// never blocking, never allocating — that request goes unprofiled).
    ///
    /// The returned guard may hold a stale previous run; call
    /// [`Marks::start`] first, which clears it.
    pub fn checkout(&self) -> Option<PooledMarks<L>> {
        let m = self.inner.free.lock().ok()?.pop();
        match m {
            Some(marks) => Some(PooledMarks {
                marks: Some(marks),
                pool: Arc::clone(&self.inner),
            }),
            None => {
                self.inner.exhausted.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// How many checkouts have failed because the pool was empty. Nonzero
    /// means `pool_size` is smaller than your real in-flight peak.
    pub fn exhausted_checkouts(&self) -> u64 {
        self.inner.exhausted.load(Ordering::Relaxed)
    }

    /// How many `Marks` are currently free.
    pub fn available(&self) -> usize {
        self.inner.free.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// The calibration shared by every `Marks` in this pool.
    pub fn calibration(&self) -> Calibration {
        self.inner.cal
    }
}

/// A checked-out [`Marks`]; derefs to `Marks` and returns itself to the
/// pool when dropped. `Send` (for `L: Send`), so it can move between
/// threads with the request it profiles.
pub struct PooledMarks<L> {
    marks: Option<Marks<L>>,
    pool: Arc<PoolInner<L>>,
}

impl<L> Deref for PooledMarks<L> {
    type Target = Marks<L>;
    fn deref(&self) -> &Marks<L> {
        self.marks.as_ref().expect("PooledMarks used after drop")
    }
}

impl<L> DerefMut for PooledMarks<L> {
    fn deref_mut(&mut self) -> &mut Marks<L> {
        self.marks.as_mut().expect("PooledMarks used after drop")
    }
}

// Added for this repository: a `Marks` rides inside a struct that derives
// `Debug`. It prints its size rather than its contents — a hundred raw tick
// values in a log line is noise, and the run count is the part that says
// whether a trace is there at all.
impl<L> fmt::Debug for Marks<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Marks")
            .field("stamps", &self.stamps.len())
            .field("capacity", &self.stamps.capacity())
            .finish()
    }
}

impl<L> fmt::Debug for PooledMarks<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.marks {
            Some(m) => write!(f, "PooledMarks({m:?})"),
            None => f.write_str("PooledMarks(returned)"),
        }
    }
}

impl<L> Drop for PooledMarks<L> {
    fn drop(&mut self) {
        if let Some(m) = self.marks.take() {
            // The free list was built with `pool_size` capacity and only
            // `pool_size` Marks exist, so this push never reallocates. If
            // the lock is poisoned (a panic elsewhere), quietly let this
            // Marks go: profiling must never take down the pipeline.
            if let Ok(mut free) = self.pool.free.lock() {
                free.push(m);
            }
        }
    }
}
