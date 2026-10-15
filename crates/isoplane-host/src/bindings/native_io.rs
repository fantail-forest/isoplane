//! Instance-owned native networking resources, independent of the Wasmtime Store.

mod addresses;
mod buffer_sizes;
mod keep_alive;
mod streams;

use addresses::Addresses;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use streams::{Input, Output};

use isoplane_abi::native_io::{
    AddressFamily, ByteCount, Error, KeepAlive, KeepAliveChange, Request, ResourceHandle,
    ResourceKind, Response, SocketBuffer,
};
use isoplane_abi::result::ResultRecord;
use parking_lot::Mutex;
use tokio::net::TcpStream;

#[cfg(test)]
use super::CellBindings;
mod context;
use crate::egress::ConnectionPermit;
use crate::futures::{FutureId, FutureTable};
use crate::InstanceId;
pub(crate) use context::{Context, Runtime};

fn report_failure(bindings: &dyn Context, error: &Error) {
    if matches!(error, Error::Denied(_)) {
        if let Some(runtime) = bindings.runtime() {
            runtime
                .metrics
                .egress_denials
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    tracing::warn!(%error, "native I/O operation failed");
}

struct ReactorWork {
    future: FutureId,
    owner: InstanceId,
    table: Arc<FutureTable>,
    task: tokio::task::AbortHandle,
}

impl ReactorWork {
    fn is_pending(&self) -> bool {
        self.table.is_pending(self.owner, self.future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_native_handle_budget_is_enforced_by_instance_resources() {
        for limit in [0, 2] {
            let mut context = super::super::tests::ctx();
            Arc::get_mut(&mut context)
                .expect("new context has no other owner")
                .configure_resources(super::super::GuestResourceLimits {
                    native_handles: limit,
                    ..Default::default()
                });
            let bindings = CellBindings::new(context.clone());
            let mut resources = context.native_io.lock();
            for _ in 0..limit {
                assert!(
                    resources
                        .call(Request::CreateTcp(AddressFamily::V4), &bindings)
                        .is_ok(),
                    "configured native slots must be available"
                );
            }
            assert!(
                resources
                    .call(Request::CreateTcp(AddressFamily::V4), &bindings)
                    .is_err(),
                "the next native handle must be refused"
            );
        }
    }

    fn resources(handles: usize) -> (Resources, CellBindings) {
        (
            Resources::new(ResourceLimits {
                handles: ResourceCount(handles),
            }),
            CellBindings::new(super::super::tests::ctx()),
        )
    }

    fn created(resources: &mut Resources, bindings: &CellBindings) -> ResourceHandle {
        match resources
            .call(Request::CreateTcp(AddressFamily::V4), bindings)
            .expect("an available native resource slot must accept a socket")
        {
            Response::Handle(handle) => handle,
            result => panic!("socket creation returned an unexpected result: {result:?}"),
        }
    }

    struct EndpointPeer(std::net::SocketAddr);

    impl crate::egress::DestinationPolicy for EndpointPeer {
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

    /// An instance's socket connected to an owned peer, with the host's
    /// stream beneath it.
    struct Connected {
        resources: Resources,
        bindings: CellBindings,
        socket: ResourceHandle,
        stream: Arc<TcpStream>,
        remote: std::net::SocketAddr,
        local: std::net::SocketAddr,
        _accepted: TcpStream,
    }

    async fn connected(family: AddressFamily) -> Connected {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let remote = listener.local_addr().unwrap();
        let policy = Arc::new(
            crate::egress::HostEgressPolicy::system(
                crate::egress::ConnectionLimits::uniform(1).expect("connection limits"),
            )
            .unwrap()
            .with_destinations(Arc::new(EndpointPeer(remote))),
        );
        let attempt = policy
            .begin_connect(
                crate::egress::ConnectionOwner::new("tenant".into(), InstanceId(1)).unwrap(),
            )
            .await
            .unwrap();
        let stream = Arc::new(attempt.connect(remote).await.unwrap());
        let permit = attempt.established();
        let (accepted, local) = listener.accept().await.unwrap();
        let (mut resources, bindings) = resources(1);
        let socket = resources
            .insert(Resource::Socket(Socket {
                family,
                no_delay: Arc::new(AtomicBool::new(false)),
                keep_alive: Arc::default(),
                buffer_sizes: Arc::default(),
                streams_taken: false,
                state: Arc::new(Mutex::new(SocketState::Connected(Arc::new(Connection {
                    stream: stream.clone(),
                    _permit: permit,
                })))),
                task: None,
            }))
            .unwrap();
        Connected {
            resources,
            bindings,
            socket,
            stream,
            remote,
            local,
            _accepted: accepted,
        }
    }

    async fn connected_endpoints(family: AddressFamily) {
        let Connected {
            mut resources,
            bindings,
            socket,
            remote,
            local,
            _accepted,
            ..
        } = connected(family).await;
        for (peer, native) in [(true, remote), (false, local)] {
            let result = resources
                .call(Request::SocketAddress { socket, peer }, &bindings)
                .unwrap();
            let Response::SocketAddress(address) = result else {
                panic!("socket endpoint query returned {result:?}")
            };
            let expected = match family {
                AddressFamily::V4 => native,
                AddressFamily::V6 => {
                    let std::net::IpAddr::V4(ip) = native.ip() else {
                        panic!("test peer must be IPv4")
                    };
                    std::net::SocketAddr::new(ip.to_ipv6_mapped().into(), native.port())
                }
            };
            assert_eq!(
                address, expected,
                "guest endpoints must preserve the logical socket family and actual peer identity"
            );
        }
    }

    #[tokio::test]
    async fn ipv6_guest_endpoints_preserve_mapped_addresses_over_native_ipv4() {
        connected_endpoints(AddressFamily::V6).await;
    }

    #[tokio::test]
    async fn ipv4_guest_endpoints_preserve_native_addresses() {
        connected_endpoints(AddressFamily::V4).await;
    }

    fn keep_alive(
        resources: &mut Resources,
        bindings: &CellBindings,
        socket: ResourceHandle,
        change: Option<KeepAliveChange>,
    ) -> Result<KeepAlive, Error> {
        match resources.call(Request::KeepAlive { socket, change }, bindings)? {
            Response::KeepAlive(effective) => Ok(effective),
            response => panic!("a keep-alive request returned {response:?}"),
        }
    }

    const NANOS_PER_SEC: u64 = 1_000_000_000;

    #[tokio::test]
    async fn keep_alive_requested_on_a_connected_socket_reaches_the_host_socket_clamped() {
        use isoplane_abi::native_io::{DurationNanos, ProbeCount};
        let Connected {
            mut resources,
            bindings,
            socket,
            stream,
            _accepted,
            ..
        } = connected(AddressFamily::V4).await;
        let host = socket2::SockRef::from(stream.as_ref());
        assert!(
            !host.keepalive().unwrap(),
            "the host socket must start without keep-alive"
        );
        for change in [
            KeepAliveChange::IdleTime(DurationNanos(NANOS_PER_SEC)),
            KeepAliveChange::Interval(DurationNanos(5 * NANOS_PER_SEC)),
            KeepAliveChange::Count(ProbeCount(1_000)),
        ] {
            keep_alive(&mut resources, &bindings, socket, Some(change))
                .expect("a nonzero timing must be accepted");
        }
        assert!(
            !host.keepalive().unwrap(),
            "timings alone must not start probing"
        );
        let effective = keep_alive(
            &mut resources,
            &bindings,
            socket,
            Some(KeepAliveChange::Enabled(true)),
        )
        .expect("enabling keep-alive must succeed");
        assert_eq!(
            effective,
            KeepAlive {
                enabled: true,
                idle_time: DurationNanos(10 * NANOS_PER_SEC),
                interval: DurationNanos(5 * NANOS_PER_SEC),
                count: ProbeCount(127),
            },
            "the guest must read back the clamped settings"
        );
        assert!(host.keepalive().unwrap(), "the host socket must probe");
        // socket2 reads a socket's timings back on Unix only.
        #[cfg(unix)]
        assert_eq!(
            (
                host.tcp_keepalive_time().unwrap(),
                host.tcp_keepalive_interval().unwrap()
            ),
            (
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(5)
            )
        );
        assert_eq!(host.tcp_keepalive_retries().unwrap(), 127);

        assert!(
            matches!(
                keep_alive(
                    &mut resources,
                    &bindings,
                    socket,
                    Some(KeepAliveChange::Count(ProbeCount(0)))
                ),
                Err(Error::InvalidArgument(_))
            ),
            "a zero probe count must be refused"
        );
        assert_eq!(
            host.tcp_keepalive_retries().unwrap(),
            127,
            "a refused change must leave the host socket as it was"
        );
        assert_eq!(
            keep_alive(&mut resources, &bindings, socket, None).unwrap(),
            effective,
            "a refused change must leave the settings the guest reads as they were"
        );

        keep_alive(
            &mut resources,
            &bindings,
            socket,
            Some(KeepAliveChange::Enabled(false)),
        )
        .expect("disabling keep-alive must succeed");
        assert!(
            !host.keepalive().unwrap(),
            "disabling must stop the host socket probing"
        );
    }

    #[test]
    fn keep_alive_requested_before_connecting_is_held_for_the_connection() {
        use isoplane_abi::native_io::DurationNanos;
        let (mut resources, bindings) = resources(1);
        let socket = created(&mut resources, &bindings);
        let initial = keep_alive(&mut resources, &bindings, socket, None)
            .expect("an unconnected socket must report its settings");
        assert!(!initial.enabled, "keep-alive must start disabled");
        for change in [
            KeepAliveChange::Enabled(true),
            KeepAliveChange::IdleTime(DurationNanos(NANOS_PER_SEC)),
        ] {
            keep_alive(&mut resources, &bindings, socket, Some(change))
                .expect("an unconnected socket must accept keep-alive settings");
        }
        assert_eq!(
            keep_alive(&mut resources, &bindings, socket, None).unwrap(),
            KeepAlive {
                enabled: true,
                idle_time: DurationNanos(10 * NANOS_PER_SEC),
                ..initial
            },
            "the socket must hold the clamped settings until it connects"
        );
    }

    fn buffer_size(
        resources: &mut Resources,
        bindings: &CellBindings,
        socket: ResourceHandle,
        buffer: SocketBuffer,
        requested: Option<u64>,
    ) -> Result<ByteCount, Error> {
        let request = Request::BufferSize {
            socket,
            buffer,
            requested: requested.map(ByteCount),
        };
        match resources.call(request, bindings)? {
            Response::BufferSize(size) => Ok(size),
            response => panic!("a buffer-size request returned {response:?}"),
        }
    }

    /// The receive and send sizes this host OS reports for a socket given
    /// `bytes` for each, which Linux, unlike other systems, doubles.
    fn as_the_kernel_reports(bytes: usize) -> (usize, usize) {
        let reference = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
            .expect("a reference socket must open");
        reference.set_recv_buffer_size(bytes).unwrap();
        reference.set_send_buffer_size(bytes).unwrap();
        (
            reference.recv_buffer_size().unwrap(),
            reference.send_buffer_size().unwrap(),
        )
    }

    #[tokio::test]
    async fn a_buffer_size_requested_on_a_connected_socket_reaches_the_host_socket_clamped() {
        let Connected {
            mut resources,
            bindings,
            socket,
            stream,
            _accepted,
            ..
        } = connected(AddressFamily::V4).await;
        let host = socket2::SockRef::from(stream.as_ref());
        let kernel_send_bytes = host.send_buffer_size().unwrap();

        assert_eq!(
            buffer_size(
                &mut resources,
                &bindings,
                socket,
                SocketBuffer::Receive,
                Some(1)
            ),
            Ok(ByteCount(4_096)),
            "the guest must read back a one-byte receive buffer raised to the floor"
        );
        let (floor_receive_bytes, _) = as_the_kernel_reports(4_096);
        assert_eq!(host.recv_buffer_size().unwrap(), floor_receive_bytes);
        assert_eq!(
            host.send_buffer_size().unwrap(),
            kernel_send_bytes,
            "sizing the receive buffer must leave the send buffer to the kernel"
        );

        assert_eq!(
            buffer_size(
                &mut resources,
                &bindings,
                socket,
                SocketBuffer::Send,
                Some(1 << 30)
            ),
            Ok(ByteCount(212_992)),
            "the guest must read back a 1 GiB send buffer cut to the ceiling"
        );
        let (_, ceiling_send_bytes) = as_the_kernel_reports(212_992);
        assert_eq!(host.send_buffer_size().unwrap(), ceiling_send_bytes);

        assert!(
            matches!(
                buffer_size(
                    &mut resources,
                    &bindings,
                    socket,
                    SocketBuffer::Receive,
                    Some(0)
                ),
                Err(Error::InvalidArgument(_))
            ),
            "a zero size must be refused"
        );
        assert_eq!(
            host.recv_buffer_size().unwrap(),
            floor_receive_bytes,
            "a refused size must leave the host socket as it was"
        );
        assert_eq!(
            buffer_size(
                &mut resources,
                &bindings,
                socket,
                SocketBuffer::Receive,
                None
            ),
            Ok(ByteCount(4_096)),
            "a refused size must leave the size the guest reads as it was"
        );
    }

    #[test]
    fn a_buffer_size_requested_before_connecting_is_held_for_the_connection() {
        let (mut resources, bindings) = resources(1);
        let socket = created(&mut resources, &bindings);
        let mut size = |buffer, requested| {
            buffer_size(&mut resources, &bindings, socket, buffer, requested)
                .expect("an unconnected socket must accept and report buffer sizes")
        };
        assert_eq!(
            size(SocketBuffer::Send, None),
            ByteCount(65_536),
            "a buffer the guest has not sized must read as the nominal size"
        );
        size(SocketBuffer::Send, Some(100_000));
        assert_eq!(
            (size(SocketBuffer::Send, None), size(SocketBuffer::Receive, None)),
            (ByteCount(100_000), ByteCount(65_536)),
            "the socket must hold the send size until it connects, leaving the receive buffer unsized"
        );
    }

    #[test]
    fn exhausting_native_handles_refuses_allocation_until_a_resource_is_dropped() {
        let (mut resources, bindings) = resources(1);
        let socket = created(&mut resources, &bindings);
        assert!(
            matches!(
                resources.call(Request::CreateTcp(AddressFamily::V4), &bindings),
                Err(Error::ResourceLimit)
            ),
            "a second socket must exceed the instance's one-handle budget"
        );
        resources
            .call(
                Request::Drop {
                    handle: socket,
                    kind: ResourceKind::Tcp,
                },
                &bindings,
            )
            .expect("dropping the owned socket must release its resource slot");
        let replacement = created(&mut resources, &bindings);
        assert_ne!(
            socket, replacement,
            "dropped handles must never alias replacement resources"
        );
    }

    #[test]
    fn a_pollable_destructor_cannot_drop_a_socket() {
        let (mut resources, bindings) = resources(1);
        let socket = created(&mut resources, &bindings);
        assert!(
            matches!(resources.call(Request::Drop { handle: socket, kind: ResourceKind::Pollable }, &bindings),
            Err(Error::InvalidHandle(handle)) if handle == socket),
            "readiness capability must not destroy a native socket"
        );
        assert!(
            resources.socket(socket).is_ok(),
            "a refused destructor must retain the socket"
        );
    }

    #[test]
    fn another_instances_socket_handle_has_no_authority() {
        let (mut first, bindings) = resources(1);
        let socket = created(&mut first, &bindings);
        let (mut second, second_bindings) = resources(1);
        assert!(
            matches!(second.call(Request::FinishConnect(socket), &second_bindings), Err(Error::InvalidHandle(handle)) if handle == socket),
            "a socket handle from another instance must not resolve in this instance"
        );
    }

    #[test]
    fn a_socket_cannot_finish_a_connection_that_has_not_started() {
        let (mut resources, bindings) = resources(1);
        let socket = created(&mut resources, &bindings);
        assert!(
            matches!(
                resources.call(Request::FinishConnect(socket), &bindings),
                Err(Error::InvalidState(_))
            ),
            "an unconnected socket must not manufacture stream handles"
        );
    }

    #[test]
    fn a_socket_handle_cannot_authorize_dns_resolution() {
        let (mut resources, bindings) = resources(2);
        let socket = created(&mut resources, &bindings);
        assert!(
            matches!(resources.call(Request::Resolve { network: socket, name: "example.test".into() }, &bindings), Err(Error::InvalidHandle(handle)) if handle == socket),
            "DNS must require the instance's network authority"
        );
    }

    #[test]
    fn a_socket_pollable_cannot_authorize_a_stream_send() {
        let (mut resources, bindings) = resources(2);
        let socket = created(&mut resources, &bindings);
        let Response::Handle(pollable) = resources
            .call(Request::Subscribe(socket), &bindings)
            .expect("an owned socket must permit a subscription")
        else {
            panic!("a subscription must return its handle")
        };
        assert!(
            matches!(
                resources.call(
                    Request::TrySend {
                        pollables: vec![pollable],
                        bytes: vec![1]
                    },
                    &bindings
                ),
                Err(Error::InvalidState(_))
            ),
            "only a connected output stream's pollable may authorize sending bytes"
        );
    }

    #[test]
    fn a_socket_refuses_a_destination_from_another_address_family() {
        let (mut resources, bindings) = resources(2);
        let socket = created(&mut resources, &bindings);
        let Response::Handle(network) = resources
            .call(Request::Network, &bindings)
            .expect("the instance must acquire network authority")
        else {
            panic!("network creation must return its handle")
        };
        assert!(
            matches!(
                resources.call(
                    Request::Connect {
                        socket,
                        network,
                        destination: "[::1]:443".parse().expect("the IPv6 endpoint must parse")
                    },
                    &bindings
                ),
                Err(Error::InvalidState(_))
            ),
            "an IPv4 socket must reject an IPv6 destination before reactor work starts"
        );
    }

    #[test]
    fn an_unready_poll_returns_an_explicit_refusal_instead_of_waiting() {
        let (mut resources, bindings) = resources(2);
        let socket = created(&mut resources, &bindings);
        let Response::Handle(subscription) = resources
            .call(Request::Subscribe(socket), &bindings)
            .expect("an owned socket must support readiness subscription")
        else {
            panic!("subscription must return a handle")
        };
        assert!(
            matches!(
                resources.call(Request::Poll(vec![subscription]), &bindings),
                Err(Error::Unsupported(_))
            ),
            "a poll with no ready resource must refuse synchronous waiting"
        );
    }
}

impl Drop for ReactorWork {
    fn drop(&mut self) {
        self.task.abort();
        self.table.cancel_owned(self.owner, self.future);
    }
}

/// Maximum simultaneously retained native handles for an instance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceCount(pub usize);

/// Bounds the host records retained on behalf of one instance.
pub(crate) struct ResourceLimits {
    pub handles: ResourceCount,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            handles: ResourceCount(super::GuestResourceLimits::default().native_handles),
        }
    }
}

struct Connection {
    stream: Arc<TcpStream>,
    _permit: ConnectionPermit,
}

enum SocketState {
    Created(AddressFamily),
    Connecting,
    Connected(Arc<Connection>),
    Failed(Error),
}

struct Socket {
    family: AddressFamily,
    no_delay: Arc<AtomicBool>,
    keep_alive: Arc<Mutex<keep_alive::Settings>>,
    buffer_sizes: Arc<Mutex<buffer_sizes::Settings>>,
    streams_taken: bool,
    state: Arc<Mutex<SocketState>>,
    task: Option<ReactorWork>,
}

struct Timer {
    ready: Arc<AtomicBool>,
    /// When the timer is due, on the reactor's clock; `None` for one created
    /// ready.
    deadline: Option<tokio::time::Instant>,
    /// Ends with the dispatch that armed it; the deadline does not.
    wake: Option<ReactorWork>,
}

impl Timer {
    /// Marks the timer ready once its deadline has passed, and otherwise arms
    /// a wake in the current dispatch if it has none, so a guest that waits on
    /// a timer created in an earlier request is still woken by it.
    fn refresh(&mut self, bindings: &dyn Context) -> Result<(), Error> {
        let Some(deadline) = self.deadline else {
            return Ok(());
        };
        if self.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let runtime = bindings
            .runtime()
            .ok_or(Error::Unsupported("native timer without reactor"))?;
        if reactor_now(&runtime) >= deadline {
            self.ready.store(true, Ordering::Release);
        } else if !self.wake.as_ref().is_some_and(ReactorWork::is_pending) {
            self.wake = Some(arm_timer(&self.ready, deadline, &runtime, bindings)?);
        }
        Ok(())
    }
}

pub(crate) fn reactor_now(runtime: &Runtime) -> tokio::time::Instant {
    let _reactor = runtime.reactor.enter();
    tokio::time::Instant::now()
}

fn arm_timer(
    ready: &Arc<AtomicBool>,
    deadline: tokio::time::Instant,
    runtime: &Runtime,
    bindings: &dyn Context,
) -> Result<ReactorWork, Error> {
    let future = bindings
        .alloc_rearmable(isoplane_abi::native_io::canonical::POLL_IMPORT)
        .ok_or(Error::ResourceLimit)?;
    let completion = runtime.clone();
    let ready = ready.clone();
    let task = runtime.spawn(async move {
        tokio::time::sleep_until(deadline).await;
        ready.store(true, Ordering::Release);
        completion.settle_future(future, ResultRecord::Ok(Vec::new()));
    });
    runtime.futures.attach_task(future, task.abort_handle());
    Ok(ReactorWork {
        future: FutureId::from(future),
        owner: bindings.instance(),
        table: runtime.futures.clone(),
        task: task.abort_handle(),
    })
}

enum Resource {
    Network,
    Addresses(Addresses),
    Socket(Socket),
    Input(Input),
    Output(Output),
    Error(StreamFailure),
    Subscription(ResourceHandle),
    Timer(Timer),
}

/// What a failed stream operation leaves behind for the guest to inspect.
struct StreamFailure {
    message: String,
    code: isoplane_abi::native_io::network_error::NetworkErrorCode,
}

/// Retained by InstanceCtx across Store destruction and released with its owner.
pub(crate) struct Resources {
    next: u32,
    prepared: Option<isoplane_abi::native_io::PreparedResources>,
    limits: ResourceLimits,
    entries: BTreeMap<ResourceHandle, Resource>,
    network: Option<ResourceHandle>,
    resolved: BTreeSet<std::net::IpAddr>,
}

/// What an idle instance that opted in to running timers between requests is
/// waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdleTimers {
    pub(crate) minimum_interval: std::time::Duration,
    /// The earliest deadline of a timer still pending, if one is.
    pub(crate) earliest_deadline: Option<tokio::time::Instant>,
}

impl Resources {
    pub(crate) fn idle_timers(&self) -> Option<IdleTimers> {
        let minimum_interval_ms = self.prepared.as_ref()?.timers_between_requests_ms?;
        let earliest_deadline = self
            .entries
            .values()
            .filter_map(|resource| match resource {
                Resource::Timer(timer) if !timer.ready.load(Ordering::Acquire) => timer.deadline,
                _ => None,
            })
            .min();
        Some(IdleTimers {
            minimum_interval: std::time::Duration::from_millis(minimum_interval_ms),
            earliest_deadline,
        })
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.next = 1;
        self.prepared = None;
        self.network = None;
        self.resolved.clear();
    }

    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            next: 1,
            prepared: None,
            limits,
            entries: BTreeMap::new(),
            network: None,
            resolved: BTreeSet::new(),
        }
    }

    /// The resources initialization leaves for the template, at `now` on the
    /// reactor's clock.
    pub(crate) fn prepared(
        &self,
        now: tokio::time::Instant,
    ) -> Result<isoplane_abi::native_io::PreparedResources, Error> {
        let mut retained = BTreeMap::<&'static str, usize>::new();
        let mut ready_pollables = Vec::new();
        let mut pending_timers = Vec::new();
        for (&handle, resource) in &self.entries {
            let kind = match resource {
                Resource::Network => continue,
                Resource::Timer(timer) => {
                    match timer
                        .deadline
                        .filter(|&deadline| !timer.ready.load(Ordering::Acquire) && deadline > now)
                    {
                        Some(deadline) => {
                            pending_timers.push(isoplane_abi::native_io::PreparedTimer {
                                handle,
                                remaining_ms: u64::try_from(
                                    (deadline - now).as_nanos().div_ceil(1_000_000),
                                )
                                .unwrap_or(u64::MAX),
                            })
                        }
                        None => ready_pollables.push(handle),
                    }
                    continue;
                }
                Resource::Addresses(_) => {
                    "initialization retains a DNS result; dispose it before capture"
                }
                Resource::Socket(_) => {
                    "initialization retains a TCP socket; dispose it before capture"
                }
                Resource::Input(_) => {
                    "initialization retains an input stream; dispose it before capture"
                }
                Resource::Output(_) => {
                    "initialization retains an output stream; dispose it before capture"
                }
                Resource::Error(_) => {
                    "initialization retains a stream error; dispose it before capture"
                }
                Resource::Subscription(_) => {
                    "initialization retains a readiness subscription; dispose it before capture"
                }
            };
            *retained.entry(kind).or_default() += 1;
        }
        if let Some((&kind, _)) = retained.first_key_value() {
            tracing::warn!(
                ?retained,
                "initialization capture refused retained native resources"
            );
            return Err(Error::InvalidState(kind));
        }
        Ok(isoplane_abi::native_io::PreparedResources {
            next_handle: self.next,
            network: self.network,
            ready_pollables,
            pending_timers,
            timers_between_requests_ms: None,
        })
    }

    /// Restores the template's resources in an instance activated at `now` on
    /// the reactor's clock.
    pub(crate) fn restore_prepared(
        &mut self,
        prepared: &isoplane_abi::native_io::PreparedResources,
        now: tokio::time::Instant,
    ) -> Result<(), Error> {
        if let Some(previous) = &self.prepared {
            return if previous == prepared {
                Ok(())
            } else {
                Err(Error::InvalidState(
                    "retained native resources belong to a different template",
                ))
            };
        }
        if !self.entries.is_empty() || self.next != 1 {
            return Err(Error::InvalidState(
                "native resources must be restored before use",
            ));
        }
        let handles: BTreeSet<_> = prepared
            .network
            .iter()
            .chain(&prepared.ready_pollables)
            .chain(prepared.pending_timers.iter().map(|timer| &timer.handle))
            .copied()
            .collect();
        let count = prepared.ready_pollables.len()
            + prepared.pending_timers.len()
            + usize::from(prepared.network.is_some());
        if prepared.next_handle == 0
            || handles.len() != count
            || handles
                .iter()
                .any(|handle| handle.0 == 0 || handle.0 >= prepared.next_handle)
        {
            return Err(Error::InvalidState(
                "invalid prepared native handle sequence",
            ));
        }
        self.reserve(ResourceCount(count))?;
        if let Some(handle) = prepared.network {
            self.entries.insert(handle, Resource::Network);
        }
        for &handle in &prepared.ready_pollables {
            self.entries.insert(
                handle,
                Resource::Timer(Timer {
                    ready: Arc::new(AtomicBool::new(true)),
                    deadline: None,
                    wake: None,
                }),
            );
        }
        for timer in &prepared.pending_timers {
            self.entries.insert(
                timer.handle,
                Resource::Timer(Timer {
                    ready: Arc::new(AtomicBool::new(false)),
                    deadline: Some(now + std::time::Duration::from_millis(timer.remaining_ms)),
                    wake: None,
                }),
            );
        }
        self.next = prepared.next_handle;
        self.network = prepared.network;
        self.prepared = Some(prepared.clone());
        Ok(())
    }

    fn reserve(&self, count: ResourceCount) -> Result<(), Error> {
        if self
            .entries
            .len()
            .saturating_add(self.resolved.len())
            .saturating_add(count.0)
            > self.limits.handles.0
            || self
                .next
                .checked_add(u32::try_from(count.0).map_err(|_| Error::ResourceLimit)?)
                .is_none()
        {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }

    fn insert(&mut self, resource: Resource) -> Result<ResourceHandle, Error> {
        self.reserve(ResourceCount(1))?;
        let handle = ResourceHandle(self.next);
        self.next += 1;
        self.entries.insert(handle, resource);
        Ok(handle)
    }

    fn socket(&self, handle: ResourceHandle) -> Result<Arc<Mutex<SocketState>>, Error> {
        self.tcp(handle).map(|socket| socket.state.clone())
    }

    fn tcp(&self, handle: ResourceHandle) -> Result<&Socket, Error> {
        match self.entries.get(&handle) {
            Some(Resource::Socket(socket)) => Ok(socket),
            Some(
                Resource::Network
                | Resource::Addresses(_)
                | Resource::Error(_)
                | Resource::Input(_)
                | Resource::Output(_)
                | Resource::Subscription(_)
                | Resource::Timer(_),
            )
            | None => Err(Error::InvalidHandle(handle)),
        }
    }

    pub(crate) fn call(
        &mut self,
        request: Request,
        bindings: &dyn Context,
    ) -> Result<Response, Error> {
        let operation = match &request {
            Request::Network => "network",
            Request::Resolve { .. } => "resolve",
            Request::NextAddress(_) => "next_address",
            Request::CreateTcp(_) => "create_tcp",
            Request::Connect { .. } => "connect",
            Request::FinishConnect(_) => "finish_connect",
            Request::Read { .. } => "read",
            Request::TrySend { .. } => "send",
            Request::ErrorMessage(_) => "error_message",
            Request::ErrorCode(_) => "error_code",
            Request::SocketAddress { .. } => "socket_address",
            Request::NoDelay { .. } => "no_delay",
            Request::KeepAlive { .. } => "keep_alive",
            Request::BufferSize { .. } => "buffer_size",
            Request::Shutdown { .. } => "shutdown",
            Request::Subscribe(_) => "subscribe",
            Request::Timer(_) => "timer",
            Request::Ready(_) => "ready",
            Request::Poll(_) => "poll",
            Request::Drop { .. } => "drop",
        };
        let observes_completion = matches!(
            &request,
            Request::NextAddress(_) | Request::FinishConnect(_)
        );
        let span = tracing::info_span!("native_io", tenant = %bindings.tenant(), instance = %bindings.instance(), operation);
        let _entered = span.enter();
        let result = self.execute(request, bindings);
        if let Err(error) = &result {
            let already_reported = observes_completion
                && matches!(
                    error,
                    Error::Dns { .. } | Error::Connection { .. } | Error::Denied(_)
                );
            if !already_reported && !matches!(error, Error::WouldBlock) {
                report_failure(bindings, error);
            }
        }
        result
    }

    fn execute(&mut self, request: Request, bindings: &dyn Context) -> Result<Response, Error> {
        match request {
            Request::NoDelay { socket, enabled } => {
                self.no_delay(socket, enabled).map(Response::NoDelay)
            }
            Request::KeepAlive { socket, change } => {
                self.keep_alive(socket, change).map(Response::KeepAlive)
            }
            Request::BufferSize {
                socket,
                buffer,
                requested,
            } => self
                .buffer_size(socket, buffer, requested)
                .map(Response::BufferSize),
            Request::Network => {
                if let Some(handle) = self.network {
                    return Ok(Response::Handle(handle));
                }
                let handle = self.insert(Resource::Network)?;
                self.network = Some(handle);
                Ok(Response::Handle(handle))
            }
            Request::Resolve { network, name } => {
                if !matches!(self.entries.get(&network), Some(Resource::Network)) {
                    return Err(Error::InvalidHandle(network));
                }
                self.reserve(ResourceCount(1))?;
                self.insert(Resource::Addresses(Addresses::start(
                    bindings,
                    name,
                    self.limits.handles,
                )?))
                .map(Response::Handle)
            }
            Request::NextAddress(handle) => {
                let address = match self.entries.get_mut(&handle) {
                    Some(Resource::Addresses(addresses)) => addresses.next()?,
                    // New resource kinds cannot carry DNS answer authority.
                    _ => return Err(Error::InvalidHandle(handle)),
                };
                if let Some(address) = address {
                    if !self.resolved.contains(&address) {
                        self.reserve(ResourceCount(1))?;
                    }
                    self.resolved.insert(address);
                }
                Ok(Response::Address(address))
            }
            Request::CreateTcp(family) => self
                .insert(Resource::Socket(Socket {
                    family,
                    no_delay: Arc::new(AtomicBool::new(false)),
                    keep_alive: Arc::default(),
                    buffer_sizes: Arc::default(),
                    streams_taken: false,
                    state: Arc::new(Mutex::new(SocketState::Created(family))),
                    task: None,
                }))
                .map(Response::Handle),
            Request::Connect {
                socket,
                network,
                destination,
            } => self.connect(bindings, socket, network, destination),
            Request::FinishConnect(handle) => {
                let state = self.socket(handle)?;
                match &*state.lock() {
                    SocketState::Connected(_) => {}
                    SocketState::Connecting => return Err(Error::WouldBlock),
                    SocketState::Created(_) => {
                        return Err(Error::InvalidState("connect has not started"))
                    }
                    SocketState::Failed(error) => return Err(error.clone()),
                }
                if let Some(Resource::Socket(socket)) = self.entries.get(&handle) {
                    if socket.streams_taken {
                        return Err(Error::InvalidState("connected streams already acquired"));
                    }
                }
                self.reserve(ResourceCount(2))?;
                let input = self.insert(Resource::Input(Input::new(state.clone())))?;
                let output = self.insert(Resource::Output(Output::new(state)))?;
                if let Some(Resource::Socket(socket)) = self.entries.get_mut(&handle) {
                    socket.streams_taken = true;
                }
                Ok(Response::Streams { input, output })
            }
            Request::Read { stream, length } => {
                let result = match self.entries.get_mut(&stream) {
                    Some(Resource::Input(input)) => input.read(length, bindings),
                    _ => return Err(Error::InvalidHandle(stream)),
                };
                if let Err(error) = &result {
                    report_failure(bindings, error);
                }
                self.stream_result(result)
            }
            Request::TrySend { pollables, bytes } => {
                let target = pollables
                    .into_iter()
                    .find_map(|handle| match self.entries.get(&handle) {
                        Some(Resource::Subscription(target))
                            if matches!(self.entries.get(target), Some(Resource::Output(_))) =>
                        {
                            Some(*target)
                        }
                        // Only output pollables authorize native sends; other resource kinds add no authority.
                        _ => None,
                    })
                    .ok_or(Error::InvalidState("native send has no output pollable"))?;
                self.output(target)?.send(&bytes, bindings)
            }
            Request::ErrorMessage(handle) => match self.entries.get(&handle) {
                Some(Resource::Error(failure)) => {
                    Ok(Response::Bytes(failure.message.as_bytes().to_vec()))
                }
                _ => Err(Error::InvalidHandle(handle)),
            },
            Request::ErrorCode(handle) => match self.entries.get(&handle) {
                Some(Resource::Error(failure)) => Ok(Response::ErrorCode(failure.code)),
                _ => Err(Error::InvalidHandle(handle)),
            },
            Request::SocketAddress { socket, peer } => {
                let socket = self.tcp(socket)?;
                let connection = streams::connection(&socket.state)?;
                let address = if peer {
                    connection.stream.peer_addr()
                } else {
                    connection.stream.local_addr()
                }
                .map_err(Error::from)?;
                let address = match (socket.family, address) {
                    (AddressFamily::V6, std::net::SocketAddr::V4(address)) => {
                        std::net::SocketAddr::new(
                            address.ip().to_ipv6_mapped().into(),
                            address.port(),
                        )
                    }
                    (AddressFamily::V4, _) | (AddressFamily::V6, std::net::SocketAddr::V6(_)) => {
                        address
                    }
                };
                Ok(Response::SocketAddress(address))
            }
            Request::Shutdown { socket, how } => {
                let state = self.socket(socket)?;
                let connection = streams::connection(&state)?;
                socket2::SockRef::from(connection.stream.as_ref())
                    .shutdown(how)
                    .map_err(Error::from)?;
                Ok(Response::Done)
            }
            Request::Subscribe(handle) => {
                match self.entries.get(&handle) {
                    Some(
                        Resource::Socket(_)
                        | Resource::Input(_)
                        | Resource::Output(_)
                        | Resource::Addresses(_),
                    ) => {}
                    Some(
                        Resource::Network
                        | Resource::Error(_)
                        | Resource::Subscription(_)
                        | Resource::Timer(_),
                    )
                    | None => return Err(Error::InvalidHandle(handle)),
                }
                self.insert(Resource::Subscription(handle))
                    .map(Response::Handle)
            }
            Request::Timer(duration) => {
                self.reserve(ResourceCount(1))?;
                let timer = if duration.0 == 0 {
                    Timer {
                        ready: Arc::new(AtomicBool::new(true)),
                        deadline: None,
                        wake: None,
                    }
                } else {
                    let runtime = bindings
                        .runtime()
                        .ok_or(Error::Unsupported("native timer without reactor"))?;
                    let duration_ms = duration
                        .0
                        .div_ceil(isoplane_abi::pal::CLOCK_QUANTUM_NS as u64);
                    let deadline =
                        reactor_now(&runtime) + std::time::Duration::from_millis(duration_ms);
                    let ready = Arc::new(AtomicBool::new(false));
                    let wake = arm_timer(&ready, deadline, &runtime, bindings)?;
                    Timer {
                        ready,
                        deadline: Some(deadline),
                        wake: Some(wake),
                    }
                };
                self.insert(Resource::Timer(timer)).map(Response::Handle)
            }
            Request::Ready(handle) => {
                self.refresh_timer(handle, bindings)?;
                self.ready(handle).map(Response::Ready)
            }
            Request::Poll(handles) => {
                let mut indexes = Vec::new();
                for (index, handle) in handles.into_iter().enumerate() {
                    self.refresh_timer(handle, bindings)?;
                    if self.ready(handle)? {
                        indexes.push(u32::try_from(index).map_err(|_| Error::ResourceLimit)?);
                    } else {
                        self.arm(handle, bindings)?;
                    }
                }
                if indexes.is_empty() {
                    return Err(Error::Unsupported(
                        "blocking poll on an empty guest stack boundary",
                    ));
                }
                Ok(Response::ReadyIndexes(indexes))
            }
            Request::Drop { handle, kind } => {
                let resource = self
                    .entries
                    .get(&handle)
                    .ok_or(Error::InvalidHandle(handle))?;
                let valid = matches!(
                    (kind, resource),
                    (ResourceKind::Tcp, Resource::Socket(_))
                        | (ResourceKind::Addresses, Resource::Addresses(_))
                        | (ResourceKind::Error, Resource::Error(_))
                        | (ResourceKind::Input, Resource::Input(_))
                        | (ResourceKind::Output, Resource::Output(_))
                        | (
                            ResourceKind::Pollable,
                            Resource::Timer(_) | Resource::Subscription(_)
                        )
                );
                if !valid {
                    return Err(Error::InvalidHandle(handle));
                }
                if let Some(Resource::Socket(socket)) = self.entries.remove(&handle) {
                    *socket.state.lock() =
                        SocketState::Failed(Error::InvalidState("socket disposed"));
                }
                Ok(Response::Done)
            }
        }
    }

    fn refresh_timer(
        &mut self,
        handle: ResourceHandle,
        bindings: &dyn Context,
    ) -> Result<(), Error> {
        match self.entries.get_mut(&handle) {
            Some(Resource::Timer(timer)) => timer.refresh(bindings),
            _ => Ok(()),
        }
    }

    /// A stream the guest polled and found not ready is one it waits on:
    /// arms the readiness wake a blocked read or send would, so a guest that
    /// checks readiness instead of attempting the operation (a zero-byte
    /// read, `Socket.Poll`) has a wake to park on. Every other pollable's
    /// wake exists from the operation that created it.
    fn arm(&mut self, handle: ResourceHandle, bindings: &dyn Context) -> Result<(), Error> {
        let Some(Resource::Subscription(target)) = self.entries.get(&handle) else {
            return Ok(());
        };
        match self.entries.get_mut(&target.clone()) {
            Some(Resource::Input(input)) => input.arm(bindings),
            Some(Resource::Output(output)) => output.arm(bindings),
            Some(
                Resource::Socket(_)
                | Resource::Addresses(_)
                | Resource::Network
                | Resource::Error(_)
                | Resource::Subscription(_)
                | Resource::Timer(_),
            )
            | None => Ok(()),
        }
    }

    fn ready(&self, handle: ResourceHandle) -> Result<bool, Error> {
        match self.entries.get(&handle) {
            Some(Resource::Timer(timer)) => Ok(timer.ready.load(Ordering::Acquire)),
            Some(Resource::Subscription(target)) => match self.entries.get(target) {
                Some(Resource::Addresses(addresses)) => Ok(addresses.ready()),
                Some(Resource::Socket(socket)) => match &*socket.state.lock() {
                    SocketState::Created(_) | SocketState::Connecting => Ok(false),
                    SocketState::Connected(_) | SocketState::Failed(_) => Ok(true),
                },
                Some(Resource::Input(input)) => input.ready(),
                Some(Resource::Output(output)) => output.ready(),
                Some(
                    Resource::Network
                    | Resource::Error(_)
                    | Resource::Subscription(_)
                    | Resource::Timer(_),
                )
                | None => Err(Error::InvalidHandle(*target)),
            },
            Some(
                Resource::Network
                | Resource::Addresses(_)
                | Resource::Error(_)
                | Resource::Socket(_)
                | Resource::Input(_)
                | Resource::Output(_),
            )
            | None => Err(Error::InvalidHandle(handle)),
        }
    }

    fn output(&mut self, handle: ResourceHandle) -> Result<&mut Output, Error> {
        match self.entries.get_mut(&handle) {
            Some(Resource::Output(output)) => Ok(output),
            _ => Err(Error::InvalidHandle(handle)),
        }
    }

    fn stream_result(&mut self, result: Result<Response, Error>) -> Result<Response, Error> {
        match result {
            Ok(response) => Ok(response),
            Err(error) => self
                .insert(Resource::Error(StreamFailure {
                    message: error.to_string(),
                    code: error.network_error_code(),
                }))
                .map(Response::StreamError),
        }
    }

    fn no_delay(&self, handle: ResourceHandle, enabled: Option<bool>) -> Result<bool, Error> {
        let socket = self.tcp(handle)?;
        let state = socket.state.lock();
        match &*state {
            SocketState::Connected(connection) => {
                if let Some(enabled) = enabled {
                    connection
                        .stream
                        .set_nodelay(enabled)
                        .map_err(Error::from)?;
                }
                connection.stream.nodelay().map_err(Error::from)
            }
            SocketState::Created(_) | SocketState::Connecting | SocketState::Failed(_) => {
                if let Some(enabled) = enabled {
                    socket.no_delay.store(enabled, Ordering::Relaxed);
                }
                Ok(socket.no_delay.load(Ordering::Relaxed))
            }
        }
    }

    fn keep_alive(
        &self,
        handle: ResourceHandle,
        change: Option<KeepAliveChange>,
    ) -> Result<KeepAlive, Error> {
        let socket = self.tcp(handle)?;
        let state = socket.state.lock();
        let mut settings = socket.keep_alive.lock();
        if let Some(change) = change {
            let changed = settings.with(change)?;
            match &*state {
                SocketState::Connected(connection) => {
                    changed.apply(&connection.stream).map_err(Error::from)?;
                }
                // No host socket exists; the connection takes the record when it completes.
                SocketState::Created(_) | SocketState::Connecting | SocketState::Failed(_) => {}
            }
            *settings = changed;
        }
        Ok(settings.effective())
    }

    fn buffer_size(
        &self,
        handle: ResourceHandle,
        buffer: SocketBuffer,
        requested: Option<ByteCount>,
    ) -> Result<ByteCount, Error> {
        let socket = self.tcp(handle)?;
        let state = socket.state.lock();
        let mut settings = socket.buffer_sizes.lock();
        if let Some(requested) = requested {
            let changed = settings.with(buffer, requested)?;
            match &*state {
                SocketState::Connected(connection) => {
                    changed.apply(&connection.stream).map_err(Error::from)?;
                }
                // No host socket exists; the connection takes the record when it completes.
                SocketState::Created(_) | SocketState::Connecting | SocketState::Failed(_) => {}
            }
            *settings = changed;
        }
        Ok(settings.effective(buffer))
    }

    fn has_resolved_address(&self, address: std::net::IpAddr) -> bool {
        if self.resolved.contains(&address) {
            return true;
        }
        match address {
            std::net::IpAddr::V4(address) => self
                .resolved
                .contains(&std::net::IpAddr::V6(address.to_ipv6_mapped())),
            std::net::IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .is_some_and(|address| self.resolved.contains(&std::net::IpAddr::V4(address))),
        }
    }

    fn connect(
        &mut self,
        bindings: &dyn Context,
        handle: ResourceHandle,
        network: ResourceHandle,
        destination: std::net::SocketAddr,
    ) -> Result<Response, Error> {
        if !matches!(self.entries.get(&network), Some(Resource::Network)) {
            return Err(Error::InvalidHandle(network));
        }
        let socket = self.tcp(handle)?;
        let state = socket.state.clone();
        let no_delay = socket.no_delay.clone();
        let keep_alive = socket.keep_alive.clone();
        let buffer_sizes = socket.buffer_sizes.clone();
        {
            let current = state.lock();
            match &*current {
                SocketState::Created(family)
                    if matches!(
                        (family, destination),
                        (AddressFamily::V4, std::net::SocketAddr::V4(_))
                            | (AddressFamily::V6, std::net::SocketAddr::V6(_))
                    ) => {}
                SocketState::Created(_) => {
                    return Err(Error::InvalidState(
                        "address family differs from socket family",
                    ))
                }
                SocketState::Connecting | SocketState::Connected(_) | SocketState::Failed(_) => {
                    return Err(Error::InvalidState("connect already started"))
                }
            }
        }
        let runtime = bindings
            .runtime()
            .ok_or(Error::Unsupported("outbound connect without reactor"))?
            .clone();
        let policy = runtime.egress.clone();
        let owner = bindings.connection_owner().map_err(Error::from)?;
        let reach = bindings.reach();
        policy
            .check_socket(
                destination,
                crate::egress::Reach {
                    allowlist: if self.has_resolved_address(destination.ip()) {
                        None
                    } else {
                        reach.allowlist
                    },
                    exceptions: reach.exceptions,
                },
            )
            .map_err(|reason| Error::Denied(reason.to_string()))?;
        let future = bindings.alloc("socket_call").ok_or(Error::ResourceLimit)?;
        *state.lock() = SocketState::Connecting;

        let completion = runtime.clone();
        let tenant = bindings.tenant().to_owned();
        let instance = bindings.instance();
        let task = runtime.spawn(async move {
            let result = async {
                // In progress, behind the instance's earlier attempts, until the
                // connection is established or fails.
                let attempt = policy.begin_connect(owner).await.map_err(Error::from)?;
                let stream = attempt.connect(destination).await.map_err(Error::from)?;
                Ok(Connection {
                    stream: Arc::new(stream),
                    _permit: attempt.established(),
                })
            }
            .await;
            {
                let mut current = state.lock();
                if !matches!(*current, SocketState::Connecting) {
                    return;
                }
                *current = match result {
                    Ok(connection) => match connection
                        .stream
                        .set_nodelay(no_delay.load(Ordering::Relaxed))
                        .and_then(|()| keep_alive.lock().apply(&connection.stream))
                        .and_then(|()| buffer_sizes.lock().apply(&connection.stream))
                    {
                        Ok(()) => SocketState::Connected(Arc::new(connection)),
                        Err(error) => SocketState::Failed(Error::from(error)),
                    },
                    Err(error) => SocketState::Failed(error),
                };
                if let SocketState::Failed(error) = &*current {
                    if matches!(error, Error::Denied(_)) {
                        completion.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                    }
                    tracing::warn!(%tenant, %instance, %destination, %error, "outbound TCP connect failed");
                }
            }
            completion.settle_future(future, ResultRecord::Ok(Vec::new()));
        });
        runtime.futures.attach_task(future, task.abort_handle());
        if let Some(Resource::Socket(socket)) = self.entries.get_mut(&handle) {
            socket.task = Some(ReactorWork {
                future: FutureId::from(future),
                owner: bindings.instance(),
                table: runtime.futures.clone(),
                task: task.abort_handle(),
            });
        }
        Ok(Response::Done)
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;
    use isoplane_abi::native_io::DurationNanos;

    fn resources(handles: usize) -> (Resources, CellBindings) {
        (
            Resources::new(ResourceLimits {
                handles: ResourceCount(handles),
            }),
            CellBindings::new(super::super::tests::ctx()),
        )
    }

    fn created(resources: &mut Resources, bindings: &CellBindings) -> ResourceHandle {
        match resources
            .call(Request::Timer(DurationNanos(0)), bindings)
            .expect("an available resource slot must accept a ready pollable")
        {
            Response::Handle(handle) => handle,
            response => panic!("pollable allocation returned an unexpected result: {response:?}"),
        }
    }

    struct Woken(Mutex<Vec<InstanceId>>);

    impl crate::bindings::WakeSink for Woken {
        fn signal(&self, owner: InstanceId) {
            self.0.lock().push(owner);
        }
    }

    fn reactor_bindings() -> (Arc<crate::bindings::InstanceCtx>, Arc<Woken>) {
        use isoplane_abi::config::{HandlerLifecycle, Limits};
        let woken = Arc::new(Woken(Mutex::new(Vec::new())));
        let mut context = crate::bindings::InstanceCtx::for_test(
            InstanceId(7),
            HandlerLifecycle::Reuse,
            Limits::default(),
        );
        context.rt = Some(Arc::new(crate::bindings::CellRuntime {
            runtime_configuration: Default::default(),
            services: Arc::new(crate::cell::Services {
                egress: Arc::new(
                    crate::egress::HostEgressPolicy::system(
                        crate::egress::ConnectionLimits::uniform(1).expect("connection limits"),
                    )
                    .expect("egress policy"),
                ),
                fetcher: Arc::new(crate::fakes::FakeFetcher::default()),
            }),
            futures: Arc::new(FutureTable::new()),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            wake: woken.clone(),
            reactor: tokio::runtime::Handle::current(),
        }));
        context.begin_slice(0, 1);
        (Arc::new(context), woken)
    }

    fn ready(resources: &mut Resources, handle: ResourceHandle, bindings: &CellBindings) -> bool {
        match resources.call(Request::Ready(handle), bindings) {
            Ok(Response::Ready(ready)) => ready,
            other => panic!("a readiness check returned {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_timer_outlives_the_request_that_created_it() {
        let (context, woken) = reactor_bindings();
        let bindings = CellBindings::new(context.clone());
        let futures = context.rt.as_ref().expect("reactor").futures.clone();
        let mut resources = Resources::new(ResourceLimits {
            handles: ResourceCount(2),
        });
        let mut timer = |milliseconds: u64| match resources.call(
            Request::Timer(DurationNanos(milliseconds * 1_000_000)),
            &bindings,
        ) {
            Ok(Response::Handle(handle)) => handle,
            other => panic!("a {milliseconds} ms timer returned {other:?}"),
        };
        let (soon, later) = (timer(10), timer(50));
        assert!(
            !futures.end_dispatch(context.instance, 1),
            "step 1: pending timers must not count as abandoned work"
        );
        assert_eq!(
            futures.live_futures(),
            0,
            "step 1: their wakes end with the request"
        );

        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        assert!(
            ready(&mut resources, soon, &bindings),
            "step 2: a timer is due once its deadline passes, though its wake ended"
        );
        assert!(
            !ready(&mut resources, later, &bindings),
            "step 2: a timer is not due before its deadline"
        );
        assert_eq!(
            futures.live_futures(),
            1,
            "step 2: checking a timer that is not yet due arms a wake for it"
        );

        tokio::time::advance(std::time::Duration::from_millis(40)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            *woken.0.lock(),
            vec![context.instance],
            "step 3: the re-armed wake must wake the instance at the deadline"
        );
        assert!(
            ready(&mut resources, later, &bindings),
            "step 3: the woken timer must be due"
        );
    }

    #[test]
    fn cancellation_refuses_new_deadlines_but_allows_polling_and_disposal() {
        use isoplane_engine::HostBindings;
        let context = super::super::tests::ctx();
        let mut bindings = CellBindings::new(context.clone());
        context.poison_imports();
        assert!(
            matches!(
                bindings.native_io(Request::Timer(DurationNanos(1))),
                Err(Error::Denied(_))
            ),
            "a cancelled dispatch must not start reactor work"
        );
        let Response::Handle(timer) = bindings
            .native_io(Request::Timer(DurationNanos(0)))
            .expect("cancellation cleanup must retain nonblocking polling")
        else {
            panic!("a ready pollable must return a handle")
        };
        assert!(
            matches!(bindings.native_io(Request::Poll(vec![timer])), Ok(Response::ReadyIndexes(indexes)) if indexes == vec![0]),
            "cancellation cleanup must be able to inspect its ready pollable"
        );
        bindings
            .native_io(Request::Drop {
                handle: timer,
                kind: ResourceKind::Pollable,
            })
            .expect("a cancelled dispatch must be allowed to dispose native resources");
    }

    #[test]
    fn exhausting_native_handles_refuses_allocation_until_a_resource_is_dropped() {
        let (mut resources, bindings) = resources(1);
        let handle = created(&mut resources, &bindings);
        assert!(
            matches!(
                resources.call(Request::Timer(DurationNanos(0)), &bindings),
                Err(Error::ResourceLimit)
            ),
            "a second pollable must exceed the instance's one-handle budget"
        );
        resources
            .call(
                Request::Drop {
                    handle,
                    kind: ResourceKind::Pollable,
                },
                &bindings,
            )
            .expect("the owner must release its pollable");
        let replacement = created(&mut resources, &bindings);
        assert_ne!(
            handle, replacement,
            "a dropped handle must never alias a replacement pollable"
        );
    }

    #[test]
    fn another_instances_pollable_handle_has_no_authority() {
        let (mut first, bindings) = resources(1);
        let handle = created(&mut first, &bindings);
        let (mut second, second_bindings) = resources(1);
        assert!(
            matches!(second.call(Request::Ready(handle), &second_bindings), Err(Error::InvalidHandle(received)) if received == handle),
            "another instance's pollable must not resolve in this instance"
        );
    }

    #[test]
    fn an_unready_poll_refuses_synchronous_waiting() {
        let (mut resources, bindings) = resources(1);
        let handle = resources
            .insert(Resource::Timer(Timer {
                ready: Arc::new(AtomicBool::new(false)),
                deadline: None,
                wake: None,
            }))
            .expect("the unready test timer must fit its resource budget");
        assert!(
            matches!(
                resources.call(Request::Poll(vec![handle]), &bindings),
                Err(Error::Unsupported(_))
            ),
            "a poll with no ready resource must refuse synchronous waiting"
        );
    }

    #[test]
    fn polling_returns_only_the_indexes_of_ready_resources() {
        let (mut resources, bindings) = resources(2);
        let pending = resources
            .insert(Resource::Timer(Timer {
                ready: Arc::new(AtomicBool::new(false)),
                deadline: None,
                wake: None,
            }))
            .expect("the pending test pollable must fit");
        let ready = created(&mut resources, &bindings);
        let Response::ReadyIndexes(indexes) = resources
            .call(Request::Poll(vec![pending, ready]), &bindings)
            .expect("polling with a ready resource must return")
        else {
            panic!("polling must return ready indexes")
        };
        assert_eq!(
            indexes,
            vec![1],
            "poll must report the ready input index without reporting pending resources"
        );
    }
}
