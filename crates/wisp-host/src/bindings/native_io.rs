//! Instance-owned native networking resources, independent of the Wasmtime Store.

mod addresses;
mod streams;

use addresses::Addresses;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use streams::{Input, Output};

use parking_lot::Mutex;
use tokio::net::TcpStream;
use wisp_abi::native_io::{AddressFamily, Error, Request, ResourceHandle, ResourceKind, Response};
use wisp_abi::result::ResultRecord;

use super::CellBindings;
use crate::egress::ConnectionPermit;
use crate::futures::{FutureId, FutureTable};
use crate::InstanceId;

fn report_failure(bindings: &CellBindings, error: &Error) {
    if matches!(error, Error::Denied(_)) {
        if let Some(runtime) = bindings.rt() {
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

    async fn connected_endpoints(family: AddressFamily) {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let remote = listener.local_addr().unwrap();
        let stream = TcpStream::connect(remote).await.unwrap();
        let (_accepted, local) = listener.accept().await.unwrap();
        let policy = Arc::new(
            crate::egress::HostEgressPolicy::system(1, 1024)
                .unwrap()
                .with_destinations(Arc::new(EndpointPeer(remote))),
        );
        let permit = policy.check_socket(remote, None).unwrap();
        let (mut resources, bindings) = resources(1);
        let socket = resources
            .insert(Resource::Socket(Socket {
                family,
                no_delay: Arc::new(AtomicBool::new(false)),
                streams_taken: false,
                state: Arc::new(Mutex::new(SocketState::Connected(Arc::new(Connection {
                    stream: Arc::new(stream),
                    permit,
                })))),
                task: None,
            }))
            .unwrap();
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
    permit: ConnectionPermit,
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
    streams_taken: bool,
    state: Arc<Mutex<SocketState>>,
    task: Option<ReactorWork>,
}

struct Timer {
    ready: Arc<AtomicBool>,
    _task: Option<ReactorWork>,
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
    code: wisp_abi::native_io::network_error::NetworkErrorCode,
}

/// Retained by InstanceCtx across Store destruction and released with its owner.
pub(crate) struct Resources {
    next: u32,
    limits: ResourceLimits,
    entries: BTreeMap<ResourceHandle, Resource>,
    network: Option<ResourceHandle>,
    resolved: BTreeSet<std::net::IpAddr>,
}

impl Resources {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.network = None;
        self.resolved.clear();
    }

    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            next: 1,
            limits,
            entries: BTreeMap::new(),
            network: None,
            resolved: BTreeSet::new(),
        }
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
        bindings: &CellBindings,
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
        let span = tracing::info_span!("native_io", tenant = %bindings.ctx.tenant, instance = %bindings.ctx.instance, operation);
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

    fn execute(&mut self, request: Request, bindings: &CellBindings) -> Result<Response, Error> {
        match request {
            Request::NoDelay { socket, enabled } => {
                self.no_delay(socket, enabled).map(Response::NoDelay)
            }
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
                let ready = Arc::new(AtomicBool::new(duration.0 == 0));
                let task = if duration.0 == 0 {
                    None
                } else {
                    let runtime = bindings
                        .rt()
                        .ok_or(Error::Unsupported("native timer without reactor"))?
                        .clone();
                    let future = bindings
                        .alloc(
                            wisp_abi::native_io::canonical::POLL_IMPORT,
                        )
                        .ok_or(Error::ResourceLimit)?;
                    let completion = runtime.clone();
                    let ready = ready.clone();
                    let task = runtime.reactor.spawn(async move {
                        let duration_ms =
                            duration.0.div_ceil(wisp_abi::pal::CLOCK_QUANTUM_NS as u64);
                        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                        ready.store(true, Ordering::Release);
                        completion.settle_future(future, ResultRecord::Ok(Vec::new()));
                    });
                    runtime.futures.attach_task(future, task.abort_handle());
                    Some(ReactorWork {
                        future: FutureId::from(future),
                        owner: bindings.ctx.instance,
                        table: runtime.futures.clone(),
                        task: task.abort_handle(),
                    })
                };
                self.insert(Resource::Timer(Timer { ready, _task: task }))
                    .map(Response::Handle)
            }
            Request::Ready(handle) => self.ready(handle).map(Response::Ready),
            Request::Poll(handles) => {
                let mut indexes = Vec::new();
                for (index, handle) in handles.into_iter().enumerate() {
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

    /// A stream the guest polled and found not ready is one it waits on:
    /// arms the readiness wake a blocked read or send would, so a guest that
    /// checks readiness instead of attempting the operation (a zero-byte
    /// read, `Socket.Poll`) has a wake to park on. Every other pollable's
    /// wake exists from the operation that created it.
    fn arm(&mut self, handle: ResourceHandle, bindings: &CellBindings) -> Result<(), Error> {
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
        bindings: &CellBindings,
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
            .rt()
            .ok_or(Error::Unsupported("outbound connect without reactor"))?
            .clone();
        let policy = runtime.services.egress.clone();
        let owner =
            crate::egress::ConnectionOwner::new(bindings.ctx.tenant.clone(), bindings.ctx.instance)
                .map_err(Error::from)?;
        let permit = policy
            .check_socket(
                destination,
                if self.has_resolved_address(destination.ip()) {
                    None
                } else {
                    bindings.ctx.allowlist.as_deref()
                },
            )
            .map_err(|reason| Error::Denied(reason.to_string()))?;
        let future = bindings
            .alloc("socket_call")
            .ok_or(Error::ResourceLimit)?;
        *state.lock() = SocketState::Connecting;

        let completion = runtime.clone();
        let tenant = bindings.ctx.tenant.clone();
        let instance = bindings.ctx.instance;
        let task = runtime.reactor.spawn(async move {
            let result = async {
                let stream = policy.connect(&owner, destination).await.map_err(Error::from)?;
                Ok(Connection {
                    stream: Arc::new(stream),
                    permit,
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
                owner: bindings.ctx.instance,
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
    use wisp_abi::native_io::DurationNanos;

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

    #[test]
    fn cancellation_refuses_new_deadlines_but_allows_polling_and_disposal() {
        use wisp_engine::HostBindings;
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
                _task: None,
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
                _task: None,
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
