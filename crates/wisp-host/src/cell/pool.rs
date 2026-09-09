//! The execution and idle-sweep thread pools, and panic containment at their boundary.

use super::*;

thread_local! {
    /// The request this execution thread is running, for the panic handler
    /// in `Cell::exec_loop`, which has to answer a caller whose job unwound.
    /// The dispatch id is needed because `finish_and_release` hands the
    /// instance back before the job returns, so a panic after that point
    /// names a slot another worker may already hold for a new request.
    pub(super) static IN_FLIGHT: std::cell::Cell<Option<(InstanceId, u64)>> =
        const { std::cell::Cell::new(None) };
}

impl Cell {
    pub(super) fn start_exec_pool(self: &Arc<Self>) -> crate::Result<()> {
        let mut handles = Vec::new();
        for i in 0..self.cfg.exec_threads {
            let me = Arc::downgrade(self);
            let queue = self.queue.clone();
            let name = format!("{}-exec-{i}", self.cfg.name);
            handles.push(
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || Cell::exec_loop(me, queue, i))?,
            );
        }
        *self.workers.lock() = handles;
        Ok(())
    }

    pub(super) fn start_idle_sweeper(self: &Arc<Self>) -> crate::Result<()> {
        let me = Arc::downgrade(self);
        let interval = Duration::from_millis(self.cfg.sweep_interval_ms.max(1));
        let stop = self.shutdown.clone();
        let name = format!("{}-idle", self.cfg.name);
        let h = std::thread::Builder::new().name(name).spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(interval);
                let Some(cell) = me.upgrade() else { return };
                // Nothing restarts this thread, and its loss is silent:
                // captures stop and every durable instance stays resident.
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
    /// dispatch costs that request, a counter and a log line rather than the
    /// thread. What it leaks is the instance, which stays in `residents` and
    /// in the gauges, and that dispatch's futures; `answer_after_panic`
    /// removes the `active` entry and answers the caller.
    fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>, worker: usize) {
        while let Some(job) = queue.pop(worker) {
            let cell = me.upgrade();
            if let Some(cell) = &cell {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                    Job::Dispatch(j) => cell.run_dispatch(*j),
                    Job::Wake(w) => cell.run_wake(w.slot),
                }));
                let holding = IN_FLIGHT.replace(None);
                if ran.is_err() {
                    cell.note_caught_panic("dispatch");
                    cell.answer_after_panic(holding);
                }
            }
            // Released whether the job ran, panicked, or was dropped with the
            // cell, or the lane reads busy to every later placement.
            queue.job_done(worker);
            if cell.is_none() {
                return;
            }
        }
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
    }
}

/// Cell-level tests: what survives a host panic.
#[cfg(test)]
mod cell_tests {
    use super::*;
    use crate::fakes::*;
    use wisp_abi::config::{AppManifest, Bindings, HandlerConfig};

    /// Stands in for a host assertion firing under a guest call.
    struct PanicsOnDispatch;

    impl wisp_engine::InstanceOps for PanicsOnDispatch {
        fn dispatch(
            &mut self,
            _rec: &DispatchRecord,
            _b: CpuBudget,
        ) -> wisp_engine::DispatchOutcome {
            panic!("a host assertion fired underneath a dispatch");
        }
        fn wake(
            &mut self,
            _f: u64,
            _r: &wisp_abi::result::ResultRecord,
            _b: CpuBudget,
        ) -> wisp_engine::DispatchOutcome {
            panic!("a host assertion fired underneath a wake");
        }
        fn gc_hint(&mut self, _p: u8) -> wisp_engine::DispatchOutcome {
            unreachable!("this fixture never reaches the hibernation path")
        }
        fn reset(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_parked(&self) -> bool {
            false
        }
        fn is_capture_eligible(&self) -> bool {
            false
        }
        fn memory(&mut self) -> &mut [u8] {
            &mut []
        }
        fn memory_len(&self) -> usize {
            0
        }
        fn capture_mutable_state(&mut self) -> anyhow::Result<wisp_engine::InstanceImage> {
            Ok(Default::default())
        }
        fn restore_mutable_state(&mut self, _i: &wisp_engine::InstanceImage) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct PanicActivator {
        module: ModuleId,
        template: Arc<Vec<u8>>,
    }

    impl Activator for PanicActivator {
        fn module(&self) -> &ModuleId {
            &self.module
        }
        fn module_hash(&self) -> wisp_engine::ModuleHash {
            wisp_engine::ModuleHash::of(&self.template)
        }
        fn abi_rev(&self) -> u32 {
            wisp_abi::ABI_REV
        }
        fn template_image(&self) -> Arc<wisp_engine::MemoryImage> {
            Arc::new(wisp_engine::MemoryImage::from_dense(&self.template))
        }
        fn activate(
            &self,
            _b: Box<dyn wisp_engine::HostBindings>,
        ) -> anyhow::Result<Box<dyn wisp_engine::InstanceOps + Send>> {
            Ok(Box::new(PanicsOnDispatch))
        }
    }

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

    fn services() -> Arc<Services> {
        Arc::new(Services {
            egress: Arc::new(crate::egress::HostEgressPolicy::new(
                Box::new(NoResolver),
                64,
                1 << 20,
            )),
            fetcher: Arc::new(FakeFetcher::default()),
        })
    }

    fn manifest(name: &str) -> AppManifest {
        AppManifest {
            name: name.into(),
            handlers: HandlerConfig {
                lifecycle: HandlerLifecycle::Reuse,
            },
            bindings: Bindings::default(),
            limits: Limits::default(),
            init_visible_vars: vec![],
        }
    }

    fn http(id: u64) -> DispatchRecord {
        DispatchRecord {
            dispatch_id: id,
            kind: DispatchKind::Http(wisp_abi::HttpRequest {
                method: "GET".into(),
                url: "http://app.example/go".into(),
                path: "/go".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: 1_000,
            now_ms: 0,
        }
    }

    fn app(cell: &Arc<Cell>, name: &str, behaviour: FakeBehaviour) -> ModuleId {
        let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest(name))
            .expect("register");
        module
    }

    fn cell(cfg: CellConfig) -> Arc<Cell> {
        Cell::new(cfg, services()).expect("a cell starts")
    }

    fn wait_for(mut p: impl FnMut() -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if p() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
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
            Arc::new(PanicActivator {
                module: bad.clone(),
                template: Arc::new(vec![0u8; wisp_abi::PAGE_SIZE]),
            }),
            manifest("bad"),
        )
        .expect("register");

        let good = app(&cell, "good", FakeBehaviour::done());

        let route = |p: &str| Target::Handler { route: p.into() };
        let failed = cell
            .dispatch_blocking(&bad, route("/go"), http(1), Duration::from_secs(10))
            .expect("the caller must be answered, not left waiting");
        assert_eq!(failed.status, 500, "a host fault is a 500: {failed:?}");
        assert_eq!(
            failed.trap.as_deref(),
            Some("host_panic"),
            "and it says what happened"
        );

        let slot = cell
            .slot(failed.instance)
            .expect("the caught panic retains its slot for inspection");
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        let payload = postcard::to_allocvec(&wisp_abi::calls::FetchRequest {
            method: "GET".into(),
            url: "http://example.test/".into(),
            headers: vec![],
            body: vec![],
        })
        .expect("the probe request must encode");
        use wisp_engine::HostBindings;
        let future = bindings.call_future(wisp_engine::ImportCall {
            import: "http_fetch",
            payload: &payload,
            scalars: [0; 3],
        });
        assert!(
            matches!(cell.futures().take_ready(failed.instance),
            Some((id, wisp_abi::result::ResultRecord::Err(wisp_abi::result::FutureError::Cancelled))) if id == future),
            "the caught panic must close its HTTP scope even while retaining the context"
        );

        let ok = cell
            .dispatch_blocking(&good, route("/go"), http(2), Duration::from_secs(10))
            .expect("the pool's only thread must still be running");
        assert!(
            (200..300).contains(&ok.status),
            "the next request is served normally: {ok:?}"
        );

        assert_eq!(
            cell.caught_panics(),
            1,
            "exactly one panic is counted; an uncounted contained fault is a silently slower \
             cell"
        );
        cell.shutdown();
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
        let mut manifest = manifest("stale-reset-wake");
        manifest.handlers.lifecycle = HandlerLifecycle::Fresh;
        cell.register_module("acme", act, manifest)
            .expect("register");
        let run = |id| {
            let (responder, rx) = std::sync::mpsc::sync_channel(1);
            cell.run_dispatch(DispatchJob {
                stream: None,
                module: module.clone(),
                target: Target::Handler {
                    route: "/go".into(),
                },
                rec: http(id),
                responder,
                enqueued_ns: None,
            });
            rx.recv_timeout(Duration::from_secs(10))
                .expect("dispatch answered")
        };
        let first = run(1);
        let slot = cell.slot(first.instance).expect("pooled instance");
        assert!(
            !slot.is_pristine(),
            "a completed dispatch must wait for acquisition before resetting"
        );
        let entry = cell.modules.read().get(&module).cloned().unwrap();
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
            "the request uses the hygienic pool"
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
        let mut manifest = manifest("reset-failure-cleanup");
        manifest.handlers.lifecycle = HandlerLifecycle::Fresh;
        cell.register_module("acme", act, manifest)
            .expect("register");
        let entry = cell.modules.read().get(&module).cloned().unwrap();
        let (candidate, _) = cell.acquire_handler(&entry).expect("initial activation");
        entry
            .pool
            .release(candidate.clone(), crate::disposition::policy_reset());
        candidate.tombstone();
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

    /// Constructs the state the race produces, a live request on an instance
    /// and a handler holding a stale dispatch id for it, rather than racing
    /// threads. Both directions are asserted, or a handler that never answers
    /// would satisfy the guard.
    #[test]
    fn a_stale_panic_handler_does_not_answer_another_requests_caller() {
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

        const LIVE: u64 = 7;
        let rx = cell.submit(&m, target, http(LIVE)).expect("admitted");
        assert!(
            wait_for(
                || !cell.residents.lock().is_empty(),
                Duration::from_secs(10)
            ),
            "the Handler instance never became resident"
        );
        let id = {
            let residents = cell.residents.lock();
            assert_eq!(residents.len(), 1, "the single parked Handler is resident");
            *residents
                .keys()
                .next()
                .expect("the resident count was checked")
        };
        assert!(
            wait_for(
                || cell.active.with(&id, |a| a.dispatch_id) == Some(LIVE),
                Duration::from_secs(10)
            ),
            "dispatch {LIVE} never reached `active`"
        );

        cell.answer_after_panic(Some((id, LIVE - 1)));
        assert!(
            matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "a handler holding a stale dispatch id answered the live request's caller"
        );
        assert_eq!(
            cell.active.with(&id, |a| a.dispatch_id),
            Some(LIVE),
            "and it deleted the live request's entry on the way past"
        );

        cell.answer_after_panic(Some((id, LIVE)));
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
        let cell = cell(CellConfig::for_test("stream-cpu"));
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
        behaviour.slice_cpu = Some(Duration::from_nanos(11));
        behaviour.response_streaming = true;
        let module = app(&cell, "stream-cpu", behaviour);
        let stream = crate::response_transport::ResponseTransport::new(1);
        let dispatch = {
            let cell = cell.clone();
            let module = module.clone();
            let stream = stream.clone();
            tokio::task::spawn_blocking(move || {
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
        assert_eq!(
            result.cpu_ns, 33,
            "final usage must sum every prescribed hot slice"
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while cell.metrics_raw().request_us.count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatch cleanup must settle");
        let meters = cell.meters(&module).expect("module meters");
        assert_eq!(
            meters.cpu_ns(),
            result.cpu_ns,
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
            "stream EOF must leave no request futures after settlement"
        );
        cell.shutdown();
    }
}
