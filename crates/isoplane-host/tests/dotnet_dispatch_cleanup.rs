//! Real .NET guest resource cleanup through the production cell's dispatch boundary.

mod common;
#[path = "common/managed.rs"]
mod managed;
use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::IsoplaneEngine;
use isoplane_fixtures::GuestRuntime;
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::ModuleId;
use std::sync::Arc;
use std::time::Duration;

/// The shared runtime Task compiled, loaded once for every test in this binary on an engine
/// with room for each test's instances.
fn shared_runtime() -> &'static (Arc<IsoplaneEngine>, isoplane_engine::Template) {
    static RUNTIME: std::sync::OnceLock<(Arc<IsoplaneEngine>, isoplane_engine::Template)> =
        std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        let engine = Arc::new(
            IsoplaneEngine::new(isoplane_fixtures::engine_config(32))
                .expect("the managed fixture engine must start"),
        );
        let template = isoplane_fixtures::load_compiled_runtime(&engine);
        (engine, template)
    })
}

fn cleanup_cell(
    runtime: GuestRuntime,
    work: Arc<PendingWork>,
    operation: &str,
) -> (Arc<Cell>, ModuleId) {
    cleanup_cell_with(
        CellConfig::for_test("managed-cleanup"),
        runtime,
        work,
        operation,
    )
}

fn cleanup_cell_with(
    config: CellConfig,
    runtime: GuestRuntime,
    work: Arc<PendingWork>,
    operation: &str,
) -> (Arc<Cell>, ModuleId) {
    let GuestRuntime::Mono = runtime;
    let (engine, template) = shared_runtime();
    let module = ModuleId::new("managed-cleanup", "v1");
    let template = template
        .clone()
        .with_application_files(managed::files("isoplane_guest_dotnet_native_io"))
        .expect("the cleanup fixture must bind its application files");
    let activator = EngineActivator::new(engine.clone(), template, module.clone());
    let cell = if operation == "/timer" {
        common::rig_resolving(config, "93.184.216.34:80").0
    } else {
        let egress = isoplane_host::egress::HostEgressPolicy::new(
            Box::new(PendingResolver(work.clone())),
            isoplane_host::egress::ConnectionLimits::uniform(32).expect("connection limits"),
        )
        .with_connections(Arc::new(PendingConnection(work)));
        let services = Arc::new(isoplane_host::cell::Services {
            fetcher: Arc::new(isoplane_host::fakes::FakeFetcher::default()),
            egress: Arc::new(egress),
        });
        Cell::new(config, services).expect("the production cell must start")
    };
    cell.register_module(
        "tenant",
        Arc::new(activator),
        common::manifest("managed-cleanup", Limits::default()),
    )
    .expect("the cleanup module must register");
    (cell, module)
}

async fn dispatch_at_idle(
    cell: &Arc<Cell>,
    module: &ModuleId,
    path: &str,
) -> isoplane_host::DispatchResult {
    let result = dispatch_request(cell, module, path);
    wait_until_idle(cell, module).await;
    result
}

fn dispatch_request(
    cell: &Arc<Cell>,
    module: &ModuleId,
    operation: &str,
) -> isoplane_host::DispatchResult {
    let path = format!("/cleanup{operation}");
    let request = HttpRequest {
        method: "GET".into(),
        url: format!("https://cleanup.test{path}"),
        path: path.clone(),
        query: vec![],
        headers: vec![],
        body: vec![],
    };
    let result = cell
        .dispatch_blocking(
            module,
            Target::Handler { route: path },
            cell.http_record(module, request),
            Duration::from_secs(60),
        )
        .expect("the managed request must complete");
    assert_eq!(
        result.status, 200,
        "the fixture must complete normally: {result:?}"
    );
    result
}

async fn wait_until_idle(cell: &Arc<Cell>, module: &ModuleId) {
    // The response precedes release into the warm pool. Observe that boundary
    // before inspecting cleanup or dispatching onto the same instance again.
    tokio::time::timeout(Duration::from_secs(60), async {
        while cell.idle_instances(module) != Some(1) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the completed guest must return to the warm pool");
}

async fn check_work_after_the_response(runtime: GuestRuntime, operation: &str) {
    isoplane_harness::LogCapture::global();
    let work = Arc::new(PendingWork::default());
    let (cell, module) = cleanup_cell(runtime, work.clone(), operation);
    let armed = dispatch_request(&cell, &module, operation);
    assert_eq!(
        armed.body, b"armed",
        "the client is answered while the guest's operation is still pending"
    );
    wait_until_idle(&cell, &module).await;
    assert_eq!(
        cell.metrics_raw().dispatch_us.count(),
        1,
        "request completion does not wait for the pending operation"
    );
    work.release.notify_one();
    let callback = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(record) = cell
                .slot(armed.instance)
                .expect("retained instance")
                .ctx
                .emitted_logs()
                .into_iter()
                .find(|record| record.fields.contains("retained-operation-completed"))
            {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background work must finish without a later request");
    assert_eq!(callback.dispatch, isoplane_host::futures::INSTANCE_WORK);
    wait_until_idle(&cell, &module).await;
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.instance, armed.instance,
        "work left after the response must not cost its instance"
    );
    assert_eq!(
        observed.body, b"2:1",
        "the left-behind work must have run to completion on the instance"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "the finished work must leave no host futures behind"
    );
    cell.shutdown();
}

#[derive(Default)]
struct PendingWork {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    clock_barrier: tokio::sync::Notify,
    release_clock: tokio::sync::Notify,
}

impl PendingWork {
    /// Holds the operation until the test releases it.
    async fn wait(self: &Arc<Self>) {
        self.started.notify_one();
        self.release.notified().await;
    }
}

struct PendingResolver(Arc<PendingWork>);
#[async_trait::async_trait]
impl isoplane_host::Resolver for PendingResolver {
    async fn resolve(
        &self,
        name: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        match name {
            "pending.test" => self.0.wait().await,
            "barrier.test" => self.0.started.notified().await,
            "clock-barrier.test" => {
                self.0.clock_barrier.notify_one();
                self.0.release_clock.notified().await;
            }
            other => panic!("unexpected fixture DNS request: {other}"),
        }
        Ok(vec![std::net::SocketAddr::from(([93, 184, 216, 34], port))])
    }
}

struct PendingConnection(Arc<PendingWork>);
#[async_trait::async_trait]
impl isoplane_host::egress::ConnectionFactory for PendingConnection {
    async fn connect(
        &self,
        _owner: &isoplane_host::egress::ConnectionOwner,
        destination: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        assert_eq!(
            destination,
            std::net::SocketAddr::from(([93, 184, 216, 34], 80)),
            "the fixture must request its explicitly controlled peer"
        );
        self.0.wait().await;
        Err(std::io::ErrorKind::ConnectionRefused.into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_cancelled_request_fails_without_discarding_its_instance() {
    let (cell, module) = cleanup_cell(
        GuestRuntime::Mono,
        Arc::new(PendingWork::default()),
        "/timer",
    );
    let before = dispatch_at_idle(&cell, &module, "/observe").await;
    let request = HttpRequest {
        method: "GET".into(),
        url: "https://cleanup.test/cleanup/cancel".into(),
        path: "/cleanup/cancel".into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    };
    let cancelled = cell
        .dispatch(
            &module,
            Target::Handler {
                route: request.path.clone(),
            },
            cell.http_record(&module, request),
            Duration::from_secs(60),
        )
        .await
        .expect("the cancelled request must settle");
    assert_eq!(cancelled.status, 500);
    assert_eq!(cancelled.completion, isoplane_engine::Completion::Failed);
    assert_eq!(cancelled.instance, before.instance);
    wait_until_idle(&cell, &module).await;
    let after = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(after.instance, before.instance);
    assert_eq!(
        after.body, b"3:1",
        "cancellation must run finally and preserve instance state"
    );
    assert_eq!(cell.metrics().cancellation_grace_kills, 0);
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_request_failures_leave_other_requests_and_instance_state_intact() {
    tokio::time::timeout(Duration::from_secs(60), async {
        for (path, status, completion) in [
            ("/cleanup/throw", 500, isoplane_engine::Completion::Failed),
            ("/cleanup/cancel", 500, isoplane_engine::Completion::Failed),
        ] {
            let work = Arc::new(PendingWork::default());
            let (cell, module) = cleanup_cell(GuestRuntime::Mono, work.clone(), "/dns");
            let request = |path: &str| HttpRequest {
                method: "GET".into(),
                url: format!("https://cleanup.test{path}"),
                path: path.into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            };
            let waiting_cell = cell.clone();
            let waiting_module = module.clone();
            let waiting_request = request("/cleanup/wait");
            let waiting = tokio::spawn(async move {
                waiting_cell
                    .dispatch(
                        &waiting_module,
                        Target::Handler {
                            route: waiting_request.path.clone(),
                        },
                        waiting_cell.http_record(&waiting_module, waiting_request),
                        Duration::from_secs(60),
                    )
                    .await
                    .expect("the concurrent request must complete")
            });
            work.started.notified().await;
            let instances = cell.resident_ids();
            assert_eq!(instances.len(), 1);
            let failed_request = request(path);
            let failure = cell
                .dispatch(
                    &module,
                    Target::Handler { route: path.into() },
                    cell.http_record(&module, failed_request),
                    Duration::from_secs(60),
                )
                .await
                .expect("the failing handler must settle");
            assert_eq!(
                failure.instance, instances[0],
                "the requests must share their instance"
            );
            assert_eq!((failure.status, failure.completion), (status, completion));
            work.release.notify_one();
            let success = waiting
                .await
                .expect("the waiting request task must not panic");
            assert_eq!(success.instance, failure.instance);
            assert_eq!(success.status, 200);
            assert_eq!(success.body, b"released");
            wait_until_idle(&cell, &module).await;
            let next = dispatch_at_idle(&cell, &module, "/observe").await;
            assert_eq!(next.instance, failure.instance);
            assert_eq!(
                next.body,
                if path == "/cleanup/cancel" {
                    b"3:1"
                } else {
                    b"3:0"
                }
            );
            if status == 500 {
                use isoplane_abi::host::{CodeTier, Dispatcher, RouteTarget};
                let mut response = cell
                    .dispatch_http_stream(
                        &RouteTarget {
                            app: module.app.clone(),
                            version: module.version.clone(),
                            tenant: "tenant".into(),

                            tier: CodeTier::Fast,
                        },
                        request(path),
                    )
                    .await
                    .expect("a handler failure must produce an HTTP response");
                assert_eq!(response.status, 500);
                assert_eq!(
                    response
                        .body
                        .read()
                        .await
                        .expect("a complete error response"),
                    None
                );
                wait_until_idle(&cell, &module).await;
                assert!(cell.slot(failure.instance).is_some());
            }
            cell.shutdown();
        }
    })
    .await
    .expect("request failures must not strand concurrent work");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fatal_exception_inside_an_aggregate_discards_the_instance() {
    let (cell, module) = cleanup_cell(
        GuestRuntime::Mono,
        Arc::new(PendingWork::default()),
        "/timer",
    );
    let before = dispatch_at_idle(&cell, &module, "/observe").await;
    wait_until_idle(&cell, &module).await;
    let request = HttpRequest {
        method: "GET".into(),
        url: "https://cleanup.test/cleanup/fatal-aggregate".into(),
        path: "/cleanup/fatal-aggregate".into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    };
    let failed = cell
        .dispatch(
            &module,
            Target::Handler {
                route: request.path.clone(),
            },
            cell.http_record(&module, request),
            Duration::from_secs(60),
        )
        .await
        .expect("fatal response");
    assert_eq!(failed.completion, isoplane_engine::Completion::Trapped);
    assert_eq!(failed.instance, before.instance);
    let next = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_ne!(next.instance, before.instance);
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timer_left_pending_keeps_its_instance() {
    let (cell, module) = cleanup_cell(
        GuestRuntime::Mono,
        Arc::new(PendingWork::default()),
        "/timer",
    );
    let long = dispatch_at_idle(&cell, &module, "/timer").await;
    assert_eq!(
        long.body, b"armed",
        "step 1: the guest must leave a timer pending"
    );
    assert_eq!(
        cell.futures()
            .pending_of_dispatch(
                long.instance,
                isoplane_host::futures::INSTANCE_WORK,
                "poll_call"
            )
            .len(),
        1,
        "step 1: the pending timer wake transfers to background ownership"
    );
    let waiting = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        (waiting.instance, waiting.body.as_slice()),
        (long.instance, &b"2:0"[..]),
        "step 2: the instance must be kept, its timer not yet due"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lookup_left_pending_after_the_response_completes_on_its_instance() {
    check_work_after_the_response(GuestRuntime::Mono, "/dns").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_resuming_on_its_dispatch_context_after_the_response_completes_on_its_instance() {
    check_work_after_the_response(GuestRuntime::Mono, "/dns-on-context").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_left_pending_after_the_response_completes_on_its_instance() {
    check_work_after_the_response(GuestRuntime::Mono, "/socket").await;
}

async fn check_guest_timer_after_clock_advances(runtime: GuestRuntime) {
    let work = Arc::new(PendingWork::default());
    let (cell, module) = cleanup_cell(runtime, work.clone(), "/dns");
    let armed = dispatch_at_idle(&cell, &module, "/observe").await;
    let slot = cell
        .slot(armed.instance)
        .expect("the reused handler must remain resident");
    let after_deadline_ms = slot.ctx.slice_clock_ms() + 2 * 24 * 60 * 60 * 1000;
    slot.ctx
        .begin_slice(after_deadline_ms, slot.ctx.dispatch_id());

    let requester = cell.clone();
    let request_module = module.clone();
    let response = tokio::task::spawn_blocking(move || {
        let request = HttpRequest {
            method: "GET".into(),
            url: "https://cleanup.test/cleanup/drive-timer".into(),
            path: "/cleanup/drive-timer".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        requester.dispatch_blocking(
            &request_module,
            Target::Handler {
                route: "/cleanup/drive-timer".into(),
            },
            requester.http_record(&request_module, request),
            Duration::from_secs(10),
        )
    });
    tokio::time::timeout(Duration::from_secs(60), work.clock_barrier.notified())
        .await
        .expect("the later dispatch must register its timer before clock advancement");
    tokio::time::timeout(Duration::from_secs(60), async {
        while !slot.is_capture_eligible() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("clock advancement must occur after the guest returns to its parked boundary");
    slot.ctx
        .begin_slice(after_deadline_ms + 10, slot.ctx.dispatch_id());
    work.release_clock.notify_one();
    let result = response
        .await
        .expect("the dispatch thread must not panic")
        .expect("a later dispatch's own timer must complete after its deadline");
    assert_eq!(
        result.status, 200,
        "the dispatch's own timer must complete: {result:?}"
    );
    assert_eq!(result.body, b"2:0", "no other callback may run");
    wait_until_idle(&cell, &module).await;
    assert_eq!(
        cell.metrics().dispatches,
        2,
        "request timers and unsafe callbacks must complete without background dispatches"
    );
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.body, b"3:0",
        "a delayed continuation must not run after the second response was formed"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_interpreted_guest_drives_its_timer_after_clock_advances() {
    check_guest_timer_after_clock_advances(GuestRuntime::Mono).await;
}

async fn check_dormant_promise(runtime: GuestRuntime) {
    isoplane_harness::LogCapture::global();
    let (cell, module) = cleanup_cell(runtime, Arc::new(PendingWork::default()), "/timer");
    let armed = dispatch_at_idle(&cell, &module, "/promise-arm").await;
    assert_eq!(
        armed.body, b"armed",
        "the first dispatch must register a dormant managed continuation"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "a managed promise must remain invisible to native-future detection after response"
    );
    let signalled = dispatch_at_idle(&cell, &module, "/promise-signal").await;
    assert_eq!(
        signalled.instance, armed.instance,
        "the later dispatch must signal the retained promise in the same guest"
    );
    assert_eq!(
        signalled.body, b"signalled",
        "the later dispatch must complete the managed promise"
    );
    let callback = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(record) = cell
                .slot(armed.instance)
                .expect("retained instance")
                .ctx
                .emitted_logs()
                .into_iter()
                .find(|record| record.fields.contains("retained-promise-completed"))
            {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retained continuation must run");
    assert_eq!(
        callback.dispatch,
        isoplane_host::futures::INSTANCE_WORK,
        "the signalling request must not execute the retained callback"
    );
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.instance, armed.instance,
        "the callback observation must retain the original guest state"
    );
    assert_eq!(
        String::from_utf8_lossy(&observed.body),
        "3:1",
        "a later active dispatch may explicitly signal retained managed state"
    );
    cell.shutdown();
}

async fn check_same_dispatch_promise(runtime: GuestRuntime) {
    let (cell, module) = cleanup_cell(runtime, Arc::new(PendingWork::default()), "/timer");
    let completed = dispatch_at_idle(&cell, &module, "/promise-complete").await;
    assert!(
        completed.slices > 1,
        "a callback chain must yield between bounded batches"
    );
    assert_eq!(
        String::from_utf8_lossy(&completed.body),
        "1:1",
        "an awaited continuation completed within its own dispatch must run once"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "the positive control must also need no native future"
    );
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.instance, completed.instance,
        "the positive control must observe the same guest"
    );
    assert_eq!(
        String::from_utf8_lossy(&observed.body),
        "2:1",
        "the completed continuation must not run twice"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dormant_interpreted_promise_can_be_signalled_by_a_later_active_dispatch() {
    check_dormant_promise(GuestRuntime::Mono).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interpreted_promise_can_resume_work_within_its_own_dispatch() {
    check_same_dispatch_promise(GuestRuntime::Mono).await;
}

async fn check_idle_socket_reuse(runtime: GuestRuntime) {
    let (cell, module) = cleanup_cell(runtime, Arc::new(PendingWork::default()), "/timer");
    let retained = dispatch_at_idle(&cell, &module, "/idle-socket").await;
    assert_eq!(
        retained.body, b"retained",
        "the fixture must retain an owned native socket"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "an idle socket must not own pending guest work"
    );
    let observed = dispatch_at_idle(&cell, &module, "/read-idle-socket").await;
    assert_eq!(
        observed.instance, retained.instance,
        "an idle native handle must not force eviction"
    );
    assert_eq!(
        observed.body, b"2:True",
        "the reused guest must retain its socket and configuration"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_interpreted_socket_does_not_prevent_clean_guest_reuse() {
    check_idle_socket_reuse(GuestRuntime::Mono).await;
}

fn upload_cell() -> (Arc<Cell>, ModuleId) {
    let (engine, template) = shared_runtime();
    let module = ModuleId::new("managed-upload", "v1");
    let template = template
        .clone()
        .with_application_files(managed::files("isoplane_guest_dotnet_upload"))
        .expect("the upload fixture must bind its application files");
    let activator = EngineActivator::new(engine.clone(), template, module.clone());
    let (cell, _) =
        common::rig_resolving(CellConfig::for_test("managed-upload"), "93.184.216.34:80");
    cell.register_module(
        "tenant",
        Arc::new(activator),
        common::manifest("managed-upload", Limits::default()),
    )
    .expect("the upload module must register");
    (cell, module)
}

/// An upload whose next bytes never arrive.
struct StalledUpload;
#[async_trait::async_trait]
impl isoplane_abi::host::HttpBody for StalledUpload {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        std::future::pending().await
    }
}

/// An upload with no bytes.
struct EmptyUpload;
#[async_trait::async_trait]
impl isoplane_abi::host::HttpBody for EmptyUpload {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

async fn upload(
    cell: &Arc<Cell>,
    path: &str,
    body: Box<dyn isoplane_abi::host::HttpBody>,
) -> String {
    use isoplane_abi::host::{CodeTier, Dispatcher, HttpUpload, RouteTarget};
    let target = RouteTarget {
        app: "managed-upload".into(),
        version: "v1".into(),
        tenant: "tenant".into(),

        tier: CodeTier::Fast,
    };
    let request = HttpRequest {
        method: "POST".into(),
        url: format!("https://upload.test{path}"),
        path: path.into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    };
    let mut response = cell
        .dispatch_http_upload(
            &target,
            HttpUpload {
                request,
                body,
                can_have_body: true,
            },
        )
        .await
        .expect("the upload dispatch must answer");
    assert_eq!(response.status, 200, "{path} must complete normally");
    let mut text = Vec::new();
    while let Some(chunk) = response.body.read().await.expect("the response body") {
        text.extend(chunk);
    }
    String::from_utf8(text).expect("a text response")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_body_read_left_after_the_response_fails_instead_of_waiting_forever() {
    let (cell, module) = upload_cell();
    assert_eq!(
        upload(&cell, "/leave-read", Box::new(StalledUpload)).await,
        "left",
        "the handler responds while its read of the upload is pending"
    );
    wait_until_idle(&cell, &module).await;
    assert_eq!(
        upload(&cell, "/left-read", Box::new(EmptyUpload)).await,
        "failed",
        "a read the response has ended must fail, so the code awaiting it can finish"
    );
    cell.shutdown();
}

struct ObservedUploadDrop(Option<tokio::sync::oneshot::Sender<()>>);

#[async_trait::async_trait]
impl isoplane_abi::host::HttpBody for ObservedUploadDrop {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        std::future::pending().await
    }
}

impl Drop for ObservedUploadDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            sender
                .send(())
                .expect("the test still waits for upload release");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_pending_managed_upload_read_releases_the_operation_and_keeps_the_instance() {
    let (cell, module) = upload_cell();
    let (dropped, released) = tokio::sync::oneshot::channel();
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        upload(
            &cell,
            "/cancel-read",
            Box::new(ObservedUploadDrop(Some(dropped))),
        ),
    )
    .await
    .expect("cancellation must settle the pending read");
    assert_eq!(response, "cancelled");
    tokio::time::timeout(Duration::from_secs(30), released)
        .await
        .expect("the cancelled upload must be released")
        .expect("upload release signal");
    wait_until_idle(&cell, &module).await;
    let before = cell.resident_ids();
    assert_eq!(before.len(), 1);
    assert_eq!(upload(&cell, "/", Box::new(EmptyUpload)).await, "stream:");
    wait_until_idle(&cell, &module).await;
    assert_eq!(cell.resident_ids(), before);
    cell.shutdown();
}
