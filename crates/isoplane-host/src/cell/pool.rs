//! The execution and idle-sweep thread pools, and panic containment at their boundary.

use super::*;

thread_local! {
    pub(super) static IN_FLIGHT: std::cell::RefCell<Option<Weak<DispatchCompletion>>> =
        const { std::cell::RefCell::new(None) };
}

impl Cell {
    pub(super) fn start_exec_pool(self: &Arc<Self>) -> crate::Result<()> {
        let mut handles = Vec::new();
        for i in 0..self.cfg.exec_threads {
            let me = Arc::downgrade(self);
            let queue = self.queue.clone();
            let name = format!("{}-exec-{i}", self.cfg.name);
            let subscriber = tracing::dispatcher::get_default(Clone::clone);
            handles.push(std::thread::Builder::new().name(name).spawn(move || {
                tracing::dispatcher::with_default(&subscriber, || {
                    Cell::exec_loop(me, queue, i);
                });
            })?);
        }
        *self.workers.lock() = handles;
        Ok(())
    }

    pub(super) fn start_idle_sweeper(self: &Arc<Self>) -> crate::Result<()> {
        let me = Arc::downgrade(self);
        let interval = Duration::from_millis(self.cfg.sweep_interval_ms.max(1));
        let stop = self.shutdown.clone();
        let name = format!("{}-idle", self.cfg.name);
        let subscriber = tracing::dispatcher::get_default(Clone::clone);
        let h = std::thread::Builder::new().name(name).spawn(move || {
            let _subscriber = tracing::dispatcher::set_default(&subscriber);
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(interval);
                let Some(cell) = me.upgrade() else { return };
                // Nothing restarts this thread, and its loss is silent:
                // captures stop and parked instances stay resident.
                let swept =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell.sweep_idle()));
                if swept.is_err() {
                    cell.note_caught_panic("sweep");
                }
            }
        })?;
        self.workers.lock().push(h);
        Ok(())
    }

    /// One execution thread. The job body is caught: a host panic inside a
    /// dispatch ends that owner and its resources without taking the thread.
    /// The completion owner outlives unwinding, including acquisition faults
    /// before an instance exists.
    pub(super) fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>, worker: usize) {
        while let Some(job) = queue.pop(worker) {
            let cell = me.upgrade();
            if let Some(cell) = &cell {
                let unanswered = match &job {
                    Job::Dispatch(job) => Some((job.responder.clone(), job.completion.clone())),
                    Job::Wake(_) => None,
                };
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                    Job::Dispatch(j) => cell.run_dispatch(*j),
                    Job::Wake(w) => cell.run_wake(w.slot),
                }));
                let holding = IN_FLIGHT.replace(None).and_then(|owner| owner.upgrade());
                if ran.is_err() {
                    cell.note_caught_panic("dispatch");
                    if let Some(owner) = &holding {
                        owner.select(TerminalReason::HostPanic, 500);
                    }
                    cell.answer_after_panic(holding.clone());
                    if let Some((responder, owner)) = unanswered {
                        owner.select(TerminalReason::HostPanic, 500);
                        owner.finish();
                        owner.answer(&responder, owner.failure_result(500, "host_panic"));
                    }
                }
            } else if let Job::Dispatch(job) = job {
                Self::cancel_queued_dispatch(*job);
            }
            // Released whether the job ran, panicked, or was dropped with the
            // cell, or the lane reads busy to every later placement.
            queue.job_done(worker);
        }
    }

    fn cancel_queued_dispatch(job: DispatchJob) {
        job.completion.select(TerminalReason::CellStopped, 503);
        job.completion.finish();
        job.completion.answer(
            &job.responder,
            job.completion.failure_result(503, "cell_stopped"),
        );
    }

    pub(super) fn note_caught_panic(&self, stage: &'static str) {
        self.caught_panics.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            cell = %self.cfg.name, stage,
            "a host panic was caught at the thread boundary; a guest fault is a trap, so this \
             is a host bug"
        );
    }

    /// Host panics caught and contained; not zero is a host bug. On the cell
    /// rather than [`Metrics`] because it is a fact about this build, not a
    /// tenant.
    pub fn caught_panics(&self) -> u64 {
        self.caught_panics.load(Ordering::Relaxed)
    }

    /// Idempotent.
    pub fn shutdown(&self) {
        self.stop(false);
    }

    pub(super) fn stop(&self, from_drop: bool) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.queue.close();
        let me = std::thread::current().id();
        for h in self.workers.lock().drain(..) {
            // The last strong reference can live on a pool thread, so `Drop`
            // runs there; joining yourself is a deadlock.
            if from_drop && h.thread().id() == me {
                continue;
            }
            if h.join().is_err() {
                tracing::error!(cell = %self.cfg.name, "a cell thread panicked on its way out");
            }
        }
        for job in self.queue.drain_stopped() {
            Self::cancel_queued_dispatch(job);
        }
        for (_, starts) in self.starts.drain() {
            for job in starts {
                Self::cancel_queued_dispatch(job);
            }
        }
        for (id, dispatches) in self.active.drain() {
            let slot = self.slot(id);
            for dispatch in dispatches.into_inner() {
                let owner = dispatch.completion;
                owner.select(TerminalReason::CellStopped, 503);
                let state = slot
                    .as_ref()
                    .and_then(|slot| slot.ctx.dispatch(dispatch.dispatch_id));
                let stream = state
                    .as_ref()
                    .and_then(|state| state.transport.lock().take());
                let _response = stream.as_ref().map(|stream| stream.clone().settlement());
                let _completion = owner.settlement();
                if let Some(stream) = stream {
                    stream.fail("cell_stopped".into());
                }
                if let Some(state) = state {
                    state.request_cancellation();
                }
                if let Some(slot) = &slot {
                    slot.ctx.end_dispatch(dispatch.dispatch_id);
                }
                self.rt.futures.end_dispatch(id, dispatch.dispatch_id);
                if let Some(sender) = dispatch.responder {
                    owner.answer(&sender, owner.failure_result(503, "cell_stopped"));
                }
            }
            if let Some(slot) = slot {
                slot.discard();
                self.forget(&slot);
            }
            self.rt.futures.release_instance(id);
        }
    }
}

/// Cell-level tests: what survives a host panic.
#[cfg(test)]
pub(super) mod cell_tests {
    use super::super::completion::test_events::Events;
    use super::*;
    use crate::fakes::*;
    use isoplane_abi::config::{AppManifest, Bindings};

    struct NoResolver;
    #[async_trait::async_trait]
    impl crate::Resolver for NoResolver {
        async fn resolve(
            &self,
            _h: &str,
            _p: u16,
        ) -> Result<Vec<std::net::SocketAddr>, crate::egress::DnsError> {
            Ok(vec![])
        }
    }

    pub(in crate::cell) fn services() -> Arc<Services> {
        Arc::new(Services {
            egress: Arc::new(crate::egress::HostEgressPolicy::new(
                Box::new(NoResolver),
                crate::egress::ConnectionLimits::uniform(64).expect("connection limits"),
            )),
            fetcher: Arc::new(FakeFetcher::default()),
            hibernation: Arc::new(FakeHibernationStore::default()),
        })
    }

    pub(in crate::cell) fn manifest(name: &str) -> AppManifest {
        AppManifest {
            guest_memory_bytes: None,
            resource_limits: Default::default(),
            validation_limits: Default::default(),
            name: name.into(),
            bindings: Bindings::default(),
            limits: Limits::default(),
            init_visible_vars: vec![],
        }
    }

    pub(in crate::cell) fn http(id: u64) -> DispatchRecord {
        DispatchRecord {
            dispatch_id: id,
            kind: DispatchKind::Http(isoplane_abi::HttpRequest {
                method: "GET".into(),
                url: "http://app.example/go".into(),
                path: "/go".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            occupancy_budget_ms: 1_000,
            now_ms: 0,
        }
    }

    pub(in crate::cell) fn app(cell: &Arc<Cell>, name: &str, behaviour: FakeBehaviour) -> ModuleId {
        let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest(name))
            .expect("register");
        module
    }

    pub(in crate::cell) fn cell(cfg: CellConfig) -> Arc<Cell> {
        Cell::new(cfg, services()).expect("a cell starts")
    }

    pub(in crate::cell) fn wait_for(mut p: impl FnMut() -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if p() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    #[test]
    fn the_sweeper_warms_a_template_only_while_its_module_has_instances() {
        let cell = cell(
            CellConfig::for_test("template-warm"),
            Arc::new(FakeHibernator),
        );
        let act = Arc::new(FakeActivator::named(
            ModuleId::new("warm", "v1"),
            FakeBehaviour::done(),
        ));
        let module = act.module().clone();
        cell.register_module("acme", act.clone(), manifest("warm"))
            .expect("register");
        cell.sweep_idle();
        assert_eq!(
            act.warmed.load(Ordering::Relaxed),
            0,
            "a module with no instances leaves its template to the kernel"
        );

        let done = cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/go".into(),
                },
                http(1),
                Duration::from_secs(10),
            )
            .expect("the dispatch must finish");
        assert_eq!(done.status, 200, "the dispatch must succeed: {done:?}");
        assert!(
            wait_for(
                || act.warmed.load(Ordering::Relaxed) > 0,
                Duration::from_secs(10)
            ),
            "a module with a resident instance has its template warmed"
        );
    }

    /// One execution thread, so there is no second one to hide the loss. The
    /// panic's backtrace on stderr is intended: a caught host panic is a host
    /// bug and stays loud.
    #[test]
    fn a_panicking_dispatch_does_not_take_the_execution_thread_with_it() {
        let cell = cell(CellConfig {
            exec_threads: 1,
            ..CellConfig::for_test("panic-dispatch")
        });

        let bad = ModuleId::new("bad", "v1");
        cell.register_module(
            "acme",
            super::super::completion::tests::panic_activator(bad.clone()),
            manifest("bad"),
        )
        .expect("register");

        let good = app(&cell, "good", FakeBehaviour::done());

        let route = |p: &str| Target::Handler { route: p.into() };
        let events = Events::default();
        let failed = events
            .scope(|| cell.dispatch_blocking(&bad, route("/go"), http(1), Duration::from_secs(10)))
            .expect("the caller must be answered, not left waiting");
        assert_eq!(failed.status, 500, "a host fault is a 500: {failed:?}");
        assert_eq!(
            failed.trap.as_deref(),
            Some("host_panic"),
            "and it says what happened"
        );

        assert!(
            cell.slot(failed.instance).is_none(),
            "the caught panic evicts its instance"
        );

        let ok = cell
            .dispatch_blocking(&good, route("/go"), http(2), Duration::from_secs(10))
            .expect("the pool's only thread must still be running");
        assert!(
            (200..300).contains(&ok.status),
            "the next dispatch is served normally: {ok:?}"
        );

        assert_eq!(
            cell.caught_panics(),
            1,
            "exactly one panic is counted; an uncounted contained fault is a silently slower \
             cell"
        );
        cell.shutdown();
        let records = events.completed(1);
        assert_eq!(
            records.len(),
            1,
            "a caught dispatch panic must emit exactly one completion"
        );
        assert_eq!(
            records[0]["outcome"], "host_panic",
            "the canonical outcome must name the caught host fault"
        );
        assert_eq!(
            records[0]["tenant"], "acme",
            "panic attribution must retain the registered tenant"
        );
        assert_eq!(
            records[0]["occupancy_complete"], false,
            "an interrupted slice cannot claim a complete zero occupancy measurement"
        );
    }

    #[test]
    fn a_refused_slot_entry_closes_its_attached_upload_scope() {
        let events = Events::default();
        let _trace = events.install();
        use crate::bindings::request_body::RequestBodyScope;
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("busy-upload")
        });
        let act = Arc::new(FakeActivator::new(FakeBehaviour::done()));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest("busy-upload"))
            .expect("register reusable guest");
        let run = |id, upload| {
            let (responder, response) = std::sync::mpsc::sync_channel(1);
            cell.run_dispatch(DispatchJob {
                stream: None,
                upload,
                module: module.clone(),
                target: Target::Handler { route: "/".into() }.into(),
                rec: http(id),
                responder: responder.into(),
                completion: cell
                    .completion_owner(&module, &http(id))
                    .expect("registered dispatch owner"),
            });
            response
                .recv_timeout(Duration::from_secs(10))
                .expect("dispatch must answer")
        };
        let first = run(1, None);
        let slot = cell.slot(first.instance).expect("reusable resident");
        let entered = slot.enter().expect("hold the reusable slot busy");
        let upload = RequestBodyScope::new(None, true);
        let refused = run(2, Some(upload.clone()));
        assert_eq!(
            refused.status, 409,
            "the held slice must exercise slot entry refusal"
        );
        assert!(
            matches!(
                upload.can_have_body(),
                Err(isoplane_abi::result::FutureError::Cancelled)
            ),
            "entry refusal must close the attached upload even while its context is retained"
        );
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "entry refusal must end dispatch-owned futures"
        );
        drop(entered);
        cell.shutdown();
        let records = events.completed(1);
        assert_eq!(
            records.len(),
            2,
            "each admitted dispatch must have exactly one final record"
        );
        assert!(
            records
                .iter()
                .any(|event| event["outcome"] == "entry_refused"),
            "the handled terminal refusal must be present in the canonical events"
        );
    }

    #[test]
    fn a_queued_wake_after_reset_leaves_the_pooled_instance_pristine() {
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("stale-reset-wake")
        });
        let act = Arc::new(FakeActivator::new(
            FakeBehaviour::done().with_lazy_init_probe(),
        ));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest("stale-reset-wake"))
            .expect("register");
        let run = |id| {
            let (responder, rx) = std::sync::mpsc::sync_channel(1);
            cell.run_dispatch(DispatchJob {
                stream: None,
                upload: None,
                module: module.clone(),
                target: Target::Handler {
                    route: "/go".into(),
                }
                .into(),
                rec: http(id),
                responder: responder.into(),
                completion: cell
                    .completion_owner(&module, &http(id))
                    .expect("registered dispatch owner"),
            });
            rx.recv_timeout(Duration::from_secs(10))
                .expect("dispatch answered")
        };
        let first = run(1);
        let entry = cell.modules.read().get(&module).cloned().unwrap();
        let (warm, _) = cell.acquire_handler(&entry).expect("warm instance");
        assert_eq!(warm.id, first.instance, "the completed instance stays warm");
        entry
            .pool
            .release(warm.clone(), crate::disposition::policy_reset());
        assert!(
            !warm.is_pristine(),
            "a policy reset must wait for acquisition before resetting"
        );
        let (slot, _) = cell.acquire_handler(&entry).expect("reset candidate");
        assert!(slot.is_pristine(), "acquisition must reset the candidate");
        cell.run_wake(slot.clone());
        assert!(
            slot.is_pristine(),
            "a stale wake must not enter reset guest memory"
        );
        entry.pool.release(slot, crate::disposition::policy_reset());
        let next = run(2);
        assert_eq!(
            next.instance, first.instance,
            "the reset instance remains reusable"
        );
        assert_eq!(
            next.acquired, "pooled_fresh",
            "the dispatch uses the hygienic pool"
        );
        assert_eq!(
            next.body,
            vec![0],
            "the next dispatch observes template state"
        );
        cell.shutdown();
    }

    #[test]
    fn a_failed_deferred_reset_removes_the_discarded_resident() {
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("reset-failure-cleanup")
        });
        let act = Arc::new(FakeActivator::new(FakeBehaviour::done()));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest("reset-failure-cleanup"))
            .expect("register");
        let entry = cell.modules.read().get(&module).cloned().unwrap();
        let (candidate, _) = cell.acquire_handler(&entry).expect("initial activation");
        entry
            .pool
            .release(candidate.clone(), crate::disposition::policy_reset());
        candidate.discard();
        let (replacement, source) = cell.acquire_handler(&entry).expect("fallback activation");
        assert_eq!(
            source,
            AcquireSource::Activated,
            "a refused reset requires activation"
        );
        assert!(
            !cell.index.lock().contains_key(&candidate.id),
            "discarded reset candidates must leave the instance index"
        );
        let residents = cell.residents.lock();
        assert!(
            !residents.contains_key(&candidate.id),
            "discarded reset candidates must not retain resident metadata"
        );
        assert!(
            residents.contains_key(&replacement.id),
            "fallback activation must register its replacement"
        );
        drop(residents);
        cell.shutdown();
    }

    struct ResumingActivator {
        initial: FakeActivator,
        resumed: FakeActivator,
    }

    impl Activator for ResumingActivator {
        fn snapshot_size_bound(&self, max_memory_bytes: u64) -> anyhow::Result<u64> {
            Ok(self
                .initial
                .snapshot_size_bound(max_memory_bytes)?
                .max(self.resumed.snapshot_size_bound(max_memory_bytes)?))
        }
        fn module(&self) -> &ModuleId {
            self.initial.module()
        }
        fn module_hash(&self) -> isoplane_engine::ModuleHash {
            self.initial.module_hash()
        }
        fn abi_rev(&self) -> u32 {
            self.initial.abi_rev()
        }
        fn template_image(&self) -> Arc<isoplane_engine::MemoryImage> {
            self.initial.template_image()
        }
        fn activate(
            &self,
            bindings: Box<dyn isoplane_engine::HostBindings>,
        ) -> anyhow::Result<Box<dyn isoplane_engine::InstanceOps + Send>> {
            if self.initial.activations.load(Ordering::SeqCst) == 0 {
                self.initial.activate(bindings)
            } else {
                self.resumed.activate(bindings)
            }
        }
    }

    fn hibernated_dispatch(
        cell: &Arc<Cell>,
    ) -> (Arc<Slot>, std::sync::mpsc::Receiver<DispatchResult>, u64) {
        let module = ModuleId::new("sleeping-io", "v1");
        let activator = Arc::new(ResumingActivator {
            initial: FakeActivator::named(
                module.clone(),
                FakeBehaviour::program([Step::Park {
                    imports: vec![("clock_deadline_in", 600_000)],
                }]),
            ),
            resumed: FakeActivator::named(
                module.clone(),
                FakeBehaviour::program([Step::Done {
                    status: 201,
                    body: b"restored".to_vec(),
                }]),
            ),
        });
        cell.register_module("acme", activator, manifest("sleeping-io"))
            .expect("the controlled restoration guest must register");
        let (responder, response) = std::sync::mpsc::sync_channel(1);
        cell.run_dispatch(DispatchJob {
            stream: None,
            upload: None,
            module: module.clone(),
            target: Target::Handler { route: "/".into() }.into(),
            rec: http(1),
            responder: responder.into(),
            completion: cell
                .completion_owner(&module, &http(1))
                .expect("registered dispatch owner"),
        });
        let owners = cell.resident_ids();
        assert_eq!(
            owners.len(),
            1,
            "the controlled dispatch must own exactly one slot"
        );
        let slot = cell
            .slot(owners[0])
            .expect("the parked dispatch must remain resident");
        cell.force_hibernate(slot.id)
            .expect("the pending dispatch must capture");
        let futures = cell.futures().live_guest_future_ids();
        assert_eq!(
            futures.len(),
            1,
            "the guest must retain exactly its controlled I/O future"
        );
        (slot, response, futures[0])
    }

    #[test]
    fn an_in_flight_hibernated_handler_cannot_be_discarded() {
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("protected-continuation")
        });
        let (slot, response, future) = hibernated_dispatch(&cell);
        assert!(
            cell.discard_instance(slot.id).is_err(),
            "eviction must refuse an unfinished dispatch even when it is hibernated"
        );
        assert_eq!(
            slot.state(),
            LifecycleTier::Hibernated,
            "refused eviction must leave the continuation available for wake"
        );
        cell.futures()
            .complete(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
        cell.run_wake(slot.clone());
        let result = response
            .recv_timeout(Duration::from_secs(10))
            .expect("the protected dispatch must finish after its I/O completes");
        assert_eq!(
            result.body, b"restored",
            "the original continuation must finish"
        );
        cell.discard_instance(slot.id)
            .expect("the completed handler must become evictable");
        assert_eq!(
            slot.state(),
            LifecycleTier::Discarded,
            "idle eviction must reclaim the instance"
        );
        cell.shutdown();
    }

    #[test]
    fn concurrent_io_wakes_restore_a_hibernated_dispatch_once() {
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("restoring-io")
        });
        let events = super::super::completion::test_events::Events::default();
        let clock = Arc::new(AtomicU64::new(100));
        let observed = clock.clone();
        *cell.completion_clock.write() = Arc::new(move || Some(observed.load(Ordering::Relaxed)));
        let (slot, response, future) = events.scope(|| hibernated_dispatch(&cell));
        assert!(
            events.records().is_empty(),
            "capture must not complete a parked dispatch"
        );
        clock.store(900, Ordering::Relaxed);
        cell.futures()
            .complete(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let cell = cell.clone();
                let slot = slot.clone();
                scope.spawn(move || cell.run_wake(slot));
            }
        });
        let result = response
            .recv_timeout(Duration::from_secs(10))
            .expect("the restored dispatch must answer");
        assert_eq!(
            result.status, 201,
            "the pending continuation must resume rather than restart"
        );
        assert_eq!(
            result.body, b"restored",
            "restoration must deliver the captured continuation's result"
        );
        assert_eq!(
            cell.metrics().hibernation_wakes,
            1,
            "concurrent notifications must coalesce into one restoration"
        );
        assert_eq!(
            cell.futures().pending_total(slot.id),
            0,
            "completion must release every owned future"
        );
        let records = events.completed(1);
        assert_eq!(
            records.len(),
            1,
            "concurrent restored wakes must finish one original owner"
        );
        assert_eq!(
            records[0]["total_wall_ns"], 800,
            "restoration must retain the original delivery clock"
        );
        assert_eq!(records[0]["slices"], 2, "capture must preserve both slices");
        assert_eq!(
            records[0]["capture_hints"], 1,
            "capture hint usage must remain separate from slices"
        );
        cell.shutdown();
    }

    #[test]
    fn a_missing_checkpoint_ends_its_pending_dispatch_and_releases_owned_work() {
        let events = Events::default();
        let _trace = events.install();
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..CellConfig::for_test("missing-checkpoint")
        });
        let (slot, response, future) = hibernated_dispatch(&cell);
        slot.ctx.snapshot.lock().take();
        cell.futures()
            .complete(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
        cell.run_wake(slot.clone());
        let result = response
            .recv_timeout(Duration::from_secs(10))
            .expect("a failed restoration must answer its caller");
        assert_eq!(
            result.status, 500,
            "a missing continuation must fail the active dispatch"
        );
        assert!(
            result.trap.is_some(),
            "the terminal response must carry its restoration diagnostic"
        );
        assert_eq!(
            slot.state(),
            LifecycleTier::Discarded,
            "a failed restoration must discard its instance"
        );
        assert_eq!(
            cell.futures().pending_total(slot.id),
            0,
            "a failed restoration must cancel all owned work"
        );
        assert!(
            !cell.active.contains_key(&slot.id),
            "a failed restoration must close its active dispatch record"
        );
        cell.shutdown();
        let records = events.completed(1);
        assert_eq!(
            records.len(),
            1,
            "each admitted dispatch must have exactly one final record"
        );
        assert!(
            records
                .iter()
                .any(|event| event["outcome"] == "restore_failed"),
            "the handled terminal refusal must be present in the canonical events"
        );
    }

    /// Constructs the state the race produces, a live dispatch on an instance
    /// and a handler holding a stale dispatch id for it, rather than racing
    /// threads. Both directions are asserted, or a handler that never answers
    /// would satisfy the guard.
    #[test]
    fn a_stale_panic_handler_does_not_answer_the_caller_of_another_dispatch() {
        const LIVE: u64 = 900;
        let cell = cell(CellConfig {
            exec_threads: 2,
            ..CellConfig::for_test("stale-panic")
        });
        let m = app(
            &cell,
            "parker",
            FakeBehaviour::program([Step::Park {
                imports: vec![("clock_deadline_in", 600_000)],
            }]),
        );
        let target = Target::Handler { route: "/".into() };
        let rx = cell.submit(&m, target, http(LIVE)).expect("admitted");
        assert!(
            wait_for(|| !cell.resident_ids().is_empty(), Duration::from_secs(10)),
            "the handler never became resident"
        );
        let id = *cell.resident_ids().first().expect("resident");
        assert!(
            wait_for(
                || cell.first_active(id, |a| a.dispatch_id) == Some(LIVE),
                Duration::from_secs(10)
            ),
            "dispatch {LIVE} never reached `active`"
        );

        let stale = cell
            .completion_owner(&m, &http(LIVE))
            .expect("same caller ID with distinct ownership");
        stale.acquired(id, "warm_parked");
        cell.answer_after_panic(Some(stale));
        assert!(
            matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "a handler holding a stale dispatch id answered the live dispatch's caller"
        );
        assert_eq!(
            cell.first_active(id, |a| a.dispatch_id),
            Some(LIVE),
            "and it deleted the live dispatch's entry on the way past"
        );

        let current = cell
            .first_active(id, |dispatch| dispatch.completion.clone())
            .expect("live owner");
        current.select(TerminalReason::HostPanic, 500);
        cell.answer_after_panic(Some(current));
        let out = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the owning handler must answer");
        assert_eq!(out.status, 500, "a host fault is a 500");
        assert_eq!(
            out.trap.as_deref(),
            Some("host_panic"),
            "and names the panic"
        );
        assert_eq!(out.dispatch_id, LIVE, "for the live dispatch");
        cell.shutdown();
    }

    #[tokio::test]
    async fn a_streaming_dispatch_bills_every_hot_slice_once_at_final_completion() {
        let events = Events::default();
        let cell = cell(CellConfig::for_test("stream-occupancy"));
        let mut behaviour = FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", 0)],
            },
            Step::Park {
                imports: vec![("clock_deadline_in", 0)],
            },
            Step::Done {
                status: 200,
                body: b"AB".to_vec(),
            },
        ]);
        behaviour.slice_occupancy = Some(Duration::from_nanos(11));
        behaviour.response_streaming = true;
        let module = app(&cell, "stream-occupancy", behaviour);
        let stream = crate::response_transport::ResponseTransport::new(1);
        let dispatch = {
            let cell = cell.clone();
            let module = module.clone();
            let stream = stream.clone();
            let events = events.clone();
            tokio::task::spawn_blocking(move || {
                events.scope(|| {
                    cell.dispatch_with_stream(
                        &module,
                        Target::Handler {
                            route: "/go".into(),
                        },
                        http(1),
                        Duration::from_secs(10),
                        Some(stream),
                    )
                })
            })
        };
        let mut exchange = stream.exchange().await.expect("stream headers");
        let mut body = vec![];
        while let Some(chunk) = exchange.body.read().await.expect("stream body") {
            body.extend(chunk);
        }
        let result = dispatch
            .await
            .expect("dispatch task")
            .expect("completed dispatch");
        assert_eq!(
            body, b"AB",
            "the initial streamed body must drain before EOF"
        );
        assert_eq!(
            result.slices, 3,
            "both awaited timers and final response must be accounted"
        );
        assert!(
            result.occupancy_ns >= 33,
            "response usage includes every prescribed hot slice"
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while cell.metrics_raw().dispatch_us.count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatch cleanup must settle");
        let records = events.completed(1);
        assert_eq!(records[0]["slice_occupancy_ns"], 33);
        let meters = cell.meters(&module).expect("module meters");
        assert_eq!(
            meters.occupancy_ns(),
            records[0]["occupancy_ns"]
                .as_u64()
                .expect("final occupancy"),
            "streaming must record each hot slice exactly once"
        );
        assert_eq!(
            meters.dispatches(),
            1,
            "parks and EOF must not record extra dispatches"
        );
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "stream EOF must leave no dispatch futures after settlement"
        );
        cell.shutdown();
    }
}
