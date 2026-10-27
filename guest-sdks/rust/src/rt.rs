//! **The isoplane scheduler**: a single-threaded, non-concurrent executor that
//! owns a continuation queue and a future table mapping host future ids to
//! waker slots.
//!
//! # How parking works, precisely
//!
//! This module is where the platform's central invariant is either true or a
//! slogan: *suspension implies an empty stack*. The only way for guest code to
//! wait is to return control to the host with a "pending" status.
//!
//! There is one code path and it is a chain of ordinary `return`s:
//!
//! ```text
//!  host calls isoplane_handle(ptr, len)
//!    -> rt::handle_dispatch  decodes the record, spawns ONE task, calls run()
//!      -> run()              pops the task, polls it exactly once
//!        -> app's async fn   awaits a host timer
//!          -> HostFuture::poll   calls the `clock_deadline_in` import, gets a future id,
//!                                records (id -> this task) in the future
//!                                table, and returns Poll::Pending
//!        <- Poll::Pending    every `.await` in the app's state machine
//!                            propagates Pending outward; no frame loops,
//!                            no frame waits
//!      <- run()              the ready queue is empty, so the loop EXITS
//!    <- handle_dispatch      returns PollStatus::Pending.encode()
//!  host sees `pending`; the wasm stack is now empty and the instance is
//!  Parked: evictable, snapshotable, relocatable.
//! ```
//!
//! Later, `isoplane_wake(future_id, result)` writes the result into the slot,
//! pushes the owning task back onto the ready queue and calls `run()` again --
//! a *fresh* stack, not a resumed one. That is the whole mechanism. There is
//! no Asyncify, no stack switching, no thread, and nothing that spins:
//! `run()` polls only tasks that something has explicitly made ready, and when
//! the ready queue drains it returns rather than looping.
//!
//! `debug_assert`s and the tests at the bottom of this file pin that down:
//! a dispatch that parks must cost **exactly one** poll.
//!
//! An instance can have several dispatches in progress. Every task belongs to
//! the dispatch it was spawned in, a slice polls only its own dispatch's
//! tasks, and the status it returns is that dispatch's.

use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use isoplane_abi::dispatch::DispatchRecord;
use isoplane_abi::result::{ResultRecord, WakeRecord};
use isoplane_abi::status::{PollStatus, TrapCode};

use std::alloc::{alloc as raw_alloc, dealloc as raw_dealloc};
use std::collections::{BTreeMap, VecDeque};

/// What a top-level task hands back when it finishes.
pub enum TaskOutcome {
    /// The dispatch is complete. Carries the response handle, or 0 for
    /// trigger types that have no response (an alarm does not).
    Response(u32),
    /// A concurrent task finished. Does not complete a dispatch.
    Done,
    /// Unrecoverable at the ABI layer.
    Trap(TrapCode),
}

pub type BoxTask = Pin<Box<dyn Future<Output = TaskOutcome> + 'static>>;

struct Task {
    fut: BoxTask,
    /// The dispatch the task belongs to; it runs only in that dispatch's
    /// slices.
    dispatch: u64,
}

/// One entry of the future table: a host future id, the task waiting on it,
/// and the result once `isoplane_wake` delivers one.
struct FutureSlot {
    owner: u32,
    result: Option<ResultRecord>,
}

pub(crate) struct Runtime {
    tasks: Vec<Option<Task>>,
    free_slots: Vec<u32>,
    /// Unfinished tasks per dispatch.
    live_tasks: BTreeMap<u64, usize>,
    ready: VecDeque<u32>,
    futures: BTreeMap<u64, FutureSlot>,
    current: u32,
    /// The dispatch the current slice runs.
    current_dispatch: u64,
    main_response: BTreeMap<u64, u32>,
    trap: Option<TrapCode>,
    /// Buffers handed to the host by `isoplane_alloc`, freed once the host has
    /// handed them back in a `isoplane_handle` / `isoplane_wake` call.
    host_bufs: Vec<(usize, usize, usize)>,
    /// Instrumentation. `polls` is the assertion that backs "parking is a
    /// return, not a spin".
    pub(crate) polls: u64,
    pub(crate) parks: u64,
}

impl Runtime {
    const fn new() -> Runtime {
        Runtime {
            tasks: Vec::new(),
            free_slots: Vec::new(),
            live_tasks: BTreeMap::new(),
            ready: VecDeque::new(),
            futures: BTreeMap::new(),
            current: u32::MAX,
            current_dispatch: 0,
            main_response: BTreeMap::new(),
            trap: None,
            host_bufs: Vec::new(),
            polls: 0,
            parks: 0,
        }
    }

    fn insert_task(&mut self, t: Task) -> u32 {
        *self.live_tasks.entry(t.dispatch).or_default() += 1;
        let id = match self.free_slots.pop() {
            Some(i) => {
                self.tasks[i as usize] = Some(t);
                i
            }
            None => {
                self.tasks.push(Some(t));
                (self.tasks.len() - 1) as u32
            }
        };
        self.ready.push_back(id);
        id
    }

    fn take_task(&mut self, id: u32) -> Option<Task> {
        self.tasks.get_mut(id as usize).and_then(|s| s.take())
    }

    fn put_task(&mut self, id: u32, t: Task) {
        self.tasks[id as usize] = Some(t);
    }

    /// Note: the caller drops the finished `Task` *outside* the runtime
    /// borrow, because dropping it drops its `HostFuture`s, whose `Drop`
    /// re-enters the runtime to deregister.
    fn finish_task(&mut self, id: u32, dispatch: u64, out: TaskOutcome) {
        self.free_slots.push(id);
        if let Some(live) = self.live_tasks.get_mut(&dispatch) {
            *live -= 1;
            if *live == 0 {
                self.live_tasks.remove(&dispatch);
            }
        }
        self.ready.retain(|&x| x != id);
        match out {
            TaskOutcome::Response(h) => {
                self.main_response.insert(dispatch, h);
            }
            TaskOutcome::Trap(c) => self.trap = Some(c),
            TaskOutcome::Done => {}
        }
    }

    /// The next ready task of `dispatch`, skipping stale entries; tasks of
    /// other dispatches stay queued for their own slices.
    fn next_ready(&mut self, dispatch: u64) -> Option<u32> {
        let position = self.ready.iter().position(|&id| {
            self.tasks
                .get(id as usize)
                .and_then(Option::as_ref)
                .is_none_or(|task| task.dispatch == dispatch)
        })?;
        self.ready.remove(position)
    }

    fn mark_ready(&mut self, id: u32) {
        if self.tasks.get(id as usize).is_some() && !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }

    /// The only place a `PollStatus` is decided, for the dispatch the slice
    /// ran. Note that there is no branch here that waits for anything.
    fn status(&mut self, dispatch: u64) -> PollStatus {
        if let Some(code) = self.trap.take() {
            return PollStatus::Trapped { code };
        }
        if let Some(r) = self.main_response.remove(&dispatch) {
            return PollStatus::Done { response: r };
        }
        if self.live_tasks.contains_key(&dispatch) {
            self.parks += 1;
            PollStatus::Pending
        } else {
            // All the dispatch's tasks have finished.
            PollStatus::Done { response: 0 }
        }
    }
}

// ---------------------------------------------------------------------------
// Single-threaded global access.
//
// The engine builds wasmtime without threads, shared memory or atomics and the
// validator rejects a module that declares them, so the module cannot express a
// second thread; and both `isoplane-host` and the engine refuse to start a
// slice in an instance while another is running. Together those make an
// `UnsafeCell` global the honest representation. The one hazard left is
// *self*-nesting, so `with` refuses it in debug builds.
// ---------------------------------------------------------------------------

struct Cell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Cell<T> {}

static RT: Cell<Runtime> = Cell(UnsafeCell::new(Runtime::new()));
static BUSY: Cell<bool> = Cell(UnsafeCell::new(false));

#[inline]
pub(crate) fn with<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    unsafe {
        let busy = &mut *BUSY.0.get();
        debug_assert!(
            !*busy,
            "re-entrant runtime access: never run guest code inside `with`"
        );
        *busy = true;
        let r = f(&mut *RT.0.get());
        *busy = false;
        r
    }
}

// ---------------------------------------------------------------------------
// Wakers
// ---------------------------------------------------------------------------

static VTABLE: RawWakerVTable = RawWakerVTable::new(w_clone, w_wake, w_wake_ref, w_drop);

unsafe fn w_clone(p: *const ()) -> RawWaker {
    RawWaker::new(p, &VTABLE)
}
unsafe fn w_wake(p: *const ()) {
    with(|rt| rt.mark_ready(p as usize as u32));
}
unsafe fn w_wake_ref(p: *const ()) {
    with(|rt| rt.mark_ready(p as usize as u32));
}
unsafe fn w_drop(_: *const ()) {}

fn waker_for(id: u32) -> Waker {
    // SAFETY: the vtable is trivially thread-safe because there is exactly one
    // thread; see the module note above.
    unsafe { Waker::from_raw(RawWaker::new(id as usize as *const (), &VTABLE)) }
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Drive every ready task to its next await point, then return.
///
/// The loop condition is "is anything ready", never "is anything finished".
/// When nothing is ready the function returns and the wasm stack unwinds to
/// nothing -- that return *is* the park.
pub(crate) fn run(dispatch: u64) -> PollStatus {
    with(|rt| rt.current_dispatch = dispatch);
    while let Some(id) = with(|rt| rt.next_ready(dispatch)) {
        // A stale ready entry (its task already finished) is a permitted
        // spurious wakeup, not an error.
        let Some(mut task) = with(|rt| rt.take_task(id)) else {
            continue;
        };
        with(|rt| {
            rt.current = id;
            rt.polls += 1;
        });
        let waker = waker_for(id);
        let mut cx = Context::from_waker(&waker);
        match task.fut.as_mut().poll(&mut cx) {
            Poll::Pending => with(move |rt| rt.put_task(id, task)),
            Poll::Ready(out) => {
                with(|rt| rt.finish_task(id, task.dispatch, out));
                drop(task); // outside `with`: HostFuture::drop re-enters
            }
        }
    }
    with(|rt| rt.status(dispatch))
}

/// Schedule concurrent work within the active dispatch.
/// Completing the response cancels host futures; callers must await any
/// required task results before completing.
pub fn spawn(fut: impl Future<Output = ()> + 'static) {
    let boxed: BoxTask = Box::pin(async move {
        fut.await;
        TaskOutcome::Done
    });
    with(|rt| {
        let dispatch = rt.current_dispatch;
        rt.insert_task(Task {
            fut: boxed,
            dispatch,
        })
    });
}

pub(crate) fn spawn_main(dispatch: u64, fut: BoxTask) {
    with(|rt| rt.insert_task(Task { fut, dispatch }));
}

// ---------------------------------------------------------------------------
// The future table
// ---------------------------------------------------------------------------

/// The host caps a dispatch's simultaneous pending futures at its record's
/// `pending_futures_limit` and refuses the one past it; this SDK keeps no
/// ceiling of its own.
pub(crate) fn register(id: u64) {
    with(|rt| {
        let owner = rt.current;
        rt.futures
            .entry(id)
            .or_insert(FutureSlot {
                owner,
                result: None,
            })
            .owner = owner;
    })
}

pub(crate) fn take_result(id: u64) -> Option<ResultRecord> {
    with(|rt| rt.futures.get_mut(&id).and_then(|s| s.result.take()))
}

/// Called from `HostFuture::drop`. A future the guest stopped caring about is
/// forgotten locally so a late `isoplane_wake` for it is a no-op.
/// This only removes the local await; it does not call `future_cancel` to
/// cancel the host operation.
pub(crate) fn forget(id: u64) {
    with(|rt| {
        rt.futures.remove(&id);
    });
}

pub(crate) fn deliver(id: u64, res: ResultRecord) {
    with(|rt| {
        if let Some(slot) = rt.futures.get_mut(&id) {
            slot.result = Some(res);
            let owner = slot.owner;
            rt.mark_ready(owner);
        }
        // Unknown id: the guest dropped the future. Dropping the result is
        // correct -- there is nobody left to observe it.
    });
}

/// Futures registered and not yet delivered. Bounded by the plan's
/// `pending_futures_per_dispatch`.
pub fn pending_futures() -> usize {
    with(|rt| rt.futures.len())
}

/// Test/telemetry hook: total `Future::poll` calls on top-level tasks.
pub fn poll_count() -> u64 {
    with(|rt| rt.polls)
}
/// Test/telemetry hook: how many times this instance has returned `pending`.
pub fn park_count() -> u64 {
    with(|rt| rt.parks)
}

// ---------------------------------------------------------------------------
// Export bodies. The `export_*!` macros emit thin `#[no_mangle]` wrappers in
// the guest crate that call straight into these.
// ---------------------------------------------------------------------------

/// `isoplane_alloc(size, align) -> ptr`: the host->guest buffer handoff. No host
/// pointer ever enters guest memory; the host copies into this buffer, and the
/// guest frees it when the host hands it back.
pub fn export_alloc(size: usize, align: usize) -> usize {
    let size = size.max(1);
    let align = align.max(1).next_power_of_two();
    let Ok(layout) = Layout::from_size_align(size, align) else {
        return 0;
    };
    let p = unsafe { raw_alloc(layout) };
    if p.is_null() {
        return 0;
    }
    with(|rt| rt.host_bufs.push((p as usize, size, align)));
    p as usize
}

fn free_host_buf(ptr: usize) {
    let found = with(|rt| {
        rt.host_bufs
            .iter()
            .position(|&(p, _, _)| p == ptr)
            .map(|i| rt.host_bufs.swap_remove(i))
    });
    if let Some((p, size, align)) = found {
        unsafe { raw_dealloc(p as *mut u8, Layout::from_size_align_unchecked(size, align)) };
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn outstanding_handoff_buffers() -> usize {
    with(|rt| rt.host_bufs.len())
}

/// `isoplane_handle(ptr, len) -> poll-status`.
pub fn export_handle(ptr: usize, len: usize, make: fn(DispatchRecord) -> BoxTask) -> u64 {
    #[cfg(target_arch = "wasm32")]
    crate::install_panic_hook();
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    let rec: DispatchRecord = match postcard::from_bytes(bytes) {
        Ok(r) => r,
        // Malformed dispatch encoding is an ABI break, not an app error.
        Err(_) => {
            return PollStatus::Trapped {
                code: TrapCode::Protocol,
            }
            .encode()
        }
    };
    free_host_buf(ptr);
    let dispatch = rec.dispatch_id;
    spawn_main(dispatch, make(rec));
    run(dispatch).encode()
}

/// `isoplane_wake(future, ptr, len) -> poll-status`, for the dispatch the
/// wake record names.
pub fn export_wake(future: u64, ptr: usize, len: usize) -> u64 {
    #[cfg(target_arch = "wasm32")]
    crate::install_panic_hook();
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    let wake: WakeRecord = match postcard::from_bytes(bytes) {
        Ok(r) => r,
        Err(_) => {
            return PollStatus::Trapped {
                code: TrapCode::Protocol,
            }
            .encode()
        }
    };
    free_host_buf(ptr);
    deliver(future, wake.result);
    run(wake.dispatch).encode()
}

/// Test support (non-wasm builds only): drop every task and future.
#[cfg(not(target_arch = "wasm32"))]
pub fn reset() {
    // Move the old runtime OUT before dropping it: dropping a `Task` drops its
    // `HostFuture`s, whose `Drop` re-enters the runtime to deregister. That
    // rule holds in production too -- see `run`.
    let old = with(|rt| core::mem::replace(rt, Runtime::new()));
    drop(old);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::future::HostFuture;
    use crate::testing;

    /// **The park test.** A dispatch that awaits a host future must cost
    /// exactly one poll and then RETURN. If parking were implemented by
    /// waiting, this would either never return or the poll count would climb.
    #[test]
    fn parking_is_one_poll_and_a_return() {
        testing::with_stub_host(|host| {
            reset();
            let before = with(|rt| rt.polls);
            let id = host.next_future_id();
            spawn_main(
                1,
                Box::pin(async move {
                    let f: HostFuture<()> = crate::future::new_unit(id);
                    let _ = f.await;
                    TaskOutcome::Response(0)
                }),
            );
            let st = run(1);
            assert_eq!(st, PollStatus::Pending, "an awaited host future must park");
            assert_eq!(with(|rt| rt.polls) - before, 1, "parking must not re-poll");
            assert_eq!(pending_futures(), 1);
        });
    }

    /// The other half: a wake is a *new* slice, and it completes
    /// the dispatch on a fresh stack.
    #[test]
    fn a_wake_resumes_and_completes() {
        testing::with_stub_host(|host| {
            reset();
            let id = host.next_future_id();
            spawn_main(
                1,
                Box::pin(async move {
                    let f: HostFuture<()> = crate::future::new_unit(id);
                    f.await.unwrap();
                    TaskOutcome::Response(7)
                }),
            );
            assert_eq!(run(1), PollStatus::Pending);
            deliver(id, ResultRecord::Ok(Vec::new()));
            assert_eq!(run(1), PollStatus::Done { response: 7 });
            assert_eq!(pending_futures(), 0, "the future table must not leak");
        });
    }

    /// Two concurrent host futures awaited together, ZERO threads. Both
    /// register on the first poll; the task parks once and is re-polled once
    /// per wake.
    #[test]
    fn two_concurrent_futures_park_once_and_join() {
        testing::with_stub_host(|host| {
            reset();
            let (a, b) = (host.next_future_id(), host.next_future_id());
            spawn_main(
                1,
                Box::pin(async move {
                    let fa: HostFuture<()> = crate::future::new_unit(a);
                    let fb: HostFuture<()> = crate::future::new_unit(b);
                    let (ra, rb) = crate::future::join2(fa, fb).await;
                    ra.unwrap();
                    rb.unwrap();
                    TaskOutcome::Response(1)
                }),
            );
            assert_eq!(run(1), PollStatus::Pending);
            assert_eq!(
                pending_futures(),
                2,
                "both futures must be in flight at once"
            );
            let polls_after_park = with(|rt| rt.polls);
            assert_eq!(polls_after_park, 1);

            deliver(a, ResultRecord::Ok(Vec::new()));
            assert_eq!(run(1), PollStatus::Pending, "one of two is not done");
            deliver(b, ResultRecord::Ok(Vec::new()));
            assert_eq!(run(1), PollStatus::Done { response: 1 });
            assert_eq!(with(|rt| rt.polls), 3, "one poll per wake, no spinning");
        });
    }

    /// A future dropped without completing must not leave a slot behind, and a
    /// late wake for it must be inert rather than a trap.
    #[test]
    fn dropping_a_future_deregisters_it() {
        testing::with_stub_host(|host| {
            reset();
            let id = host.next_future_id();
            spawn_main(
                1,
                Box::pin(async move {
                    {
                        let f: HostFuture<()> = crate::future::new_unit(id);
                        let mut f = Box::pin(f);
                        let w = waker_for(0);
                        let mut cx = Context::from_waker(&w);
                        assert!(f.as_mut().poll(&mut cx).is_pending());
                    } // dropped here
                    TaskOutcome::Response(0)
                }),
            );
            assert_eq!(run(1), PollStatus::Done { response: 0 });
            assert_eq!(pending_futures(), 0);
            deliver(id, ResultRecord::Ok(Vec::new()));
            assert_eq!(run(1), PollStatus::Done { response: 0 });
        });
    }

    /// Two dispatches in progress at once: each slice runs and reports only
    /// its own dispatch, so the second can finish while the first waits.
    #[test]
    fn dispatches_in_progress_finish_independently() {
        testing::with_stub_host(|host| {
            reset();
            let (a, b) = (host.next_future_id(), host.next_future_id());
            let waiting_on = |id| -> BoxTask {
                Box::pin(async move {
                    let f: HostFuture<()> = crate::future::new_unit(id);
                    f.await.unwrap();
                    TaskOutcome::Response(id as u32)
                })
            };
            spawn_main(1, waiting_on(a));
            assert_eq!(run(1), PollStatus::Pending, "the first dispatch parks");
            spawn_main(2, waiting_on(b));
            assert_eq!(run(2), PollStatus::Pending, "the second dispatch parks");

            deliver(b, ResultRecord::Ok(Vec::new()));
            assert_eq!(
                run(2),
                PollStatus::Done { response: b as u32 },
                "the second dispatch finishes while the first still waits"
            );
            deliver(a, ResultRecord::Ok(Vec::new()));
            assert_eq!(run(1), PollStatus::Done { response: a as u32 });
            assert_eq!(pending_futures(), 0, "the future table must not leak");
        });
    }

    #[test]
    fn a_malformed_dispatch_traps_with_protocol() {
        testing::with_stub_host(|_| {
            reset();
            let junk = [0xFFu8; 8];
            let st = PollStatus::decode(export_handle(junk.as_ptr() as usize, 8, |_| {
                Box::pin(async { TaskOutcome::Response(0) })
            }));
            assert_eq!(
                st,
                PollStatus::Trapped {
                    code: TrapCode::Protocol
                }
            );
        });
    }
}
