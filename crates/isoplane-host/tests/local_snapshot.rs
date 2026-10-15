//! Real guest continuations through physically reserved local snapshots.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use isoplane_abi::config::{
    AppManifest, Bindings, GuestMemoryBytes, HandlerConfig, HandlerLifecycle, Limits,
};
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_fixtures::GuestRuntime;
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_hibernate::snapshot::{
    LocalSnapshotStorage, SnapshotBytes, SnapshotError, SnapshotReservation, SnapshotStorage,
};
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, ReclaimKind, Services, Target};
use isoplane_host::egress::{
    ConnectAttempt, ConnectionError, ConnectionFactory, ConnectionOwner, DenyReason,
    DestinationPolicy, HostEgressPolicy, LocalConnections,
};
use isoplane_host::fakes::FakeFetcher;
use isoplane_host::{EngineActivator, InstanceId, LifecycleTier, ModuleId, Resolver};

#[path = "common/managed.rs"]
mod managed;

const DEADLINE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct StorageCounts {
    live: AtomicUsize,
    reserved_bytes: AtomicU64,
    saves: AtomicUsize,
    loads: AtomicUsize,
}

struct ObservedStorage {
    inner: LocalSnapshotStorage,
    counts: Arc<StorageCounts>,
}

impl SnapshotStorage for ObservedStorage {
    fn reserve(&self, bytes: SnapshotBytes) -> Result<Box<dyn SnapshotReservation>, SnapshotError> {
        let inner = self.inner.reserve(bytes)?;
        self.counts.live.fetch_add(1, Ordering::SeqCst);
        self.counts.reserved_bytes.store(bytes.0, Ordering::SeqCst);
        Ok(Box::new(ObservedReservation {
            inner,
            counts: self.counts.clone(),
        }))
    }
}

struct ObservedReservation {
    inner: Box<dyn SnapshotReservation>,
    counts: Arc<StorageCounts>,
}

impl SnapshotReservation for ObservedReservation {
    fn reserved_bytes(&self) -> SnapshotBytes {
        self.inner.reserved_bytes()
    }
    fn save(&mut self, parts: &[std::io::IoSlice<'_>]) -> Result<(), SnapshotError> {
        self.inner.save(parts)?;
        self.counts.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn read_exact_at(
        &mut self,
        offset: u64,
        parts: &mut [std::io::IoSliceMut<'_>],
    ) -> Result<(), SnapshotError> {
        self.inner.read_exact_at(offset, parts)?;
        // A restore reads the snapshot from its start once, then its pages.
        if offset == 0 {
            self.counts.loads.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Drop for ObservedReservation {
    fn drop(&mut self) {
        self.counts.live.fetch_sub(1, Ordering::SeqCst);
    }
}

struct NoDns;
#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> Result<Vec<SocketAddr>, isoplane_host::egress::DnsError> {
        panic!("literal endpoint probes must not resolve DNS")
    }
}

struct ExactPeer(SocketAddr);
impl DestinationPolicy for ExactPeer {
    fn authorize(&self, peer: SocketAddr) -> Result<(), isoplane_host::egress::DenyReason> {
        if peer == self.0 {
            Ok(())
        } else {
            Err(isoplane_host::egress::DenyReason::NotAllowlisted(
                peer.to_string(),
            ))
        }
    }
}

struct ConnectGate {
    established: mpsc::SyncSender<InstanceId>,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ConnectionFactory for ConnectGate {
    async fn connect(
        &self,
        owner: &ConnectionOwner,
        peer: SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        let stream = LocalConnections.connect(owner, peer).await?;
        self.established
            .send(owner.instance())
            .expect("test must await the connection barrier");
        self.release.notified().await;
        Ok(stream)
    }
}

struct Rig {
    cell: Arc<Cell>,
    module: ModuleId,
    counts: Arc<StorageCounts>,
    _directory: tempfile::TempDir,
}

impl Rig {
    fn new(wasm: &[u8], max_memory_bytes: u64, egress: Arc<HostEgressPolicy>) -> Self {
        let engine = Arc::new(
            IsoplaneEngine::new(EngineConfig {
                max_instances: 2,
                max_memory_bytes,
                guard_size_bytes: 64 << 20,
                ..Default::default()
            })
            .expect("real engine must start"),
        );
        let template = engine.load_template(wasm).expect("real guest must load");
        Self::serving(engine, template, max_memory_bytes, egress)
    }

    /// The shared runtime Task compiled, serving `files`.
    fn shared_runtime(
        egress: Arc<HostEgressPolicy>,
        files: isoplane_engine::PlatformFiles,
    ) -> Self {
        let config = isoplane_fixtures::engine_config(2);
        let max_memory_bytes = config.max_memory_bytes;
        let engine = Arc::new(IsoplaneEngine::new(config).expect("real engine must start"));
        let template = isoplane_fixtures::load_compiled_runtime(&engine)
            .with_application_files(files)
            .expect("real guest must load");
        Self::serving(engine, template, max_memory_bytes, egress)
    }

    fn serving(
        engine: Arc<IsoplaneEngine>,
        template: isoplane_engine::Template,
        max_memory_bytes: u64,
        egress: Arc<HostEgressPolicy>,
    ) -> Self {
        let directory = tempfile::tempdir().expect("each test owns a snapshot directory");
        let counts = Arc::new(StorageCounts::default());
        let snapshots = Arc::new(ObservedStorage {
            inner: LocalSnapshotStorage::new(directory.path())
                .expect("open local snapshot storage"),
            counts: counts.clone(),
        });
        let cell = Cell::with_snapshot_storage(
            CellConfig {
                exec_threads: 1,
                max_idle_per_module: 1,
                wall_ms_cap: 60_000,
                hibernate: isoplane_host::hibernate_policy::HibernateConfig {
                    handler_after_ms: 0,
                    ..Default::default()
                },
                ..CellConfig::for_test("local-snapshot")
            },
            Arc::new(Services {
                egress,
                fetcher: Arc::new(FakeFetcher::default()),
            }),
            snapshots,
        )
        .expect("cell must start with real snapshots");
        cell.set_node_coordinated_reclamation();
        let module = ModuleId::new("snapshot-probe", "v1");
        let activator = Arc::new(EngineActivator::new(engine, template, module.clone()));
        cell.register_module(
            "snapshot-test",
            activator,
            AppManifest {
                name: "snapshot-probe".into(),
                guest_memory_bytes: Some(
                    GuestMemoryBytes::new(max_memory_bytes).expect("guest ceiling must be valid"),
                ),
                resource_limits: Default::default(),
                validation_limits: Default::default(),
                handlers: HandlerConfig {
                    lifecycle: HandlerLifecycle::Reuse,
                },
                bindings: Bindings::default(),
                limits: Limits::default(),
                init_visible_vars: vec![],
            },
        )
        .expect("guest must register");
        Self {
            cell,
            module,
            counts,
            _directory: directory,
        }
    }

    fn submit(&self, path: &str) -> mpsc::Receiver<DispatchResult> {
        let request = isoplane_abi::HttpRequest {
            method: "GET".into(),
            url: format!("http://snapshot.test{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        let mut record = self.cell.http_record(&self.module, request);
        record.occupancy_budget_ms = 30_000;
        self.cell
            .submit(&self.module, Target::Handler { route: path.into() }, record)
            .expect("probe dispatch must be queued")
    }

    fn reclaim(&self, instance: InstanceId, kind: ReclaimKind) {
        wait_until(
            || {
                self.cell
                    .reclamation_candidates(isoplane_host::now_ms())
                    .into_iter()
                    .find(|candidate| candidate.instance == instance && candidate.kind == kind)
                    .is_some_and(|candidate| {
                        self.cell
                            .reclaim_candidate(candidate)
                            .expect("real capture must succeed")
                    })
            },
            "the returned guest must become reclaimable",
        );
        assert_eq!(
            self.cell.instance_state(instance),
            Some(LifecycleTier::Hibernated),
            "reclamation must release the live guest Store"
        );
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.cell.shutdown();
    }
}

fn wait_until(mut check: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + DEADLINE;
    while !check() {
        assert!(Instant::now() < deadline, "hung test: {message}");
        std::thread::yield_now();
    }
}

#[test]
fn growing_to_the_memory_ceiling_preserves_the_final_page_through_local_reclamation() {
    let wasm = wat::parse_str(r#"(module
        (memory (export "memory") 1)
        (global $phase (export "phase") (mut i32) (i32.const 0))
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (if (i32.eqz (global.get $phase))
                (then
                    (if (i32.ne (memory.grow (i32.const 1)) (i32.const 1)) (then unreachable))
                    (i32.store (i32.const 131068) (i32.const 305419896))
                    (global.set $phase (i32.const 1)))
                (else
                    (if (i32.ne (memory.size) (i32.const 2)) (then unreachable))
                    (if (i32.ne (i32.load (i32.const 131068)) (i32.const 305419896)) (then unreachable))))
            i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#)
        .expect("growth probe must parse");
    let rig = Rig::new(
        &wasm,
        2 * isoplane_abi::WASM_PAGE_SIZE as u64,
        Arc::new(HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )),
    );
    let first = rig
        .submit("/")
        .recv_timeout(DEADLINE)
        .expect("first dispatch must finish");
    assert_eq!(
        first.completion,
        Completion::Done,
        "guest must grow to its ceiling"
    );
    rig.reclaim(first.instance, ReclaimKind::Idle);
    let second = rig
        .submit("/")
        .recv_timeout(DEADLINE)
        .expect("snapshot acquisition must finish");
    assert_eq!(
        second.completion,
        Completion::Done,
        "woken guest must retain its grown final-page marker"
    );
    assert_eq!(
        second.acquired, "hibernation_wake",
        "next request must restore its locally saved Store"
    );
    assert_eq!(
        second.instance, first.instance,
        "restoration must preserve the pooled identity"
    );
    assert_eq!(
        rig.counts.saves.load(Ordering::SeqCst),
        1,
        "reclamation must write one real snapshot"
    );
    assert_eq!(
        rig.counts.loads.load(Ordering::SeqCst),
        1,
        "acquisition must read the reserved snapshot"
    );
    assert!(
        rig.counts.reserved_bytes.load(Ordering::SeqCst) > 2 * isoplane_abi::WASM_PAGE_SIZE as u64,
        "admission must reserve maximum memory plus snapshot metadata"
    );
    wait_until(
        || rig.cell.discard_instance(first.instance).is_ok(),
        "completed guest must reach its disposable boundary",
    );
    assert_eq!(
        rig.counts.live.load(Ordering::SeqCst),
        0,
        "discard must release the physical reservation"
    );
}

/// Begins a connection for `instance`, which must be admitted or refused without waiting.
fn begin_now(
    policy: &Arc<HostEgressPolicy>,
    instance: InstanceId,
) -> Result<ConnectAttempt, ConnectionError> {
    let owner = ConnectionOwner::new("snapshot-test".into(), instance).expect("fixture owner");
    let mut attempt = std::pin::pin!(policy.begin_connect(owner));
    match std::future::Future::poll(
        attempt.as_mut(),
        &mut std::task::Context::from_waker(std::task::Waker::noop()),
    ) {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => panic!("instance {instance} has no connection in progress"),
    }
}

fn managed_stream_snapshot(runtime: GuestRuntime) {
    isoplane_harness::LogCapture::global();
    let GuestRuntime::Mono = runtime;
    let listener =
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("owned peer must bind");
    let peer = listener
        .local_addr()
        .expect("owned peer address must be known");
    let (established, connected) = mpsc::sync_channel(1);
    let connections = Arc::new(ConnectGate {
        established,
        release: tokio::sync::Notify::new(),
    });
    let policy = Arc::new(
        HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )
        .with_connections(connections.clone())
        .with_destinations(Arc::new(ExactPeer(peer))),
    );
    let rig = Rig::shared_runtime(
        policy.clone(),
        managed::files("isoplane_guest_dotnet_native_io"),
    );
    let response = rig.submit(&format!("/stream/{}/{}", peer.ip(), peer.port()));
    let instance = connected
        .recv_timeout(DEADLINE)
        .expect("real TCP connection must reach the controlled barrier");
    let (mut server, _) = listener
        .accept()
        .expect("owned peer must accept the established connection");
    server
        .set_read_timeout(Some(DEADLINE))
        .expect("peer reads must not hang the test");
    server
        .set_write_timeout(Some(DEADLINE))
        .expect("peer writes must not hang the test");
    let connect_futures = rig.cell.futures().live_guest_future_ids();
    assert_eq!(
        connect_futures.len(),
        1,
        "ConnectAsync must own one pending host future"
    );
    connections.release.notify_one();
    wait_until(
        || {
            let futures = rig.cell.futures().live_guest_future_ids();
            futures.len() == 1
                && futures != connect_futures
                && !rig.cell.futures().has_ready(instance)
                && rig.cell.instance_state(instance) == Some(LifecycleTier::Parked)
        },
        "ConnectAsync must finish and ReadExactlyAsync must park on a new host future",
    );
    let read_futures = rig.cell.futures().live_guest_future_ids();
    assert_eq!(
        policy.instance_connections(instance),
        1,
        "pending read must retain its real socket's open permit"
    );
    rig.reclaim(instance, ReclaimKind::Active);
    assert_eq!(
        rig.cell.futures().live_guest_future_ids(),
        read_futures,
        "local capture must retain the pending native read in its original host table"
    );
    assert_eq!(
        policy.instance_connections(instance),
        1,
        "the hibernated instance's host-owned socket must stay open and keep counting"
    );
    assert!(
        matches!(
            begin_now(&policy, instance),
            Err(ConnectionError::Denied(DenyReason::OpenConnectionLimit { instance: refused, limit: 1, .. }))
                if refused == instance
        ),
        "the hibernated instance is still at its open connection limit"
    );
    let other = InstanceId::alloc();
    assert!(
        begin_now(&policy, other).is_ok(),
        "another instance is not limited by the hibernated instance's connections"
    );
    assert_eq!(
        policy.instance_connections(other),
        0,
        "the other instance's dropped attempt returns its permit"
    );
    server
        .write_all(b"ping")
        .expect("peer must deliver bytes after guest hibernation");
    let result = response.recv_timeout(DEADLINE).unwrap_or_else(|error| {
        panic!(
            "{runtime} continuation must finish after native read completion: {error}; logs: {:?}",
            rig.cell.slot(instance).map(|slot| slot.ctx.emitted_logs())
        )
    });
    assert_eq!(
        result.status, 200,
        "managed stream must complete through the restored continuation"
    );
    assert_eq!(
        result.body, b"transferred",
        "managed async continuation must finish after local snapshot restoration"
    );
    let mut echoed = [0; 4];
    server
        .read_exact(&mut echoed)
        .expect("restored NetworkStream must write its received bytes back");
    assert_eq!(
        &echoed, b"ping",
        "restored socket must preserve the peer payload"
    );
    let mut eof = [0];
    assert_eq!(
        server
            .read(&mut eof)
            .expect("peer must observe guest disposal"),
        0,
        "managed socket disposal must close the retained host descriptor"
    );
    wait_until(
        || {
            policy.instance_connections(instance) == 0
                && rig.cell.futures().live_guest_future_ids().is_empty()
        },
        "terminal completion must release native connection quota and pending work",
    );
    assert_eq!(
        rig.counts.saves.load(Ordering::SeqCst),
        1,
        "pending native read must write a real local snapshot"
    );
    assert_eq!(
        rig.counts.loads.load(Ordering::SeqCst),
        1,
        "read completion must restore the saved guest"
    );
    wait_until(
        || rig.cell.discard_instance(instance).is_ok(),
        "completed guest must release its cached snapshot",
    );
    assert_eq!(
        rig.counts.live.load(Ordering::SeqCst),
        0,
        "terminal discard must release reserved disk blocks"
    );
}

#[test]
fn interpreted_tcp_read_survives_local_snapshot_reclamation() {
    managed_stream_snapshot(GuestRuntime::Mono);
}
