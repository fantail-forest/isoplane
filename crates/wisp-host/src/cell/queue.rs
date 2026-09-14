//! The cell's work queue: two lanes per execution thread, wakes ahead of
//! dispatches, new work placed on the less loaded of two lanes drawn at
//! random.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use wisp_abi::dispatch::DispatchRecord;

use super::{DispatchResult, Target};
use crate::slot::Slot;
use crate::ModuleId;

pub(super) enum Job {
    Dispatch(Box<DispatchJob>),
    /// A host future completed for this instance; deliver it.
    Wake(WakeItem),
}

/// A queued wake.
pub(super) struct WakeItem {
    pub(super) slot: Arc<Slot>,
}

pub(crate) struct HttpDispatch {
    pub(crate) response: Arc<crate::response_transport::ResponseTransport>,
    pub(crate) upload: Option<Arc<crate::bindings::request_body::RequestBodyScope>>,
}

pub(super) struct DispatchJob {
    pub(super) module: ModuleId,
    pub(super) target: Target,
    pub(super) rec: DispatchRecord,
    pub(super) responder: std::sync::mpsc::SyncSender<DispatchResult>,
    pub(super) stream: Option<Arc<crate::response_transport::ResponseTransport>>,
    pub(super) upload: Option<Arc<crate::bindings::request_body::RequestBodyScope>>,
    pub(super) completion: Arc<super::DispatchCompletion>,
}

/// Two lanes per worker. Wakes jump ahead of new dispatches, and only a
/// dispatch is ever shed, so work already in flight keeps making progress
/// while a saturated cell refuses new work.
pub(super) struct WorkQueue {
    lanes: Box<[Lane]>,
    /// The dispatch depth of one lane.
    cap: usize,
}

/// `load` sits on its own cache line so that sampling two lanes' loads never
/// pulls in a line a third lane's push is writing.
#[repr(align(128))]
struct Lane {
    inner: Mutex<QueueInner>,
    cv: Condvar,
    /// Everything queued in this lane, of either kind, plus the job its
    /// worker has taken and not yet finished. Read without the lock by the
    /// placer, so a stale read costs a worse choice rather than a wrong one.
    load: AtomicUsize,
}

#[derive(Default)]
struct QueueInner {
    wakes: std::collections::VecDeque<WakeItem>,
    dispatches: std::collections::VecDeque<QueuedDispatch>,
    closed: bool,
}

struct QueuedDispatch {
    job: Box<DispatchJob>,
    _waiting: tracing::Span,
}

/// A per-thread xorshift. It runs on every dispatch, so it needs to be cheap
/// and unbiased rather than good.
fn lane_rand(n: usize) -> usize {
    use std::cell::Cell as StdCell;
    thread_local! {
        static STATE: StdCell<u64> = const { StdCell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            // The cell's address differs per thread, so no two workers walk
            // the same sequence.
            x = s as *const _ as u64 | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x % n as u64) as usize
    })
}

impl WorkQueue {
    pub(super) fn new(depth_per_lane: usize, workers: usize) -> Self {
        Self {
            lanes: (0..workers.max(1))
                .map(|_| Lane {
                    inner: Mutex::new(QueueInner::default()),
                    cv: Condvar::new(),
                    load: AtomicUsize::new(0),
                })
                .collect(),
            cap: depth_per_lane.max(1),
        }
    }

    /// Power of two choices: the less loaded of two lanes drawn at random.
    #[inline]
    fn pick(&self) -> usize {
        let n = self.lanes.len();
        if n <= 1 {
            return 0;
        }
        let a = lane_rand(n);
        let b = lane_rand(n);
        if self.lanes[a].load.load(Ordering::Relaxed) <= self.lanes[b].load.load(Ordering::Relaxed)
        {
            a
        } else {
            b
        }
    }

    /// Releases the load a push added, once the job has run rather than when
    /// it was taken, so a worker inside a long dispatch still reads as busy.
    #[inline]
    pub(super) fn job_done(&self, worker: usize) {
        let lane = &self.lanes[worker % self.lanes.len()];
        // Saturating, so a decrement without its push cannot wrap the lane
        // into looking infinitely loaded.
        let _ = lane
            .load
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// `Err` hands the job back, which is the shed.
    pub(super) fn push_dispatch(&self, j: Box<DispatchJob>) -> Result<(), Box<DispatchJob>> {
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if g.closed || g.dispatches.len() >= self.cap {
            return Err(j);
        }
        let waiting = j
            .completion
            .in_scope(|| tracing::info_span!("dispatch_queue"));
        g.dispatches.push_back(QueuedDispatch {
            job: j,
            _waiting: waiting,
        });
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        lane.cv.notify_one();
        Ok(())
    }

    /// A wake is never shed, so it is placed regardless of the lane's depth.
    pub(super) fn push_wake(&self, item: WakeItem) {
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if g.closed {
            return;
        }
        g.wakes.push_back(item);
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        lane.cv.notify_one();
    }

    /// The next job for `worker`, from its own lane only: a stealing scan
    /// would put every idle worker back on every other worker's lock. Wakes
    /// come out before dispatches. `None` once the queue is closed and the
    /// lane drained.
    pub(super) fn pop(&self, worker: usize) -> Option<Job> {
        let lane = &self.lanes[worker % self.lanes.len()];
        let mut g = lane.inner.lock();
        loop {
            if let Some(s) = g.wakes.pop_front() {
                return Some(Job::Wake(s));
            }
            if let Some(d) = g.dispatches.pop_front() {
                return Some(Job::Dispatch(d.job));
            }
            if g.closed {
                return None;
            }
            lane.cv.wait(&mut g);
        }
    }

    /// Stops admission. `closed` and the lane are written under one lock, so
    /// a dispatch admitted before the close is still popped and served, and
    /// one arriving after it is handed back as a shed.
    pub(super) fn close(&self) {
        for lane in self.lanes.iter() {
            lane.inner.lock().closed = true;
            lane.cv.notify_all();
        }
    }

    /// Called only after closure and worker joins; a cell without workers still owns its queue.
    pub(super) fn drain_stopped(&self) -> Vec<DispatchJob> {
        self.lanes
            .iter()
            .flat_map(|lane| {
                let mut queue = lane.inner.lock();
                queue.wakes.clear();
                lane.load.store(0, Ordering::Relaxed);
                queue
                    .dispatches
                    .drain(..)
                    .map(|queued| *queued.job)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn depth(&self) -> usize {
        self.lanes
            .iter()
            .map(|l| {
                let g = l.inner.lock();
                g.wakes.len() + g.dispatches.len()
            })
            .sum()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::bindings::InstanceCtx;
    use crate::pool::InstancePool;
    use std::time::{Duration, Instant};
    use wisp_abi::config::{HandlerLifecycle, Limits};

    /// The queue never looks inside a job, so the payload is minimal.
    fn job() -> Box<DispatchJob> {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        job_on(tx, 1)
    }

    fn job_on(
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
        dispatch_id: u64,
    ) -> Box<DispatchJob> {
        let rec = wisp_abi::dispatch::DispatchRecord {
            dispatch_id,
            kind: crate::cell::pool::cell_tests::http(dispatch_id).kind,
            trace: Default::default(),
            cancel_future: 0,
            cpu_budget_ms: 30_000,
            now_ms: 0,
        };
        let activator = Arc::new(crate::fakes::FakeActivator::new(
            crate::fakes::FakeBehaviour::done(),
        ));
        let entry = Arc::new(super::super::ModuleEntry {
            tenant: "queue-test".into(),
            debug_cpu_ms: None,
            limits: Limits::default(),
            manifest: wisp_abi::config::AppManifest {
                guest_memory_bytes: None,
                resource_limits: Default::default(),
                validation_limits: Default::default(),
                name: "queue-test".into(),
                handlers: wisp_abi::config::HandlerConfig {
                    lifecycle: HandlerLifecycle::Reuse,
                },
                bindings: Default::default(),
                limits: Limits::default(),
                init_visible_vars: vec![],
            },
            pool: InstancePool::new(activator.clone(), HandlerLifecycle::Reuse, 1),
            activator,
            meters: Arc::new(crate::meters::Meters::new()),
        });
        let completion = super::super::DispatchCompletion::new(
            entry,
            &rec,
            wisp_engine::CpuBudget::http(),
            Arc::new(crate::mono_ns),
        )
        .expect("queue fixture correlation token");
        Box::new(DispatchJob {
            stream: None,
            upload: None,
            module: ModuleId::new("t", "v1"),
            target: Target::Handler { route: "/".into() },
            rec,
            responder,
            completion,
        })
    }

    /// One slot is enough for any number of wakes; the queue only moves it.
    fn a_slot() -> Arc<Slot> {
        let pool = InstancePool::new(
            Arc::new(crate::fakes::FakeActivator::new(
                crate::fakes::FakeBehaviour::done(),
            )),
            HandlerLifecycle::Reuse,
            16,
        );
        let (slot, _) = pool
            .acquire(|id| {
                Arc::new(InstanceCtx::for_test(
                    id,
                    HandlerLifecycle::Reuse,
                    Limits::default(),
                ))
            })
            .expect("a slot");
        slot
    }

    fn wake_of(slot: &Arc<Slot>) -> WakeItem {
        WakeItem { slot: slot.clone() }
    }

    /// A lost wake-up is a thread that never returns, so every join here has
    /// a deadline that fails the test rather than hanging it.
    pub(super) fn join_before<T>(h: std::thread::JoinHandle<T>, within: Duration, what: &str) -> T {
        let deadline = Instant::now() + within;
        while !h.is_finished() {
            assert!(
                Instant::now() < deadline,
                "{what}: still running after {within:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        h.join().expect("thread panicked")
    }

    #[test]
    fn every_worker_has_a_lane_of_the_configured_depth() {
        let q = WorkQueue::new(128, 8);
        assert_eq!(q.lanes.len(), 8, "one lane per worker");
        assert_eq!(q.cap, 128, "a lane holds the configured depth, undivided");

        let q = WorkQueue::new(4, 64);
        assert_eq!(q.lanes.len(), 64, "one lane per worker");
        assert_eq!(
            q.cap, 4,
            "64 workers must not turn a depth of 4 into a depth of 1"
        );
    }

    #[test]
    fn a_lane_sheds_the_dispatch_past_its_depth() {
        let q = WorkQueue::new(4, 1);
        for i in 0..4 {
            assert!(
                q.push_dispatch(job()).is_ok(),
                "job {i} is inside the depth"
            );
        }
        assert!(
            q.push_dispatch(job()).is_err(),
            "the fifth dispatch on a lane of depth four must be handed back"
        );
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "draining makes room"
        );
        q.job_done(0);
        assert!(
            q.push_dispatch(job()).is_ok(),
            "the shed is a depth limit, not a one-way door"
        );
        q.close();
    }

    #[test]
    fn a_wake_is_never_shed() {
        let q = WorkQueue::new(1, 1);
        assert!(q.push_dispatch(job()).is_ok(), "the one slot");
        assert!(q.push_dispatch(job()).is_err(), "the lane is full");
        let slot = a_slot();
        q.push_wake(wake_of(&slot));
        assert!(
            matches!(q.pop(0), Some(Job::Wake(_))),
            "a wake pushed onto a full lane must still be queued, and ahead of the dispatch"
        );
        q.close();
    }

    /// Checked statistically: with one loaded lane and the rest idle, a
    /// placer that reads load lands on the loaded one only when both samples
    /// drew it, and a blind one lands there one time in N.
    #[test]
    fn power_of_two_avoids_the_loaded_lane() {
        let q = WorkQueue::new(4096, 8);
        q.lanes[3].load.store(1_000, Ordering::Relaxed);

        let mut hit_deep = 0;
        const N: usize = 20_000;
        for _ in 0..N {
            if q.pick() == 3 {
                hit_deep += 1;
            }
        }
        // Blind: about N/8. Both samples on lane 3: about N/64.
        assert!(
            hit_deep < N / 20,
            "the loaded lane was chosen {hit_deep} times in {N}; the placer is not reading load"
        );
        let seen: std::collections::HashSet<usize> = (0..2_000).map(|_| q.pick()).collect();
        assert!(
            seen.len() >= 6,
            "the sampler must reach most lanes, saw {}",
            seen.len()
        );
    }

    #[test]
    fn a_lane_keeps_counting_the_job_its_worker_is_still_running() {
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "one job");
        let load = || q.lanes[0].load.load(Ordering::Relaxed);
        assert_eq!(load(), 1, "a queued dispatch is load");

        let taken = q.pop(0).expect("the job");
        assert_eq!(
            load(),
            1,
            "the worker is inside the guest with this job; a placer that reads zero here sends \
             it another"
        );
        drop(taken);
        q.job_done(0);
        assert_eq!(
            load(),
            0,
            "the load is released when the job finishes, not when it is taken"
        );
    }

    #[test]
    fn queued_wakes_count_toward_the_lane_that_is_holding_them() {
        let q = WorkQueue::new(8, 4);
        let slot = a_slot();
        for _ in 0..400 {
            q.push_wake(wake_of(&slot));
        }
        let loads: Vec<usize> = q
            .lanes
            .iter()
            .map(|l| l.load.load(Ordering::Relaxed))
            .collect();
        assert_eq!(
            loads.iter().sum::<usize>(),
            400,
            "400 wakes are queued and the lanes report {loads:?}"
        );
    }

    #[test]
    fn a_wake_is_popped_before_a_dispatch() {
        let slot = a_slot();
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "the dispatch");
        q.push_wake(wake_of(&slot));
        assert!(
            matches!(q.pop(0), Some(Job::Wake(_))),
            "the wake queued second must come out first"
        );
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "and the dispatch after it"
        );
        q.close();
    }

    #[test]
    fn closing_the_queue_releases_parked_workers() {
        let q = Arc::new(WorkQueue::new(64, 4));
        let hs: Vec<_> = (0..4)
            .map(|w| {
                let q = q.clone();
                std::thread::spawn(move || q.pop(w))
            })
            .collect();
        // A worker with an empty lane is on its condition variable by the
        // time it can be observed holding nothing, so the close must reach it
        // there or the join below hangs.
        std::thread::sleep(Duration::from_millis(20));
        q.close();
        for (w, h) in hs.into_iter().enumerate() {
            assert!(
                join_before(
                    h,
                    Duration::from_secs(10),
                    &format!("worker {w} after close")
                )
                .is_none(),
                "a closed queue returns None"
            );
        }
    }

    #[test]
    fn a_dispatch_admitted_before_the_close_is_still_served() {
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "admitted");
        q.close();
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "the close must not strand a dispatch the caller was told was admitted"
        );
        assert!(q.pop(0).is_none(), "and then the lane reports closure");
        assert!(
            q.push_dispatch(job()).is_err(),
            "a dispatch after the close is handed back as a shed"
        );
    }
}
