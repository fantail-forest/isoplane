//! Cell metrics (spec.md §10).
//!
//! > **Metrics (per app, exported + dashboard):** dispatches, CPU-ms, p50/p99
//! > activation & wake, live/parked/hibernated instance counts, dirty-set size
//! > distribution, delta storage bytes, GC time share, trap rate.
//!
//! The point of §10 is stated a line earlier and is worth keeping in view:
//! *"the lifecycle is visible, so 'why was this request 12 ms' decomposes into
//! '9 ms hibernation wake' rather than mystery."* So the histograms here are
//! per lifecycle transition, not one blended request timer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// A bounded-sample latency distribution. Exact percentiles over the retained
/// window rather than approximations over all time — the window is what an
/// operator is looking at anyway.
#[derive(Debug)]
pub struct Histogram {
    samples: Mutex<Vec<u64>>,
    cap: usize,
    count: AtomicU64,
    sum: AtomicU64,
    /// 1 = keep every sample (the default), N = keep one in N, 0 = keep none.
    ///
    /// The sample window is behind a `Mutex`, and a histogram on the
    /// per-dispatch path is therefore a per-cell singleton lock taken by every
    /// execution thread on every dispatch. At the densities the campaign runs
    /// that is not free, and "the cell's knee is lock contention" is a claim
    /// the telemetry itself can manufacture. This knob makes the claim
    /// testable by difference: run the same point with the two §10 per-dispatch
    /// histograms unsampled and see whether the knee moves.
    ///
    /// `count` and `sum` are updated regardless of the setting, so turning
    /// sampling down never turns a metric into a zero — it costs percentiles,
    /// not the rate or the mean. See [`Metrics::set_per_dispatch_sampling`].
    sample_every: AtomicU64,
}

impl Histogram {
    pub fn new(cap: usize) -> Self {
        Self {
            samples: Mutex::new(Vec::with_capacity(cap)),
            cap,
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            sample_every: AtomicU64::new(1),
        }
    }
    /// Keep one sample in `n`. `0` disables the sample window entirely, which
    /// is the configuration that removes the lock from the dispatch path;
    /// `count()`, `mean()` and the exported rate keep working.
    pub fn set_sample_every(&self, n: u64) {
        self.sample_every.store(n, Ordering::Relaxed);
    }
    pub fn sample_every(&self) -> u64 {
        self.sample_every.load(Ordering::Relaxed)
    }
    pub fn record(&self, v: u64) {
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        self.sum.fetch_add(v, Ordering::Relaxed);
        // Decided *before* the lock: at `sample_every == 0` this method takes
        // no lock at all, which is the whole point of the knob.
        // `%` rather than `is_multiple_of`: the workspace declares
        // `rust-version = "1.85"` and that method landed in 1.87. Clippy
        // suggests it because this crate does not inherit the workspace MSRV.
        #[allow(clippy::manual_is_multiple_of)]
        let keep = match self.sample_every.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            k => n % k == 0,
        };
        if !keep {
            return;
        }
        let mut s = self.samples.lock();
        if s.len() < self.cap {
            s.push(v);
        } else {
            // Reservoir-ish: overwrite a rotating slot so the window slides
            // instead of freezing at startup.
            let i = (self.count.load(Ordering::Relaxed) as usize) % self.cap;
            s[i] = v;
        }
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
    /// How many samples the percentile window is holding. Distinct from
    /// [`Histogram::count`], which counts every observation whether or not
    /// sampling retained it — and the difference between the two is exactly
    /// what [`Histogram::set_sample_every`] trades away.
    pub fn retained(&self) -> usize {
        self.samples.lock().len()
    }
    pub fn percentile(&self, p: f64) -> u64 {
        let mut s = self.samples.lock().clone();
        if s.is_empty() {
            return 0;
        }
        s.sort_unstable();
        // Nearest-rank, lower-inclusive: p50 of 1..=100 is 50, not 51. Using
        // `round` here would make the median of an even sample the upper of
        // the two middles, which reads as a bug every time someone checks it
        // by hand.
        let idx = ((s.len() as f64 - 1.0) * p).floor() as usize;
        s[idx.min(s.len() - 1)]
    }
    pub fn p50(&self) -> u64 {
        self.percentile(0.50)
    }
    pub fn p99(&self) -> u64 {
        self.percentile(0.99)
    }
    pub fn mean(&self) -> f64 {
        let c = self.count();
        if c == 0 {
            0.0
        } else {
            self.sum.load(Ordering::Relaxed) as f64 / c as f64
        }
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new(4096)
    }
}

/// Everything §10 asks a cell to expose.
#[derive(Debug, Default)]
pub struct Metrics {
    pub dispatches: AtomicU64,
    pub wakes: AtomicU64,
    pub traps: AtomicU64,
    /// Every CPU-budget kill, clean and hard. Kept as the total so a dashboard
    /// built against v1.1 still reads something true.
    pub budget_kills: AtomicU64,

    // -- §6.4's two-phase kill, and the SLI it produces --------------------
    /// Dispatches that entered grace and **returned inside it**: `finally`
    /// ran, the instance stayed consistent, the slot was reused.
    pub clean_kills: AtomicU64,
    /// Dispatches still executing at **B+G**. Terminated without unwinding,
    /// instance tombstoned. These are the ones the hang breaker counts.
    pub hard_kills: AtomicU64,
    /// Dispatches that crossed B at all — clean, hard, or still unwinding.
    /// `grace_entries - clean_kills - hard_kills` is the number that parked
    /// mid-unwind and are being driven to completion by their own poisoned
    /// futures.
    pub grace_entries: AtomicU64,
    /// §4.6: epoch ticks that found the runtime layer's *unsafe-to-trap* bit
    /// set and deferred grace entry. A guest pinning the bit shows up here and
    /// still dies at B+G, which is the property worth being able to see.
    pub grace_deferrals: AtomicU64,
    /// Instances §6.4 required the platform to tombstone. Equal to
    /// `hard_kills + traps` in a correct implementation; counted separately
    /// because "the platform must enforce it" is exactly the claim, and a
    /// divergence between these two is the bug Appendix C F2 describes.
    ///
    /// The invariant is only checkable because `traps` and `hard_kills` are
    /// **disjoint**: a hard budget kill is not a trap. They shared an
    /// increment once, which made `traps ⊇ hard_kills`, the identity above
    /// false by construction, and the blast table's adjacent `hard kills` /
    /// `tombstones` rows double-count. See [`Metrics::tombstone_invariant`],
    /// which is the assertion this sentence promises.
    pub tombstones: AtomicU64,
    /// Host calls refused because the target was a tombstone. §10's negative
    /// control for the rule: a green dashboard with this permanently at zero
    /// and no failing probe is unfalsifiable.
    ///
    /// Moved by [`crate::cell::Cell::with_instance`] and by the two
    /// `Slot::enter` refusals inside the cell's dispatch and wake lanes. It
    /// counts *refusals*, not tombstones: a cell that killed a thousand
    /// instances and was never asked to call one again reads zero here and is
    /// healthy.
    pub tombstone_refusals: AtomicU64,
    /// CPU recorded **past B** on kills of either phase (§6.4: "the overage CPU
    /// is recorded"). Already inside the tenant's `cpu_ns`; broken out so the
    /// share is visible.
    pub overage_ns: AtomicU64,

    pub shed: AtomicU64,
    pub breaker_rejections: AtomicU64,
    /// §5.4's rule biting. Mostly benign: two wake jobs for one instance, the
    /// second finding the first still inside the guest and leaving the result
    /// queued for it to drain. A non-zero value is the reentrancy guard
    /// working, not a fault — the fault it prevents would not be counted here,
    /// it would be unrepresentable.
    pub reentrancy_refusals: AtomicU64,
    /// §7.2's single-writer rule refusing a second concurrent dispatch to one
    /// Durable Instance — the 409 raised *before* the guest is entered,
    /// because the identity already has a request in flight.
    ///
    /// Distinct from [`Metrics::reentrancy_refusals`], which is §5.4's rule
    /// biting at the slot door once a dispatch has already been admitted. Both
    /// answer with 409 and neither reaches guest code, and telling them apart
    /// is the difference between "the caller is racing itself" and "a wake and
    /// a dispatch collided inside the cell".
    ///
    /// These 409s return before [`Metrics::dispatches`] is incremented, so
    /// they are **not** in the dispatch count. A generator's submitted count
    /// therefore exceeds the platform's `dispatches` by exactly
    /// `shed + breaker_rejections + single_writer_refusals`, and that identity
    /// is what makes a counting error on either side visible.
    pub single_writer_refusals: AtomicU64,
    pub pending_cap_rejections: AtomicU64,
    /// §8.6 refusals plus §11.1 subrequest-cap refusals — every outbound call
    /// the host declined to make on the tenant's behalf.
    pub egress_denials: AtomicU64,

    pub live: AtomicU64,
    pub parked: AtomicU64,
    pub hibernated: AtomicU64,
    pub hot: AtomicU64,

    /// Captures whose delta **reached the store**. A capture whose store
    /// write failed is not one of these — see
    /// [`Metrics::capture_write_failures`].
    pub captures: AtomicU64,
    /// Delta bytes actually written. Same rule as `captures`: bytes that never
    /// landed are not counted, because `delta_bytes` is what §11.2 prices.
    pub delta_bytes: AtomicU64,
    /// Hibernation captures whose store write failed. The instance is left
    /// Parked and capturable rather than marked Hibernated, so nothing later
    /// tries to wake from a key that was never stored.
    ///
    /// Its own counter rather than a silent `.ok()`: the hibernation arm's
    /// headline is `captures/instance/s`, and a spool that is full would
    /// otherwise publish a capture rate made entirely of captures that did not
    /// happen. This is the mirror of the zero-captures bug already on record —
    /// that one under-counted a real capture and had a visible symptom; this
    /// one over-counted an absent one and had none.
    pub capture_write_failures: AtomicU64,
    pub hibernation_wakes: AtomicU64,
    pub delta_fallbacks: AtomicU64,
    /// Futures shed at **capture time** (§6.8): Unbounded host futures that
    /// could not be carried across a hibernation, plus §8.6's sensitive ones.
    ///
    /// **This is not a load signal.** It moves when hibernation *works*, on an
    /// instance that was idle enough to be captured, and on the hibernation
    /// arm the first capture that sheds an unbounded future makes it non-zero
    /// for the rest of the run. The load-path questions have their own
    /// counters and a bottleneck attributor wants those instead:
    ///
    /// * "the cell refused to register more futures" — §11.1's
    ///   simultaneous-pending cap — is [`Metrics::pending_cap_rejections`];
    /// * "the cell refused work at admission" is [`Metrics::shed`].
    ///
    /// Equal to [`Metrics::futures_shed_capture`] by construction today; kept
    /// as the aggregate name so an existing dashboard keeps reading something
    /// true, and asserted equal so the day a load-path shed is added is the
    /// day a test says so.
    pub futures_shed: AtomicU64,
    /// The capture-time half of [`Metrics::futures_shed`], under the name that
    /// says which half it is.
    pub futures_shed_capture: AtomicU64,
    pub futures_buffered: AtomicU64,

    /// §6.5's target latencies. Microseconds.
    pub activation_us: Histogram,
    pub wake_us: Histogram,
    pub hibernation_wake_us: Histogram,
    pub capture_us: Histogram,
    /// End-to-end request latency, admission to final response — the thing
    /// §10 wants to decompose into "9 ms hibernation wake" rather than leave
    /// as mystery.
    pub request_us: Histogram,
    pub park_ms: Histogram,
    /// §6.6 telemetry: "your instance dirties 60 MB".
    pub dirty_pages: Histogram,
    /// §10 GC time share — the `wisp-gc-hint(snapshot)` call before capture.
    pub gc_hint_us: Histogram,

    /// §9.2 `tier=fast` visibility: how many dispatches were served without
    /// paying for an activation.
    ///
    /// Counted for **both** instance kinds. A Durable Instance acquisition is
    /// a lookup by identity rather than a pool draw, and for a long stretch
    /// only the Handler arm incremented these — which made all three read zero
    /// for any workload made of DIs, and a zero here is indistinguishable from
    /// "never incremented", "no dispatches" and "every dispatch paid for an
    /// activation". `activation_us` was recorded on the durable cold path the
    /// whole time, so the latency existed and the count did not.
    ///
    /// The four are exhaustive over admitted dispatches:
    /// `served_warm + served_pooled_fresh + served_hibernation_wake +
    /// served_activated == dispatches`.
    pub served_warm: AtomicU64,
    /// Handler-only: a reset instance drawn from the pool (§6.3).
    pub served_pooled_fresh: AtomicU64,
    /// Durable-only: the identity was Hibernated and the dispatch paid for a
    /// wake from its delta. Not "warm" (it cost a wake) and not "activated"
    /// (it did not cost a cold activation), which is exactly why §9.2 wants it
    /// separable rather than folded into either.
    pub served_hibernation_wake: AtomicU64,
    pub served_activated: AtomicU64,

    by_tenant: Mutex<BTreeMap<String, TenantCounters>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TenantCounters {
    pub dispatches: u64,
    pub cpu_ns: u64,
    pub traps: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_tenant(&self, tenant: &str, cpu_ns: u64, trapped: bool) {
        self.record_tenant_marked(tenant, cpu_ns, trapped, &mut None)
    }

    /// [`Metrics::record_tenant`], marking the wait for `by_tenant` separately
    /// from the work done under it.
    ///
    /// **A triple.** The hold allocates a `String` from the tenant name and
    /// then walks a `BTreeMap` — 26.6 ns at eight tenants, and it grows with
    /// the tenant count, so on a run with many it is the hold rather than the
    /// wait that moves. `by_tenant` is not a `CountedMutex` and appears in no
    /// row of `Cell::lock_stats`, so nothing else measures it at all.
    pub fn record_tenant_marked(
        &self,
        tenant: &str,
        cpu_ns: u64,
        trapped: bool,
        marks: &mut Option<crate::marks::RequestMarks>,
    ) {
        let mut g = self.by_tenant.lock();
        crate::marks::mark(marks, crate::marks::Mark::TenantRecordAcquired);
        let e = g.entry(tenant.to_string()).or_default();
        e.dispatches += 1;
        e.cpu_ns += cpu_ns;
        if trapped {
            e.traps += 1;
        }
    }

    pub fn tenant(&self, tenant: &str) -> TenantCounters {
        self.by_tenant.lock().get(tenant).copied().unwrap_or_default()
    }

    /// §6.4's tombstone identity, as an assertion rather than a comment.
    ///
    /// `tombstones == hard_kills + traps`. It holds only because the two
    /// terms on the right are disjoint — a hard budget kill is not a trap —
    /// which is a property of exactly one increment in `Cell::settle` and
    /// therefore worth a check that runs.
    pub fn tombstone_invariant(&self) -> Result<(), String> {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let (t, h, tr) = (l(&self.tombstones), l(&self.hard_kills), l(&self.traps));
        if t == h + tr {
            Ok(())
        } else {
            Err(format!(
                "tombstones ({t}) != hard_kills ({h}) + traps ({tr}); if traps overshoots by \
                 exactly hard_kills the two are sharing an increment again (Appendix C F2)"
            ))
        }
    }

    /// §10/H7: how densely the two **per-dispatch** histograms sample.
    ///
    /// `request_us` and `park_ms` are recorded on every completed dispatch and
    /// each takes a per-cell mutex shared by every execution thread. At ~126k
    /// dispatches/second that is two of the eight or so lock acquisitions a
    /// dispatch pays, and "the cell's knee is lock contention" is a claim the
    /// telemetry can manufacture for itself. Setting this to `0` removes both
    /// locks from the dispatch path — the counts, rates and means survive,
    /// only the percentiles stop — so the claim can be tested by difference
    /// instead of assumed.
    ///
    /// Deliberately not a removal and not a compile-time feature: §10 wants
    /// these metrics, and a run without them has to be a *run*, comparable
    /// against one with them, not a different binary.
    pub fn set_per_dispatch_sampling(&self, n: u64) {
        self.request_us.set_sample_every(n);
        self.park_ms.set_sample_every(n);
    }

    /// What [`Metrics::set_per_dispatch_sampling`] is currently set to.
    pub fn per_dispatch_sampling(&self) -> u64 {
        self.request_us.sample_every()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            dispatches: l(&self.dispatches),
            wakes: l(&self.wakes),
            traps: l(&self.traps),
            budget_kills: l(&self.budget_kills),
            clean_kills: l(&self.clean_kills),
            hard_kills: l(&self.hard_kills),
            grace_entries: l(&self.grace_entries),
            grace_deferrals: l(&self.grace_deferrals),
            tombstones: l(&self.tombstones),
            tombstone_refusals: l(&self.tombstone_refusals),
            overage_ns: l(&self.overage_ns),
            // §6.4: **the clean:hard ratio is a first-class SLI** — "a healthy
            // fleet runs in the thousands". It is a metric and not a log line
            // precisely because it is a ratio: no single kill event tells you
            // anything, and the number only means something aggregated and
            // watched over time. A fleet whose ratio collapses has guests that
            // stopped cooperating, or a runtime layer that stopped raising
            // `BudgetExceededException` at its natural boundaries — neither is
            // visible in any per-request signal.
            //
            // Zero hard kills is not "infinite", it is "no evidence yet": the
            // ratio is reported as the clean count in that case, which is the
            // most it can honestly claim.
            clean_hard_ratio: {
                let (c, h) = (l(&self.clean_kills), l(&self.hard_kills));
                if h == 0 { c as f64 } else { c as f64 / h as f64 }
            },
            shed: l(&self.shed),
            breaker_rejections: l(&self.breaker_rejections),
            reentrancy_refusals: l(&self.reentrancy_refusals),
            single_writer_refusals: l(&self.single_writer_refusals),
            pending_cap_rejections: l(&self.pending_cap_rejections),
            egress_denials: l(&self.egress_denials),
            live: l(&self.live),
            parked: l(&self.parked),
            hibernated: l(&self.hibernated),
            hot: l(&self.hot),
            captures: l(&self.captures),
            delta_bytes: l(&self.delta_bytes),
            capture_write_failures: l(&self.capture_write_failures),
            hibernation_wakes: l(&self.hibernation_wakes),
            delta_fallbacks: l(&self.delta_fallbacks),
            futures_shed: l(&self.futures_shed),
            futures_shed_capture: l(&self.futures_shed_capture),
            futures_buffered: l(&self.futures_buffered),
            served_warm: l(&self.served_warm),
            served_pooled_fresh: l(&self.served_pooled_fresh),
            served_hibernation_wake: l(&self.served_hibernation_wake),
            served_activated: l(&self.served_activated),
            activation_p50_us: self.activation_us.p50(),
            activation_p99_us: self.activation_us.p99(),
            wake_p50_us: self.wake_us.p50(),
            wake_p99_us: self.wake_us.p99(),
            hibernation_wake_p50_us: self.hibernation_wake_us.p50(),
            hibernation_wake_p99_us: self.hibernation_wake_us.p99(),
            capture_p50_us: self.capture_us.p50(),
            request_p50_us: self.request_us.p50(),
            request_p99_us: self.request_us.p99(),
            gc_hint_p50_us: self.gc_hint_us.p50(),
            dirty_pages_p50: self.dirty_pages.p50(),
            dirty_pages_p99: self.dirty_pages.p99(),
            park_p50_ms: self.park_ms.p50(),
            // §11.2/H8. A process-wide floor, not a per-cell number: the
            // clock read is a vDSO call with no cell to attribute it to. Zero
            // is the answer that says "every recorded interval was measured";
            // anything else says some CPU was spent and not recorded, which is
            // the flattering direction (Appendix C, F4) and must not be
            // indistinguishable from a genuinely idle meter.
            //
            // Both meters, summed, because a reader of this number wants one
            // question answered — "is any recorded interval a floor rather than
            // a measurement?" — and the answer is no only if *both* are zero.
            // `wisp-engine`'s clock is the one the recorded `cpu_ns` comes from;
            // this crate's is the one `CpuInterval` and the log-CPU share use.
            clock_read_failures: crate::meters::clock_read_failures()
                + wisp_engine::clock_read_failures(),
            trap_rate: {
                let d = l(&self.dispatches);
                if d == 0 { 0.0 } else { l(&self.traps) as f64 / d as f64 }
            },
            tenants: self.by_tenant.lock().clone(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot().as_json()).unwrap_or_else(|_| "{}".into())
    }

    /// Prometheus-ish text exposition, so the harness can scrape without a
    /// client library.
    pub fn to_text(&self) -> String {
        let s = self.snapshot();
        let mut out = String::new();
        for (k, v) in s.as_pairs() {
            out.push_str(&format!("wisp_cell_{k} {v}\n"));
        }
        for (tenant, c) in &s.tenants {
            out.push_str(&format!("wisp_tenant_dispatches{{tenant=\"{tenant}\"}} {}\n", c.dispatches));
            out.push_str(&format!(
                "wisp_tenant_cpu_ms{{tenant=\"{tenant}\"}} {:.3}\n",
                c.cpu_ns as f64 / 1e6
            ));
            out.push_str(&format!("wisp_tenant_traps{{tenant=\"{tenant}\"}} {}\n", c.traps));
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub dispatches: u64,
    pub wakes: u64,
    pub traps: u64,
    pub budget_kills: u64,
    pub clean_kills: u64,
    pub hard_kills: u64,
    pub grace_entries: u64,
    pub grace_deferrals: u64,
    pub tombstones: u64,
    pub tombstone_refusals: u64,
    pub overage_ns: u64,
    /// §6.4's first-class SLI. See the note in [`Metrics::snapshot`].
    pub clean_hard_ratio: f64,
    pub shed: u64,
    pub breaker_rejections: u64,
    pub reentrancy_refusals: u64,
    pub single_writer_refusals: u64,
    pub pending_cap_rejections: u64,
    pub egress_denials: u64,
    pub live: u64,
    pub parked: u64,
    pub hibernated: u64,
    pub hot: u64,
    pub captures: u64,
    pub delta_bytes: u64,
    pub capture_write_failures: u64,
    pub hibernation_wakes: u64,
    pub delta_fallbacks: u64,
    pub futures_shed: u64,
    pub futures_shed_capture: u64,
    pub futures_buffered: u64,
    pub served_warm: u64,
    pub served_pooled_fresh: u64,
    pub served_hibernation_wake: u64,
    pub served_activated: u64,
    pub activation_p50_us: u64,
    pub activation_p99_us: u64,
    pub wake_p50_us: u64,
    pub wake_p99_us: u64,
    pub hibernation_wake_p50_us: u64,
    pub hibernation_wake_p99_us: u64,
    pub capture_p50_us: u64,
    pub request_p50_us: u64,
    pub request_p99_us: u64,
    pub gc_hint_p50_us: u64,
    pub dirty_pages_p50: u64,
    pub dirty_pages_p99: u64,
    pub park_p50_ms: u64,
    /// §11.2/H8: thread- or process-CPU clock reads that failed, process-wide.
    /// See [`crate::meters::clock_read_failures`].
    pub clock_read_failures: u64,
    pub trap_rate: f64,
    pub tenants: BTreeMap<String, TenantCounters>,
}

impl MetricsSnapshot {
    pub fn as_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("dispatches", self.dispatches.to_string()),
            ("wakes", self.wakes.to_string()),
            ("traps", self.traps.to_string()),
            ("budget_kills", self.budget_kills.to_string()),
            ("clean_kills", self.clean_kills.to_string()),
            ("hard_kills", self.hard_kills.to_string()),
            ("grace_entries", self.grace_entries.to_string()),
            ("grace_deferrals", self.grace_deferrals.to_string()),
            ("tombstones", self.tombstones.to_string()),
            ("tombstone_refusals", self.tombstone_refusals.to_string()),
            ("overage_ms", format!("{:.3}", self.overage_ns as f64 / 1e6)),
            ("clean_hard_ratio", format!("{:.6}", self.clean_hard_ratio)),
            ("shed", self.shed.to_string()),
            ("breaker_rejections", self.breaker_rejections.to_string()),
            ("reentrancy_refusals", self.reentrancy_refusals.to_string()),
            ("single_writer_refusals", self.single_writer_refusals.to_string()),
            ("pending_cap_rejections", self.pending_cap_rejections.to_string()),
            ("egress_denials", self.egress_denials.to_string()),
            ("instances_live", self.live.to_string()),
            ("instances_parked", self.parked.to_string()),
            ("instances_hibernated", self.hibernated.to_string()),
            ("instances_hot", self.hot.to_string()),
            ("captures", self.captures.to_string()),
            ("delta_bytes", self.delta_bytes.to_string()),
            ("capture_write_failures", self.capture_write_failures.to_string()),
            ("hibernation_wakes", self.hibernation_wakes.to_string()),
            ("delta_fallbacks", self.delta_fallbacks.to_string()),
            ("futures_shed", self.futures_shed.to_string()),
            ("futures_shed_capture", self.futures_shed_capture.to_string()),
            ("futures_buffered", self.futures_buffered.to_string()),
            ("served_warm", self.served_warm.to_string()),
            ("served_pooled_fresh", self.served_pooled_fresh.to_string()),
            ("served_hibernation_wake", self.served_hibernation_wake.to_string()),
            ("served_activated", self.served_activated.to_string()),
            ("activation_p50_us", self.activation_p50_us.to_string()),
            ("activation_p99_us", self.activation_p99_us.to_string()),
            ("wake_p50_us", self.wake_p50_us.to_string()),
            ("wake_p99_us", self.wake_p99_us.to_string()),
            ("hibernation_wake_p50_us", self.hibernation_wake_p50_us.to_string()),
            ("hibernation_wake_p99_us", self.hibernation_wake_p99_us.to_string()),
            ("capture_p50_us", self.capture_p50_us.to_string()),
            ("request_p50_us", self.request_p50_us.to_string()),
            ("request_p99_us", self.request_p99_us.to_string()),
            ("gc_hint_p50_us", self.gc_hint_p50_us.to_string()),
            ("dirty_pages_p50", self.dirty_pages_p50.to_string()),
            ("dirty_pages_p99", self.dirty_pages_p99.to_string()),
            ("park_p50_ms", self.park_p50_ms.to_string()),
            ("clock_read_failures", self.clock_read_failures.to_string()),
            ("trap_rate", format!("{:.6}", self.trap_rate)),
        ]
    }

    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (k, v) in self.as_pairs() {
            let val: serde_json::Value = v
                .parse::<u64>()
                .map(Into::into)
                .or_else(|_| v.parse::<f64>().map(Into::into))
                .unwrap_or(serde_json::Value::String(v));
            m.insert(k.to_string(), val);
        }
        let tenants: serde_json::Map<String, serde_json::Value> = self
            .tenants
            .iter()
            .map(|(t, c)| {
                (
                    t.clone(),
                    serde_json::json!({
                        "dispatches": c.dispatches,
                        "cpu_ms": c.cpu_ns as f64 / 1e6,
                        "traps": c.traps,
                    }),
                )
            })
            .collect();
        m.insert("tenants".into(), serde_json::Value::Object(tenants));
        serde_json::Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_exact_over_the_window() {
        let h = Histogram::new(1000);
        for i in 1..=100u64 {
            h.record(i);
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.p50(), 50);
        assert_eq!(h.p99(), 99);
        assert!((h.mean() - 50.5).abs() < 0.001);
    }

    #[test]
    fn an_empty_histogram_reads_zero_rather_than_panicking() {
        let h = Histogram::default();
        assert_eq!(h.p50(), 0);
        assert_eq!(h.p99(), 0);
        assert_eq!(h.mean(), 0.0);
    }

    /// §6.4: *"the clean:hard ratio is a first-class SLI — a healthy fleet
    /// runs in the thousands"*. It has to be a number on the scrape, not a
    /// sentence in a log.
    #[test]
    fn the_clean_hard_ratio_is_a_scraped_metric() {
        let m = Metrics::new();
        for _ in 0..3_000 {
            m.clean_kills.fetch_add(1, Ordering::Relaxed);
        }
        m.hard_kills.fetch_add(1, Ordering::Relaxed);
        let s = m.snapshot();
        assert_eq!(s.clean_hard_ratio, 3_000.0, "a healthy fleet runs in the thousands");
        assert!(m.to_text().contains("wisp_cell_clean_hard_ratio 3000.000000"));
        let json: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(json["clean_kills"], 3_000);
        assert_eq!(json["hard_kills"], 1);
    }

    /// **Negative control** (§10). The ratio must be able to say "unhealthy".
    /// A denominator guard that silently reported a large number whenever
    /// hard kills were rare would make the SLI unfalsifiable — which is the
    /// exact failure mode §10's negative-control rule was written for
    /// (Appendix C, F4: measurement bugs run in the flattering direction).
    #[test]
    fn the_clean_hard_ratio_can_report_an_unhealthy_fleet() {
        let m = Metrics::new();
        m.clean_kills.fetch_add(1, Ordering::Relaxed);
        for _ in 0..1_000 {
            m.hard_kills.fetch_add(1, Ordering::Relaxed);
        }
        assert!(
            m.snapshot().clean_hard_ratio < 0.01,
            "a fleet that is all hard kills must read as such, not as healthy"
        );
        // And with no kills at all it must not invent a healthy-looking number.
        assert_eq!(Metrics::new().snapshot().clean_hard_ratio, 0.0);
    }

    /// **§6.4's tombstone identity, as a check that can fail.**
    ///
    /// The doc on [`Metrics::tombstones`] promised `tombstones ==
    /// hard_kills + traps`, and for a long time that was false by
    /// construction: `traps` was incremented for hard budget kills as well as
    /// for traps, so a *correct* implementation gave `tombstones == traps` and
    /// the documented sum overshot by exactly `hard_kills`. The blast table
    /// prints `hard kills`, `clean kills` and `tombstones` as adjacent rows,
    /// so a reader summing down the column double-counted every hard kill.
    ///
    /// This asserts both directions: the invariant holds when the two are
    /// disjoint, and it *fails* under the shared increment — a check that
    /// cannot fail is not a check.
    #[test]
    fn the_tombstone_invariant_holds_and_can_fail() {
        let m = Metrics::new();
        // Four hard kills and three traps: seven tombstoned instances.
        for _ in 0..4 {
            m.hard_kills.fetch_add(1, Ordering::Relaxed);
            m.tombstones.fetch_add(1, Ordering::Relaxed);
        }
        for _ in 0..3 {
            m.traps.fetch_add(1, Ordering::Relaxed);
            m.tombstones.fetch_add(1, Ordering::Relaxed);
        }
        m.tombstone_invariant().expect("disjoint counters must satisfy the identity");
        let s = m.snapshot();
        assert_eq!(s.tombstones, 7);
        assert_eq!(s.tombstones, s.hard_kills + s.traps);

        // Negative control: the bug this identity exists to catch. Counting
        // each hard kill as a trap as well leaves `tombstones` correct and the
        // sum inflated by exactly `hard_kills`, which is the signature.
        let bug = Metrics::new();
        for _ in 0..4 {
            bug.hard_kills.fetch_add(1, Ordering::Relaxed);
            bug.traps.fetch_add(1, Ordering::Relaxed); // the shared increment
            bug.tombstones.fetch_add(1, Ordering::Relaxed);
        }
        let e = bug.tombstone_invariant().expect_err("the shared increment must be caught");
        assert!(e.contains("tombstones (4)"), "{e}");
        assert!(e.contains("hard_kills (4)"), "{e}");
        assert!(e.contains("traps (4)"), "{e}");
    }

    /// **§10/H7: sampling costs percentiles and nothing else.**
    ///
    /// `request_us` and `park_ms` are recorded on every completed dispatch,
    /// and each `record` takes a per-cell mutex shared by every execution
    /// thread. Making them optional is what lets "the cell's knee is lock
    /// contention" be tested by difference instead of assumed — but only if
    /// switching them off leaves a *comparable run* rather than a hole in the
    /// scrape. So: count, sum and mean survive at any setting; only the
    /// retained window changes.
    #[test]
    fn sampling_gives_up_percentiles_and_keeps_the_rate() {
        let every = Histogram::new(1000);
        let one_in_four = Histogram::new(1000);
        one_in_four.set_sample_every(4);
        let off = Histogram::new(1000);
        off.set_sample_every(0);

        for i in 1..=100u64 {
            every.record(i);
            one_in_four.record(i);
            off.record(i);
        }

        for h in [&every, &one_in_four, &off] {
            assert_eq!(h.count(), 100, "the observation count is not a sampled quantity");
            assert!((h.mean() - 50.5).abs() < 0.001, "nor is the mean");
        }
        assert_eq!(every.retained(), 100);
        assert_eq!(one_in_four.retained(), 25);
        assert_eq!(off.retained(), 0, "at 0 the record path takes no lock and keeps nothing");

        assert_eq!(every.p50(), 50);
        // A quarter of the samples still describes the distribution.
        assert!((one_in_four.p50() as i64 - 50).abs() <= 8, "p50 was {}", one_in_four.p50());
        assert_eq!(off.p50(), 0, "no window, no percentile — and 0 is what an empty window reads");
    }

    /// §10's list, present and scrapeable. The harness reads this endpoint, so
    /// a rename is a breaking change to it.
    #[test]
    fn the_scrape_carries_every_metric_section_10_names() {
        let m = Metrics::new();
        m.dispatches.fetch_add(7, Ordering::Relaxed);
        m.traps.fetch_add(1, Ordering::Relaxed);
        m.activation_us.record(900);
        m.wake_us.record(40);
        m.dirty_pages.record(512);
        m.delta_bytes.fetch_add(4096, Ordering::Relaxed);
        m.record_tenant("acme", 2_000_000, false);

        let text = m.to_text();
        for needle in [
            "wisp_cell_dispatches 7",
            "wisp_cell_activation_p50_us 900",
            "wisp_cell_wake_p50_us 40",
            "wisp_cell_dirty_pages_p50 512",
            "wisp_cell_delta_bytes 4096",
            "wisp_cell_instances_parked",
            "wisp_cell_instances_hibernated",
            "wisp_cell_trap_rate",
            "wisp_tenant_cpu_ms{tenant=\"acme\"} 2.000",
        ] {
            assert!(text.contains(needle), "scrape is missing `{needle}`:\n{text}");
        }

        let json: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(json["dispatches"], 7);
        assert_eq!(json["tenants"]["acme"]["dispatches"], 1);
        assert!((json["trap_rate"].as_f64().unwrap() - 1.0 / 7.0).abs() < 1e-6);
    }
}
