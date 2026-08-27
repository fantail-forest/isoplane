//! Shared timestamp storage for work that crosses request handoffs.
//! Writers reserve slots atomically. Collection sorts completed samples by timestamp
//! and distinguishes claimed slots that have not been filled.
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::tsc_marks::Calibration;

/// Packs a label and processor tag into one word.
/// The lower sixteen bits hold a nonzero label tag; the upper sixteen retain
/// the low sixteen bits of the processor tag. Zero denotes an unfilled slot.
#[inline(always)]
fn pack(label: u8, core: u32) -> u32 {
    // +1 so that no valid entry packs to zero.
    (label as u32 + 1) | (core << 16)
}

#[inline(always)]
fn unpack(v: u32) -> Option<(u8, u32)> {
    if v == 0 {
        return None;
    }
    Some((((v & 0xffff) - 1) as u8, v >> 16))
}

/// The storage one request's trace lives in.
struct Inner {
    cal: Calibration,
    /// The origin: the caller releasing the request. Not a slot, because
    /// nothing ends at it.
    start: AtomicU64,
    start_core: AtomicU32,
    /// Next free slot. May run past `ticks.len()`, which is how truncation is
    /// detected without a branch that could fail differently per thread.
    cursor: AtomicUsize,
    ticks: Box<[AtomicU64]>,
    /// `pack(label, core)`, or zero for a slot that has been claimed and not
    /// yet written.
    tags: Box<[AtomicU32]>,
}

impl Inner {
    fn new(cal: Calibration, capacity: usize) -> Self {
        Self {
            cal,
            start: AtomicU64::new(0),
            start_core: AtomicU32::new(0),
            cursor: AtomicUsize::new(0),
            ticks: (0..capacity).map(|_| AtomicU64::new(0)).collect(),
            tags: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    /// Read the counter exactly as `Marks::mark` does.
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    #[allow(unsafe_code)]
    fn read_counter() -> (u64, u32) {
        let mut aux = 0u32;
        // SAFETY: `__rdtscp` is an unprivileged instruction present on every
        // x86-64 part, and `aux` is a live `u32` we own for the duration of
        // the call. This is the same sequence `tsc_marks::Marks::mark` issues:
        // same intrinsic, same serialising read, same core id out of the same
        // register.
        let t = unsafe { core::arch::x86_64::__rdtscp(&mut aux) };
        (t, aux)
    }

    /// There is no portable counter with this resolution, so on other
    /// architectures every timestamp is zero and the pool that would have fed
    /// this is empty anyway — see `tsc_marks_stub`. Present so that the cell,
    /// which is the runtime data plane, still builds.
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    fn read_counter() -> (u64, u32) {
        (0, 0)
    }

    /// Claim a slot and write a timestamp into it.
    ///
    /// The counter is read **before** the slot is claimed, so the atomic add —
    /// which is contended only when two threads mark the same request at the
    /// same instant, i.e. exactly at a handoff — is not inside the interval
    /// being measured.
    #[inline(always)]
    fn mark(&self, label: u8) {
        let (t, core) = Self::read_counter();
        let i = self.cursor.fetch_add(1, Ordering::Relaxed);
        if i >= self.ticks.len() {
            return;
        }
        self.ticks[i].store(t, Ordering::Relaxed);
        // `Release` so a reader that sees a non-zero tag also sees the tick
        // that was stored before it. The tag is what says "this slot is real".
        self.tags[i].store(pack(label, core), Ordering::Release);
    }

    fn begin(&self) {
        self.cursor.store(0, Ordering::Relaxed);
        for t in self.tags.iter() {
            t.store(0, Ordering::Relaxed);
        }
        let (t, core) = Self::read_counter();
        self.start.store(t, Ordering::Relaxed);
        self.start_core.store(core, Ordering::Relaxed);
    }

    fn writers_outstanding(&self) -> bool {
        let claimed = self.cursor.load(Ordering::Relaxed).min(self.ticks.len());
        (0..claimed).any(|i| self.tags[i].load(Ordering::Acquire) == 0)
    }
}

/// The request's own handle: the one that travels with it and returns the
/// buffer to the pool.
///
/// `None` from [`TracePool::checkout`] means the pool was dry and this request
/// simply goes untraced.
pub struct Trace {
    inner: Option<Arc<Inner>>,
    pool: Arc<PoolInner>,
}

impl std::fmt::Debug for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Some(i) => write!(f, "Trace({} marks)", i.cursor.load(Ordering::Relaxed)),
            None => f.write_str("Trace(returned)"),
        }
    }
}

impl Trace {
    /// Begin the request at [`crate::marks::Mark::CallerReleased`].
    #[inline(always)]
    pub fn begin(&self) {
        if let Some(i) = &self.inner {
            i.begin();
        }
    }

    /// Record a timestamp.
    #[inline(always)]
    pub fn mark(&self, label: u8) {
        if let Some(i) = &self.inner {
            i.mark(label);
        }
    }

    /// Retains a write handle across a request handoff.
    #[inline(always)]
    pub fn writer(&self) -> Writer {
        Writer(self.inner.clone())
    }

    /// The calibration these timestamps were taken against.
    pub fn calibration(&self) -> Option<Calibration> {
        self.inner.as_ref().map(|i| i.cal)
    }
}

/// Write handle for a request shared timestamp buffer.
pub struct Writer(Option<Arc<Inner>>);

impl Writer {
    #[inline(always)]
    pub fn mark(&self, label: u8) {
        if let Some(i) = &self.0 {
            i.mark(label);
        }
    }
}

/// Maximum spin attempts before abandoning a trace with outstanding writers.
/// An abandoned buffer is not returned to the free list and increments the abandoned count.
const RETURN_SPIN: usize = 4096;

impl Drop for Trace {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else { return };
        // Two conditions before this buffer can be reused: no other handle
        // exists, and every claimed slot has been written. The second is not
        // implied by the first — a writer stores its tag and then drops, so
        // there is an instant where the count is 1 and a slot is still blank.
        for _ in 0..RETURN_SPIN {
            if Arc::strong_count(&inner) == 1 && !inner.writers_outstanding() {
                if let Ok(mut free) = self.pool.free.lock() {
                    free.push(inner);
                }
                return;
            }
            std::hint::spin_loop();
        }
        self.pool.abandoned.fetch_add(1, Ordering::Relaxed);
    }
}

struct PoolInner {
    free: Mutex<Vec<Arc<Inner>>>,
    exhausted: AtomicU64,
    abandoned: AtomicU64,
    cal: Calibration,
}

/// A pool of preallocated shared trace buffers.
/// Checkout uses the mutex-protected free list; exhaustion yields no sample and is counted.
pub struct TracePool {
    inner: Arc<PoolInner>,
    capacity_per_trace: usize,
}

impl Clone for TracePool {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            capacity_per_trace: self.capacity_per_trace,
        }
    }
}

impl TracePool {
    pub fn new(cal: Calibration, pool_size: usize, capacity_per_trace: usize) -> Self {
        let free: Vec<Arc<Inner>> =
            (0..pool_size).map(|_| Arc::new(Inner::new(cal, capacity_per_trace))).collect();
        Self {
            inner: Arc::new(PoolInner {
                free: Mutex::new(free),
                exhausted: AtomicU64::new(0),
                abandoned: AtomicU64::new(0),
                cal,
            }),
            capacity_per_trace,
        }
    }

    /// Take a trace, or `None` if all are in use.
    pub fn checkout(&self) -> Option<Trace> {
        let got = self.inner.free.lock().ok().and_then(|mut f| f.pop());
        match got {
            Some(inner) => Some(Trace { inner: Some(inner), pool: Arc::clone(&self.inner) }),
            None => {
                self.inner.exhausted.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// How many checkouts found the pool empty. Non-zero means the pool is
    /// smaller than the real in-flight peak — and that the sample thinned out
    /// exactly when the cell was busiest, which biases every percentile in the
    /// flattering direction.
    pub fn exhausted_checkouts(&self) -> u64 {
        self.inner.exhausted.load(Ordering::Relaxed)
    }

    /// How many buffers were given up on because a writer had not finished.
    /// Each one permanently shrinks the pool, so a rising count and a rising
    /// `exhausted_checkouts` are the same fault seen twice.
    pub fn abandoned(&self) -> u64 {
        self.inner.abandoned.load(Ordering::Relaxed)
    }

    pub fn available(&self) -> usize {
        self.inner.free.lock().map(|f| f.len()).unwrap_or(0)
    }

    pub fn calibration(&self) -> Calibration {
        self.inner.cal
    }

    pub fn capacity_per_trace(&self) -> usize {
        self.capacity_per_trace
    }
}

/// One finished trace, in time order.
#[derive(Debug, Clone)]
pub struct Spans {
    /// `(label, nanoseconds)`, sorted by when the timestamp was taken.
    pub spans: Vec<(u8, f64)>,
    /// **Last timestamp minus the first, raw.** Not the sum of `spans`: each
    /// span has one mark's calibrated cost subtracted, so the sum is short by
    /// about that much per span. This is the figure that has to reconcile
    /// against a measurement taken from outside.
    pub span_ns: f64,
    /// True if any span's ends were read on different cores.
    pub crossed_cores: bool,
    /// The buffer filled and timestamps past the end were dropped.
    pub truncated: bool,
    /// Slots claimed but never written — a writer still in flight when the
    /// trace was read. Distinct from truncation and counted separately.
    pub raced: usize,
}

/// Read a finished trace.
///
/// Called once per traced request, on the thread that finished it, after the
/// last timestamp. Sorting is what makes the append-from-anywhere design
/// legible: slots are claimed in whatever order threads reached the cursor,
/// and time order is the only order that means anything.
pub fn read(t: &Trace) -> Option<Spans> {
    let inner = t.inner.as_ref()?;
    let cal = inner.cal;
    let claimed = inner.cursor.load(Ordering::Acquire);
    let truncated = claimed > inner.ticks.len();
    let n = claimed.min(inner.ticks.len());

    let mut entries: Vec<(u64, u8, u32)> = Vec::with_capacity(n);
    let mut raced = 0usize;
    for i in 0..n {
        match unpack(inner.tags[i].load(Ordering::Acquire)) {
            Some((label, core)) => {
                entries.push((inner.ticks[i].load(Ordering::Relaxed), label, core))
            }
            None => raced += 1,
        }
    }
    entries.sort_unstable_by_key(|(t, _, _)| *t);

    let overhead = cal.mark_overhead_ticks();
    let mut prev_t = inner.start.load(Ordering::Relaxed);
    let mut prev_core = inner.start_core.load(Ordering::Relaxed);
    let mut spans = Vec::with_capacity(entries.len());
    let mut crossed = false;
    for (t, label, core) in &entries {
        let raw = t.wrapping_sub(prev_t);
        crossed |= *core != prev_core;
        spans.push((*label, cal.ticks_to_nanos(raw.saturating_sub(overhead))));
        prev_t = *t;
        prev_core = *core;
    }
    let span_ns = match entries.last() {
        Some((last, _, _)) => {
            cal.ticks_to_nanos(last.wrapping_sub(inner.start.load(Ordering::Relaxed)))
        }
        None => 0.0,
    };
    Some(Spans { spans, span_ns, crossed_cores: crossed, truncated, raced })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn cal() -> Calibration {
        use std::sync::OnceLock;
        static C: OnceLock<Calibration> = OnceLock::new();
        *C.get_or_init(|| Calibration::measure_for(std::time::Duration::from_millis(50)))
    }

    fn spin(n: u64) -> u64 {
        let mut x = 0u64;
        for i in 0..n {
            x = x.wrapping_add(i).wrapping_mul(2_654_435_761);
        }
        x
    }

    /// **The point of the whole design: two threads, one trace, no lock.**
    ///
    /// The producer records after it has given the request away, while the
    /// consumer is already recording. With a single-owner buffer this is not
    /// expressible; here both land and time order sorts them out.
    #[test]
    fn two_threads_can_mark_the_same_request_at_the_same_time() {
        let pool = TracePool::new(cal(), 4, 64);
        let t = pool.checkout().expect("a trace");
        t.begin();

        let w = t.writer();
        let go = Arc::new(AtomicBool::new(false));
        let g2 = go.clone();
        let h = std::thread::spawn(move || {
            while !g2.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            for _ in 0..16 {
                w.mark(1);
            }
        });
        go.store(true, Ordering::Release);
        for _ in 0..16 {
            t.mark(2);
        }
        h.join().unwrap();

        let s = read(&t).expect("spans");
        assert_eq!(s.raced, 0, "every claimed slot was written");
        assert!(!s.truncated);
        assert_eq!(s.spans.len(), 32, "both threads' marks are present");
        assert_eq!(s.spans.iter().filter(|(l, _)| *l == 1).count(), 16);
        assert_eq!(s.spans.iter().filter(|(l, _)| *l == 2).count(), 16);
        // Sorted into time order, so every span is a real duration.
        assert!(s.spans.iter().all(|(_, ns)| *ns >= 0.0));
    }

    /// The coverage property, and that the total is not the sum of the parts.
    #[test]
    fn the_total_is_last_minus_first_and_not_the_sum() {
        let pool = TracePool::new(cal(), 2, 64);
        let t = pool.checkout().unwrap();
        t.begin();
        for _ in 0..8 {
            std::hint::black_box(spin(2_000));
            t.mark(3);
        }
        let s = read(&t).unwrap();
        let summed: f64 = s.spans.iter().map(|(_, ns)| ns).sum();
        assert!(s.span_ns > 0.0);
        assert!(
            s.span_ns > summed,
            "raw span {:.0} ns must exceed the overhead-corrected sum {summed:.0} ns",
            s.span_ns
        );
    }

    /// A trace that runs out of slots stops rather than growing, and says so.
    #[test]
    fn a_full_trace_truncates_and_reports_it() {
        let pool = TracePool::new(cal(), 1, 8);
        let t = pool.checkout().unwrap();
        t.begin();
        for _ in 0..64 {
            t.mark(4);
        }
        let s = read(&t).unwrap();
        assert!(s.truncated, "64 marks into 8 slots is a truncation");
        assert_eq!(s.spans.len(), 8, "and it kept exactly the slots it had");
    }

    /// **The buffer comes back, including after a second writer existed.**
    ///
    /// A pool that leaks is not a crash: it is a run whose first few thousand
    /// requests are traced and whose remaining millions are not.
    #[test]
    fn the_buffer_returns_to_the_pool_even_after_a_handoff() {
        let pool = TracePool::new(cal(), 2, 16);
        assert_eq!(pool.available(), 2);
        {
            let t = pool.checkout().unwrap();
            t.begin();
            let w = t.writer();
            w.mark(5);
            drop(w);
            t.mark(6);
            assert_eq!(pool.available(), 1, "it is checked out");
        }
        assert_eq!(pool.available(), 2, "and back");
        assert_eq!(pool.abandoned(), 0);
        assert_eq!(pool.exhausted_checkouts(), 0);
    }

    /// A dry pool hands back nothing and counts it, rather than blocking a
    /// dispatch thread or allocating on the request path.
    #[test]
    fn a_dry_pool_is_counted_and_never_blocks() {
        let pool = TracePool::new(cal(), 1, 8);
        let held = pool.checkout().expect("the one");
        assert!(pool.checkout().is_none());
        assert_eq!(pool.exhausted_checkouts(), 1);
        drop(held);
        assert!(pool.checkout().is_some());
    }

    /// Zero is reserved for "claimed but not written", so no real label can
    /// pack to it — otherwise a slot in flight would read as a valid mark with
    /// whatever tick happened to be in the array.
    #[test]
    fn no_label_packs_to_the_unwritten_sentinel() {
        for label in 0..=255u8 {
            for core in [0u32, 1, 255, 511] {
                let v = pack(label, core);
                assert_ne!(v, 0, "label {label} on core {core} packed to the sentinel");
                assert_eq!(unpack(v), Some((label, core)));
            }
        }
        assert_eq!(unpack(0), None);
    }

    /// A reused buffer does not carry its previous run's marks. `begin` clears
    /// the tags, so a short trace after a long one cannot inherit a tail.
    #[test]
    fn a_reused_buffer_starts_empty() {
        let pool = TracePool::new(cal(), 1, 32);
        {
            let t = pool.checkout().unwrap();
            t.begin();
            for _ in 0..20 {
                t.mark(7);
            }
            assert_eq!(read(&t).unwrap().spans.len(), 20);
        }
        let t = pool.checkout().unwrap();
        t.begin();
        t.mark(8);
        let s = read(&t).unwrap();
        assert_eq!(s.spans.len(), 1, "the previous run's twenty marks are gone");
        assert_eq!(s.spans[0].0, 8);
    }
}
