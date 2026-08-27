//! Timestamp interfaces without timing samples on unsupported architectures.
//! The pool remains empty and calibration reports no usable timestamp counter.
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// See [`crate::tsc_marks::Calibration`]. Reports nothing measurable.
#[derive(Clone, Copy, Debug, Default)]
pub struct Calibration;

impl Calibration {
    pub fn measure() -> Self {
        Self
    }
    pub fn measure_for(_window: Duration) -> Self {
        Self
    }
    /// Zero: this implementation does not measure a timestamp-counter rate.
    pub fn tsc_hz(&self) -> f64 {
        0.0
    }
    pub fn mark_overhead_ticks(&self) -> u64 {
        0
    }
    /// One tick, because there is no counter whose step could be measured.
    pub fn resolution_ticks(&self) -> u64 {
        1
    }
    pub fn resolution_nanos(&self) -> f64 {
        0.0
    }
    pub fn has_invariant_tsc(&self) -> bool {
        false
    }
    pub fn cross_core_ok(&self) -> bool {
        false
    }
    pub fn ticks_to_nanos(&self, _ticks: u64) -> f64 {
        0.0
    }
}

/// See [`crate::tsc_marks::Interval`].
#[derive(Clone, Copy, Debug)]
pub struct Interval<L> {
    pub label: L,
    pub ticks_raw: u64,
    pub ticks: u64,
    pub nanos: f64,
    pub at_floor: bool,
    pub cross_core: bool,
    pub cores: (u32, u32),
}

/// See [`crate::tsc_marks::Report`]. Always empty.
#[derive(Clone, Debug)]
pub struct Report<L> {
    pub intervals: Vec<Interval<L>>,
    pub total_ticks: u64,
    pub total_nanos: f64,
    pub crossed_cores: bool,
    pub cross_core_ok: bool,
}

/// See [`crate::tsc_marks::Marks`]. Records nothing.
pub struct Marks<L> {
    cal: Calibration,
    _label: std::marker::PhantomData<L>,
}

impl<L> fmt::Debug for Marks<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Marks(unsupported architecture)")
    }
}

impl<L: Copy> Marks<L> {
    pub fn new(cal: Calibration, _capacity: usize) -> Self {
        Self { cal, _label: std::marker::PhantomData }
    }
    #[inline(always)]
    pub fn start(&mut self) {}
    #[inline(always)]
    pub fn mark(&mut self, _label: L) {}
    #[inline(always)]
    pub fn stop(&mut self, _label: L) {}
    pub fn report(&self) -> Report<L> {
        Report {
            intervals: Vec::new(),
            total_ticks: 0,
            total_nanos: 0.0,
            crossed_cores: false,
            cross_core_ok: false,
        }
    }
    pub fn calibration(&self) -> &Calibration {
        &self.cal
    }
    pub fn len(&self) -> usize {
        0
    }
    pub fn is_empty(&self) -> bool {
        true
    }
    pub fn capacity(&self) -> usize {
        0
    }
    pub fn span_ticks(&self) -> u64 {
        0
    }
    pub fn span_nanos(&self) -> f64 {
        0.0
    }
}

struct PoolInner {
    exhausted: AtomicU64,
    cal: Calibration,
}

/// See [`crate::tsc_marks::MarksPool`]. Permanently empty, so every request
/// goes untraced and says so through the counter.
pub struct MarksPool<L> {
    inner: Arc<PoolInner>,
    _label: std::marker::PhantomData<L>,
}

impl<L> Clone for MarksPool<L> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner), _label: std::marker::PhantomData }
    }
}

impl<L: Copy> MarksPool<L> {
    pub fn new(cal: Calibration, _pool_size: usize, _capacity_per_marks: usize) -> Self {
        Self { inner: Arc::new(PoolInner { exhausted: AtomicU64::new(0), cal }), _label: std::marker::PhantomData }
    }
    /// Always `None`, and counted — a run on this architecture reports that
    /// every request went untraced rather than reporting traces of zero.
    pub fn checkout(&self) -> Option<PooledMarks<L>> {
        self.inner.exhausted.fetch_add(1, Ordering::Relaxed);
        None
    }
    pub fn exhausted_checkouts(&self) -> u64 {
        self.inner.exhausted.load(Ordering::Relaxed)
    }
    pub fn available(&self) -> usize {
        0
    }
    pub fn calibration(&self) -> Calibration {
        self.inner.cal
    }
}

/// See [`crate::tsc_marks::PooledMarks`]. Never constructed.
pub struct PooledMarks<L> {
    marks: Marks<L>,
}

impl<L> fmt::Debug for PooledMarks<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PooledMarks(unsupported architecture)")
    }
}

impl<L> Deref for PooledMarks<L> {
    type Target = Marks<L>;
    fn deref(&self) -> &Marks<L> {
        &self.marks
    }
}

impl<L> DerefMut for PooledMarks<L> {
    fn deref_mut(&mut self) -> &mut Marks<L> {
        &mut self.marks
    }
}
