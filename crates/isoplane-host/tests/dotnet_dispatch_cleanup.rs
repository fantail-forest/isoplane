//! Real .NET guest resource cleanup through the production cell's dispatch boundary.

mod common;
#[path = "common/managed.rs"]
mod managed;
use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{EngineConfig, IsoplaneEngine};
use isoplane_fixtures::GuestRuntime;
use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::ModuleId;
use std::sync::Arc;
use std::time::Duration;

/// The shared runtime, compiled once for every test in this binary, on an engine with room
/// for each test's instances.
fn shared_runtime() -> &'static (Arc<IsoplaneEngine>, isoplane_engine::Template) {
    static RUNTIME: std::sync::OnceLock<(Arc<IsoplaneEngine>, isoplane_engine::Template)> =
        std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        let engine = Arc::new(
            IsoplaneEngine::new(EngineConfig {
                max_instances: 32,
                max_memory_bytes: 256 << 20,
                guard_size_bytes: 64 << 20,
                ..EngineConfig::default()
            })
            .expect("the managed fixture engine must start"),
        );
        let template = engine
            .load_template(&isoplane_fixtures::mono_runtime())
            .expect("the shared runtime must load through the real engine");
        (engine, template)
    })
}

fn cleanup_cell(
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
        common::rig_resolving(CellConfig::for_test("managed-cleanup"), "93.184.216.34:80").0
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
        Cell::new(CellConfig::for_test("managed-cleanup"), services)
            .expect("the production cell must start")
    };
    cell.register_module(
        "tenant",
        Arc::new(activator),
        common::manifest(
            "managed-cleanup",
            HandlerLifecycle::Reuse,
            Limits::default(),
        ),
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
        while cell.idle_instances(module) != Some((1, 0)) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the completed guest must return to the warm pool");
}

async fn dispatch_and_discard(
    cell: &Arc<Cell>,
    module: &ModuleId,
    path: &str,
) -> isoplane_host::DispatchResult {
    let result = dispatch_request(cell, module, path);
    tokio::time::timeout(Duration::from_secs(10), async {
        while cell.slot(result.instance).is_some() || cell.idle_instances(module) != Some((0, 0)) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("an instance with abandoned guest work must be discarded, not pooled");
    result
}

async fn check_managed_cleanup(runtime: GuestRuntime, operation: &str) {
    let work = Arc::new(PendingWork::default());
    let (cell, module) = cleanup_cell(runtime, work.clone(), operation);
    let armed = dispatch_and_discard(&cell, &module, operation).await;
    assert_eq!(
        armed.body, b"armed",
        "the guest must execute the abandoned-operation branch"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "the production dispatch boundary must cancel pending managed work without test cleanup"
    );
    if operation != "/timer" {
        tokio::time::timeout(Duration::from_secs(60), work.cancelled.notified())
            .await
            .expect("dispatch completion must cancel the pending host operation");
    }
    let replacement = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_ne!(
        replacement.instance, armed.instance,
        "abandoned async work must prevent reuse of the affected guest"
    );
    assert_eq!(
        replacement.body, b"1:0",
        "the replacement must start with clean managed state"
    );
    let reused = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        reused.instance, replacement.instance,
        "a clean replacement must remain eligible for warm reuse"
    );
    assert_eq!(
        reused.body, b"2:0",
        "clean reuse must preserve application state"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "later dispatches must not re-arm an abandoned request's future"
    );
    cell.shutdown();
}

#[derive(Default)]
struct PendingWork {
    started: tokio::sync::Notify,
    cancelled: tokio::sync::Notify,
    clock_barrier: tokio::sync::Notify,
    release_clock: tokio::sync::Notify,
}

struct CancellationObserved(Arc<PendingWork>);
impl Drop for CancellationObserved {
    fn drop(&mut self) {
        self.0.cancelled.notify_one();
    }
}

impl PendingWork {
    async fn wait(self: &Arc<Self>) {
        let _cancelled = CancellationObserved(self.clone());
        self.started.notify_one();
        std::future::pending::<()>().await;
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
        unreachable!("the pending connection must be cancelled, never completed")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cell_cancels_an_abandoned_interpreted_timer_and_discards_only_the_affected_guest() {
    check_managed_cleanup(GuestRuntime::Mono, "/timer").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cell_cancels_an_abandoned_interpreted_dns_and_discards_only_the_affected_guest() {
    check_managed_cleanup(GuestRuntime::Mono, "/dns").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cell_cancels_an_abandoned_interpreted_socket_and_discards_only_the_affected_guest() {
    check_managed_cleanup(GuestRuntime::Mono, "/socket").await;
}

async fn check_expired_abandoned_timer(runtime: GuestRuntime, abandon_first: bool) {
    let work = Arc::new(PendingWork::default());
    let (cell, module) = cleanup_cell(runtime, work.clone(), "/dns");
    if abandon_first {
        dispatch_and_discard(&cell, &module, "/timer").await;
    }
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
        .expect("the later request must register its timer before clock advancement");
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
        .expect("the request thread must not panic")
        .expect("a later request's own timer must complete after its deadline");
    assert_eq!(
        result.status, 200,
        "abandoned timers must not break a later request: {result:?}"
    );
    assert_eq!(
        result.body, b"2:0",
        "expired abandoned callbacks must not run in a later request"
    );
    wait_until_idle(&cell, &module).await;
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.body, b"3:0",
        "a delayed continuation must not run after the second response was formed"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_abandoned_interpreted_timer_cannot_break_a_later_requests_timer() {
    check_expired_abandoned_timer(GuestRuntime::Mono, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_interpreted_guest_drives_its_timer_after_clock_advances() {
    check_expired_abandoned_timer(GuestRuntime::Mono, false).await;
}

async fn check_dormant_promise(runtime: GuestRuntime) {
    let (cell, module) = cleanup_cell(runtime, Arc::new(PendingWork::default()), "/timer");
    let armed = dispatch_at_idle(&cell, &module, "/promise-arm").await;
    assert_eq!(
        armed.body, b"armed",
        "the first request must register a dormant managed continuation"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "a managed promise must remain invisible to native-future detection after response"
    );
    let signalled = dispatch_at_idle(&cell, &module, "/promise-signal").await;
    assert_eq!(
        signalled.instance, armed.instance,
        "the later request must signal the retained promise in the same guest"
    );
    assert_eq!(
        signalled.body, b"signalled",
        "the later request must complete the managed promise"
    );
    let observed = dispatch_at_idle(&cell, &module, "/observe").await;
    assert_eq!(
        observed.instance, armed.instance,
        "the callback observation must retain the original guest state"
    );
    assert_eq!(
        String::from_utf8_lossy(&observed.body),
        "3:1",
        "a later active request may explicitly signal retained managed state"
    );
    cell.shutdown();
}

async fn check_same_request_promise(runtime: GuestRuntime) {
    let (cell, module) = cleanup_cell(runtime, Arc::new(PendingWork::default()), "/timer");
    let completed = dispatch_at_idle(&cell, &module, "/promise-complete").await;
    assert_eq!(
        String::from_utf8_lossy(&completed.body),
        "1:1",
        "an awaited continuation completed within its own request must run once"
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
async fn a_dormant_interpreted_promise_can_be_signalled_by_a_later_active_request() {
    check_dormant_promise(GuestRuntime::Mono).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interpreted_promise_can_resume_work_within_its_own_request() {
    check_same_request_promise(GuestRuntime::Mono).await;
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
