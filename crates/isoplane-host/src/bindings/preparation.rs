//! Preparation uses the same bounded native I/O and egress enforcement as cells.

use std::sync::Arc;

use isoplane_abi::native_io::{Error, PreparedResources, Request, Response};
use isoplane_abi::result::ResultRecord;
use parking_lot::Mutex;
use tokio::sync::Notify;

use super::native_io::{Context, ResourceCount, ResourceLimits, Resources, Runtime};
use super::WakeSink;
use crate::egress::{ConnectionOwner, HostEgressPolicy};
use crate::futures::FutureTable;
use crate::metrics::Metrics;
use crate::InstanceId;

/// Caller-established authority and budgets for one initialization run.
pub struct PreparationIoConfig {
    /// Tenant and instance attributed to all outbound connections.
    pub owner: ConnectionOwner,
    /// Host policy shared with normal application execution.
    pub egress: Arc<HostEgressPolicy>,
    /// Application destination restrictions, intersected with host policy.
    pub allowlist: Option<Vec<String>>,
    /// Maximum outstanding native operations.
    pub pending_futures: u32,
    /// Maximum retained native handles and resolved addresses.
    pub native_handles: usize,
    /// Maximum buffered future result bytes.
    pub buffered_bytes: usize,
}

struct PreparationWake(Notify);

impl WakeSink for PreparationWake {
    fn signal(&self, _owner: InstanceId) {
        self.0.notify_one();
    }
}

/// Owns preparation's reactor work until disposal or successful capture.
pub struct PreparationIo {
    config: PreparationIoConfig,
    resources: Mutex<Resources>,
    runtime: Runtime,
    wake: Arc<PreparationWake>,
}

impl PreparationIo {
    /// Bind initialization to a supplied reactor and application egress policy.
    pub fn new(config: PreparationIoConfig, reactor: tokio::runtime::Handle) -> Self {
        let wake = Arc::new(PreparationWake(Notify::new()));
        let runtime = Runtime {
            egress: config.egress.clone(),
            futures: Arc::new(FutureTable::new(config.buffered_bytes)),
            metrics: Arc::new(Metrics::default()),
            wake: wake.clone(),
            reactor,
            usage: crate::reactor_accounting::TaskUsage::new(
                Arc::new(crate::meters::Meters::new()),
                crate::reactor_accounting::Attribution::default(),
            ),
        };
        runtime.futures.set_entry_limit(
            config.owner.instance(),
            crate::futures::retained_entry_limit(config.native_handles, config.pending_futures),
        );
        Self {
            resources: Mutex::new(Resources::new(ResourceLimits {
                handles: ResourceCount(config.native_handles),
            })),
            config,
            runtime,
            wake,
        }
    }

    /// Perform a nonblocking native operation through the production resource table.
    /// Fixes what the guest observes of timers for the slice it is entering.
    pub fn begin_slice(&self) {
        self.resources
            .lock()
            .begin_slice(super::native_io::reactor_now(&self.runtime));
    }

    pub fn call(&self, request: Request) -> Result<Response, Error> {
        self.resources.lock().call(request, self)
    }

    /// Wait for the next completion, or return None when no work remains.
    pub async fn next_completion(&self) -> Option<(u64, ResultRecord)> {
        loop {
            let notified = self.wake.0.notified();
            if let Some(result) = self
                .runtime
                .futures
                .take_ready(self.instance())
                .map(crate::futures::Ready::into_pair)
            {
                return Some(result);
            }
            if self.pending_count() == 0 {
                // A completion may arrive between the first ready check and the pending check.
                return self
                    .runtime
                    .futures
                    .take_ready(self.instance())
                    .map(crate::futures::Ready::into_pair);
            }
            notified.await;
        }
    }

    /// Active reactor-poll occupancy; time waiting for I/O is excluded.
    pub fn reactor_occupancy_ns(&self) -> u64 {
        self.runtime.usage.occupancy_ns()
    }

    /// Count operations whose reactor work has not completed.
    pub fn pending_count(&self) -> usize {
        self.runtime.futures.pending_total(self.instance())
    }

    /// Capture handle identity after all live I/O resources have been disposed.
    pub fn finish(&self) -> Result<PreparedResources, Error> {
        let resources = self.resources.lock();
        let prepared = resources.prepared(super::native_io::reactor_now(&self.runtime))?;
        if self.runtime.futures.holds_guest_work(self.instance()) {
            return Err(Error::InvalidState(
                "initialization has undelivered native completions",
            ));
        }
        Ok(prepared)
    }

    /// Restore captured handle identities before issuing native operations.
    pub fn restore_resources(&self, prepared: &PreparedResources) -> Result<(), Error> {
        self.resources
            .lock()
            .restore_prepared(prepared, super::native_io::reactor_now(&self.runtime))
    }
}

impl Context for PreparationIo {
    fn runtime(&self) -> Option<Runtime> {
        Some(self.runtime.clone())
    }
    fn tenant(&self) -> &str {
        self.config.owner.tenant()
    }
    fn instance(&self) -> InstanceId {
        self.config.owner.instance()
    }
    fn reach(&self) -> crate::egress::Reach<'_> {
        crate::egress::Reach {
            allowlist: self.config.allowlist.as_deref(),
            exceptions: self.config.owner.exceptions(),
        }
    }
    fn connection_owner(&self) -> std::io::Result<crate::egress::ConnectionOwner> {
        Ok(self.config.owner.clone())
    }
    fn alloc(&self, import: &'static str) -> Option<u64> {
        self.runtime
            .futures
            .alloc(
                self.instance(),
                0,
                import,
                self.config.pending_futures,
            )
            .ok()
    }
    fn alloc_rearmable(&self, import: &'static str) -> Option<u64> {
        self.runtime
            .futures
            .alloc_rearmable(
                self.instance(),
                0,
                import,
                self.config.pending_futures,
            )
            .ok()
    }
}

impl Drop for PreparationIo {
    fn drop(&mut self) {
        self.resources.get_mut().clear();
        self.runtime
            .futures
            .release_instance(self.config.owner.instance());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_abi::native_io::{AddressFamily, DurationNanos, ResourceHandle, ResourceKind};
    use tokio::io::AsyncReadExt;

    fn preparation(policy: HostEgressPolicy) -> PreparationIo {
        PreparationIo::new(
            PreparationIoConfig {
                owner: ConnectionOwner::new("preparation-test".into(), InstanceId(1)).unwrap(),
                egress: Arc::new(policy),
                allowlist: None,
                pending_futures: 4,
                native_handles: 16,
                buffered_bytes: 1024,
            },
            tokio::runtime::Handle::current(),
        )
    }

    fn handle(io: &PreparationIo, request: Request) -> ResourceHandle {
        match io.call(request).unwrap() {
            Response::Handle(handle) => handle,
            other => panic!("native resource allocation returned {other:?}"),
        }
    }

    fn dispose(io: &PreparationIo, handle: ResourceHandle, kind: ResourceKind) {
        io.call(Request::Drop { handle, kind }).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_initialization_timer_is_captured_and_counts_from_activation() {
        let policy = || {
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap()
        };
        let io = preparation(policy());
        let timer = handle(&io, Request::Timer(DurationNanos(1_000_000_000)));
        tokio::time::advance(std::time::Duration::from_millis(400)).await;
        let prepared = io
            .finish()
            .expect("step 1: capture must accept a pending timer");
        assert_eq!(
            prepared.pending_timers,
            vec![isoplane_abi::native_io::PreparedTimer {
                handle: timer,
                remaining_ms: 600,
            }],
            "step 1: capture must record the time the timer still has to run"
        );

        tokio::time::advance(std::time::Duration::from_secs(10)).await;
        let restored = preparation(policy());
        restored.restore_resources(&prepared).unwrap();
        // Each check is a new slice, as a guest re-entered after time passed.
        let ready = |io: &PreparationIo| {
            io.begin_slice();
            io.call(Request::Ready(timer))
        };
        let ready = |io: &PreparationIo| match ready(io) {
            Ok(Response::Ready(ready)) => ready,
            other => panic!("a readiness check returned {other:?}"),
        };
        assert!(
            !ready(&restored),
            "step 2: time before activation must not count toward the timer"
        );
        tokio::time::advance(std::time::Duration::from_millis(599)).await;
        assert!(
            !ready(&restored),
            "step 3: the timer is not due before 600 ms"
        );
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(
            ready(&restored),
            "step 3: the timer is due 600 ms after activation"
        );
    }

    #[tokio::test]
    async fn immediately_ready_pollables_survive_capture_without_creating_host_work() {
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        let sentinel = handle(&io, Request::Timer(DurationNanos(0)));
        let prepared = io
            .finish()
            .expect("an inert ready sentinel must be representable");
        assert_eq!(
            prepared.ready_pollables,
            vec![sentinel],
            "capture must record every guest-cached ready pollable identity"
        );
        let restored = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        restored.restore_resources(&prepared).unwrap();
        assert!(
            matches!(
                restored.call(Request::Ready(sentinel)),
                Ok(Response::Ready(true))
            ),
            "the captured sentinel must remain immediately ready after activation"
        );
        assert_eq!(
            restored.pending_count(),
            0,
            "restoring an inert pollable must not create a timer or wake source"
        );
        assert!(
            matches!(restored.call(Request::Poll(vec![sentinel])), Ok(Response::ReadyIndexes(indexes)) if indexes == vec![0]),
            "the restored sentinel must satisfy the canonical poll operation"
        );
        dispose(&restored, sentinel, ResourceKind::Pollable);
        assert_eq!(
            handle(&restored, Request::Timer(DurationNanos(0))).0,
            prepared.next_handle,
            "disposing a captured sentinel must not permit its handle to be reused"
        );
    }

    #[tokio::test]
    async fn malformed_ready_pollable_metadata_cannot_alias_another_resource() {
        for ready_pollables in [
            vec![ResourceHandle(0)],
            vec![ResourceHandle(3)],
            vec![ResourceHandle(1)],
            vec![ResourceHandle(2), ResourceHandle(2)],
        ] {
            let io = preparation(
                HostEgressPolicy::system(
                    crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
                )
                .unwrap(),
            );
            let prepared = PreparedResources {
                next_handle: 3,
                network: Some(ResourceHandle(1)),
                ready_pollables,
                pending_timers: Vec::new(),
            };
            assert!(io.restore_resources(&prepared).is_err(), "ready pollable metadata must contain only distinct in-range handles separate from network authority");
        }
    }

    #[tokio::test]
    async fn captured_ready_pollables_consume_the_activation_handle_budget() {
        let mut io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        io.resources = Mutex::new(Resources::new(ResourceLimits {
            handles: ResourceCount(1),
        }));
        let prepared = PreparedResources {
            next_handle: 3,
            network: Some(ResourceHandle(1)),
            ready_pollables: vec![ResourceHandle(2)],
            pending_timers: Vec::new(),
        };
        assert!(
            matches!(io.restore_resources(&prepared), Err(Error::ResourceLimit)),
            "network and captured ready pollables must each consume an activation handle slot"
        );
        assert!(
            io.finish().is_ok(),
            "a refused restoration must leave no partially installed resources"
        );
    }

    #[tokio::test]
    async fn prepared_network_identity_and_retired_handles_survive_restoration() {
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        let retired = handle(&io, Request::Timer(DurationNanos(0)));
        dispose(&io, retired, ResourceKind::Pollable);
        let network = handle(&io, Request::Network);
        let prepared = io.finish().unwrap();
        let restored = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        restored.restore_resources(&prepared).unwrap();
        assert_eq!(
            handle(&restored, Request::Network),
            network,
            "restored guest network IDs must retain their identity"
        );
        let timer = handle(&restored, Request::Timer(DurationNanos(0)));
        assert_eq!(
            timer.0, prepared.next_handle,
            "activation must continue the captured handle sequence"
        );
        assert!(
            matches!(
                restored.call(Request::Ready(retired)),
                Err(Error::InvalidHandle(_))
            ),
            "a retired preparation handle must never resolve after activation"
        );
    }

    #[tokio::test]
    async fn preparation_refuses_capture_with_an_undelivered_completion() {
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        let future = io
            .alloc(
                isoplane_abi::native_io::canonical::POLL_IMPORT,
            )
            .unwrap();
        io.runtime
            .settle_future(future, ResultRecord::Ok(Vec::new()));
        assert!(
            io.finish().is_err(),
            "capture must not abandon a completion the guest has not consumed"
        );
        assert!(
            io.next_completion().await.is_some(),
            "the retained completion must remain deliverable"
        );
        assert!(
            io.finish().is_ok(),
            "draining the completion must permit capture"
        );
    }

    #[tokio::test]
    async fn preparation_enforces_the_supplied_pending_future_budget() {
        let mut io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        io.config.pending_futures = 1;
        for _ in 0..2 {
            assert!(
                matches!(
                    io.call(Request::Timer(DurationNanos(1_000_000))),
                    Ok(Response::Handle(_))
                ),
                "preparation accepts the timer and defers its wake past the budget"
            );
        }
        assert_eq!(
            io.runtime.futures.pending_total(io.instance()),
            1,
            "the budget bounds the futures preparation holds for the guest"
        );
    }

    #[tokio::test]
    async fn restoring_prepared_resources_refuses_malformed_or_reused_tables() {
        for prepared in [
            PreparedResources {
                ready_pollables: Vec::new(),
                pending_timers: Vec::new(),

                next_handle: 0,
                network: None,
            },
            PreparedResources {
                ready_pollables: Vec::new(),
                pending_timers: Vec::new(),

                next_handle: 2,
                network: Some(ResourceHandle(0)),
            },
            PreparedResources {
                ready_pollables: Vec::new(),
                pending_timers: Vec::new(),

                next_handle: 2,
                network: Some(ResourceHandle(2)),
            },
        ] {
            let io = preparation(
                HostEgressPolicy::system(
                    crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
                )
                .unwrap(),
            );
            assert!(
                io.restore_resources(&prepared).is_err(),
                "invalid prepared handles must not gain authority"
            );
        }
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        let retired = handle(&io, Request::Timer(DurationNanos(0)));
        dispose(&io, retired, ResourceKind::Pollable);
        assert!(
            io.restore_resources(&PreparedResources::default()).is_err(),
            "restoring over a used table must not reset retired handle identities"
        );
    }

    #[tokio::test]
    async fn retained_native_resources_refuse_a_different_template_baseline() {
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        io.restore_resources(&PreparedResources {
            ready_pollables: Vec::new(),
            pending_timers: Vec::new(),

            next_handle: 2,
            network: Some(ResourceHandle(1)),
        })
        .unwrap();
        assert!(
            io.restore_resources(&PreparedResources::default()).is_err(),
            "a retained resource table must not accept a different template's authority"
        );
    }

    #[tokio::test]
    async fn restored_network_handles_consume_the_native_handle_budget() {
        let mut io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        io.resources = Mutex::new(Resources::new(ResourceLimits {
            handles: ResourceCount(0),
        }));
        assert!(
            matches!(
                io.restore_resources(&PreparedResources {
                    ready_pollables: Vec::new(),
                    pending_timers: Vec::new(),

                    next_handle: 2,
                    network: Some(ResourceHandle(1))
                }),
                Err(Error::ResourceLimit)
            ),
            "restoring network authority must enforce the activation resource budget"
        );
    }

    struct Endpoint(std::net::SocketAddr);
    impl crate::egress::DestinationPolicy for Endpoint {
        fn authorize(
            &self,
            address: std::net::SocketAddr,
        ) -> Result<(), crate::egress::DenyReason> {
            if address == self.0 {
                Ok(())
            } else {
                Err(crate::egress::DenyReason::NotAllowlisted(
                    address.to_string(),
                ))
            }
        }
    }

    #[tokio::test]
    async fn preparation_connections_use_real_tcp_and_close_before_capture() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let destination = listener.local_addr().unwrap();
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap()
            .with_destinations(Arc::new(Endpoint(destination))),
        );
        let network = handle(&io, Request::Network);
        let socket = handle(&io, Request::CreateTcp(AddressFamily::V4));
        io.call(Request::Connect {
            socket,
            network,
            destination,
        })
        .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        assert!(
            io.next_completion().await.is_some(),
            "the real connection must complete through preparation's reactor"
        );
        let Response::Streams { input, output } = io.call(Request::FinishConnect(socket)).unwrap()
        else {
            panic!("a connected socket must expose streams")
        };
        assert!(
            io.finish().is_err(),
            "capture must reject a retained connection"
        );
        dispose(&io, input, ResourceKind::Input);
        dispose(&io, output, ResourceKind::Output);
        dispose(&io, socket, ResourceKind::Tcp);
        assert!(
            io.finish().is_ok(),
            "closing all connection resources must permit capture"
        );
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), peer.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            bytes, 0,
            "native connection disposal must close the real peer before capture"
        );
    }

    #[tokio::test]
    async fn preparation_obeys_application_allowlists_even_for_host_admitted_endpoints() {
        let destination = "127.0.0.1:4321".parse().unwrap();
        let mut io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap()
            .with_destinations(Arc::new(Endpoint(destination))),
        );
        io.config.allowlist = Some(vec!["example.com".into()]);
        let network = handle(&io, Request::Network);
        let socket = handle(&io, Request::CreateTcp(AddressFamily::V4));
        assert!(
            matches!(
                io.call(Request::Connect {
                    socket,
                    network,
                    destination
                }),
                Err(Error::Denied(_))
            ),
            "preparation must intersect host policy with the application's allowlist"
        );
        assert_eq!(
            io.pending_count(),
            0,
            "an egress refusal must not start reactor work"
        );
    }

    #[tokio::test]
    async fn dropping_preparation_cancels_its_pending_reactor_work() {
        let io = preparation(
            HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(4).expect("connection limits"),
            )
            .unwrap(),
        );
        handle(&io, Request::Timer(DurationNanos(u64::MAX)));
        let futures = io.runtime.futures.clone();
        drop(io);
        assert_eq!(
            futures.live_futures(),
            0,
            "failed preparation must release every pending native future"
        );
    }
}
