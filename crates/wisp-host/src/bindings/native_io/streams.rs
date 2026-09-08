//! Bounded stream buffers and their reactor ownership.

use super::*;
use std::future::Future;
use wisp_abi::native_io::ByteCount;

pub(super) struct Input {
    state: Arc<Mutex<SocketState>>,
    task: Option<ReactorWork>,
}

pub(super) struct Output {
    state: Arc<Mutex<SocketState>>,
    task: Option<ReactorWork>,
}

pub(super) fn connection(state: &Mutex<SocketState>) -> Result<Arc<Connection>, Error> {
    match &*state.lock() {
        SocketState::Connected(connection) => Ok(connection.clone()),
        SocketState::Failed(error) => Err(error.clone()),
        SocketState::Created(_) | SocketState::Connecting => {
            Err(Error::InvalidState("stream is not connected"))
        }
    }
}

fn watch(
    bindings: &CellBindings,
    operation: impl Future<Output = ()> + Send + 'static,
) -> Result<ReactorWork, Error> {
    let runtime = bindings
        .rt()
        .ok_or(Error::Unsupported("stream operation without reactor"))?
        .clone();
    let future = bindings
        .alloc(wisp_abi::native_io::canonical::SOCKET_IMPORT)
        .ok_or(Error::ResourceLimit)?;
    let completion = runtime.clone();
    let span = tracing::info_span!("native_readiness", tenant = %bindings.ctx.tenant, instance = %bindings.ctx.instance);
    let task = runtime.reactor.spawn(tracing::Instrument::instrument(
        async move {
            operation.await;
            completion.settle_future(future, ResultRecord::Ok(Vec::new()));
        },
        span,
    ));
    runtime.futures.attach_task(future, task.abort_handle());
    Ok(ReactorWork {
        future: FutureId::from(future),
        owner: bindings.ctx.instance,
        table: runtime.futures.clone(),
        task: task.abort_handle(),
    })
}

impl Input {
    pub(super) fn new(state: Arc<Mutex<SocketState>>) -> Self {
        Self { state, task: None }
    }

    pub(super) fn ready(&self) -> Result<bool, Error> {
        let stream = connection(&self.state)?;
        match stream
            .stream
            .try_io(tokio::io::Interest::READABLE, || Ok(()))
        {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(Error::from(error)),
        }
    }

    pub(super) fn read(
        &mut self,
        length: ByteCount,
        bindings: &CellBindings,
    ) -> Result<Response, Error> {
        let length = usize::try_from(length.0)
            .unwrap_or(usize::MAX)
            .min(wisp_abi::imports::MAX_OUT_BUFFER_BYTES);
        if length == 0 {
            return Ok(Response::Bytes(Vec::new()));
        }
        let stream = connection(&self.state)?;
        let reservation = stream
            .permit
            .reserve_bytes(ByteCount(length as u64))
            .map_err(|error| Error::Denied(error.to_string()))?;
        let mut bytes = vec![0; reservation.available().0 as usize];
        match stream.stream.try_read(&mut bytes) {
            Ok(0) => Ok(Response::Closed),
            Ok(count) => {
                reservation.commit(ByteCount(count as u64));
                bytes.truncate(count);
                Ok(Response::Bytes(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if self
                    .task
                    .as_ref()
                    .is_none_or(|work| work.task.is_finished())
                {
                    self.task = Some(watch(bindings, async move {
                        if let Err(error) = stream.stream.readable().await {
                            tracing::warn!(%error, "native input readiness failed");
                        }
                    })?);
                }
                Ok(Response::Bytes(Vec::new()))
            }
            Err(error) => Err(Error::from(error)),
        }
    }
}

impl Output {
    pub(super) fn new(state: Arc<Mutex<SocketState>>) -> Self {
        Self { state, task: None }
    }

    pub(super) fn ready(&self) -> Result<bool, Error> {
        let stream = connection(&self.state)?;
        match stream
            .stream
            .try_io(tokio::io::Interest::WRITABLE, || Ok(()))
        {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(Error::from(error)),
        }
    }

    pub(super) fn send(
        &mut self,
        bytes: &[u8],
        bindings: &CellBindings,
    ) -> Result<Response, Error> {
        let stream = connection(&self.state)?;
        let reservation = stream
            .permit
            .reserve_bytes(ByteCount(bytes.len() as u64))
            .map_err(|error| Error::Denied(error.to_string()))?;
        match stream
            .stream
            .try_write(&bytes[..reservation.available().0 as usize])
        {
            Ok(count) => {
                reservation.commit(ByteCount(count as u64));
                Ok(Response::Capacity(ByteCount(count as u64)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if self
                    .task
                    .as_ref()
                    .is_none_or(|work| work.task.is_finished())
                {
                    self.task = Some(watch(bindings, async move {
                        if let Err(error) = stream.stream.writable().await {
                            tracing::warn!(%error, "native output readiness failed");
                        }
                    })?);
                }
                Err(Error::WouldBlock)
            }
            Err(error) => Err(Error::from(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{CellRuntime, InstanceCtx, Services, WakeSink};
    use crate::egress::{DestinationPolicy, HostEgressPolicy, Resolver};
    use crate::fakes::FakeFetcher;
    use tokio::io::AsyncReadExt;
    use wisp_abi::config::{HandlerLifecycle, Limits};

    struct NoDns;
    #[async_trait::async_trait]
    impl Resolver for NoDns {
        async fn resolve(
            &self,
            _: &str,
            _: u16,
        ) -> Result<Vec<std::net::SocketAddr>, crate::egress::DnsError> {
            panic!("a numeric stream test must not resolve a name")
        }
    }

    struct OwnedPeer(std::net::SocketAddr);
    impl DestinationPolicy for OwnedPeer {
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

    struct Wake(tokio::sync::Notify);
    impl WakeSink for Wake {
        fn signal(&self, _: InstanceId) {
            self.0.notify_one();
        }
    }

    async fn connected(
        max_bytes: ByteCount,
    ) -> (Arc<Mutex<SocketState>>, TcpStream, CellBindings, Arc<Wake>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the owned stream peer must bind");
        let address = listener
            .local_addr()
            .expect("the owned stream peer must have an endpoint");
        let socket = tokio::net::TcpSocket::new_v4().expect("the controlled test socket must open");
        socket
            .set_send_buffer_size(4096)
            .expect("the test must control its socket's send capacity");
        let (client, peer) = tokio::join!(socket.connect(address), listener.accept());
        let policy = Arc::new(
            HostEgressPolicy::new(Box::new(NoDns), 1, max_bytes.0)
                .with_destinations(Arc::new(OwnedPeer(address))),
        );
        let permit = policy
            .check_socket(address, None)
            .expect("the exact owned peer must acquire quota");
        let wake = Arc::new(Wake(tokio::sync::Notify::new()));
        let mut context =
            InstanceCtx::for_test(InstanceId(83), HandlerLifecycle::Reuse, Limits::default());
        context.rt = Some(Arc::new(CellRuntime {
            services: Arc::new(Services {
                egress: policy,
                fetcher: Arc::new(FakeFetcher::default()),
            }),
            futures: Arc::new(FutureTable::new()),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            wake: wake.clone(),
            reactor: tokio::runtime::Handle::current(),
        }));
        context.begin_slice(1, 1);
        let state = Arc::new(Mutex::new(SocketState::Connected(Arc::new(Connection {
            stream: Arc::new(client.expect("the controlled socket must connect")),
            permit,
        }))));
        (
            state,
            peer.expect("the owned peer must accept").0,
            CellBindings::new(Arc::new(context)),
            wake,
        )
    }

    #[tokio::test]
    async fn a_full_socket_parks_one_output_wake_and_preserves_partial_send_counts() {
        let (state, mut peer, bindings, wake) = connected(ByteCount(1 << 30)).await;
        let mut output = Output::new(state);
        let bytes = vec![0x5a; 65536];
        let mut sent = 0;
        loop {
            match output.send(&bytes, &bindings) {
                Ok(Response::Capacity(count)) => {
                    assert!(
                        count.0 > 0 && count.0 <= bytes.len() as u64,
                        "each send must report only bytes accepted by the real socket"
                    );
                    sent += count.0 as usize;
                }
                Err(Error::WouldBlock) => break,
                result => panic!(
                    "controlled output backpressure produced an unexpected result: {result:?}"
                ),
            }
        }
        let runtime = bindings.rt().expect("the stream test must have a reactor");
        assert_eq!(
            runtime.futures.pending_total(bindings.ctx.instance),
            1,
            "backpressure must retain exactly one output readiness task"
        );
        let mut received = vec![0; sent];
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            peer.read_exact(&mut received),
        )
        .await
        .expect("the peer must drain before the test deadline")
        .expect("the peer must receive accepted bytes");
        assert!(
            received.iter().all(|byte| *byte == 0x5a),
            "partial sends must preserve every accepted byte"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), wake.0.notified())
            .await
            .expect("draining the peer must wake the blocked sender");
        assert!(
            runtime.futures.take_ready(bindings.ctx.instance).is_some(),
            "the writable wake must belong to the blocked instance"
        );
        assert!(
            matches!(
                output.send(b"x", &bindings),
                Ok(Response::Capacity(ByteCount(1)))
            ),
            "a writable socket must accept the remaining byte"
        );
    }

    #[tokio::test]
    async fn resetting_a_slot_closes_its_real_socket_and_cancels_pending_input() {
        use crate::Activator;
        use wisp_engine::HostBindings;
        let (state, mut peer, mut bindings, _) = connected(ByteCount(1024)).await;
        let input = bindings
            .ctx
            .native_io
            .lock()
            .insert(Resource::Input(Input::new(state)))
            .expect("the pending input must fit the instance's resource budget");
        let result = bindings.native_io(Request::Read {
            stream: input,
            length: ByteCount(1),
        });
        assert!(
            matches!(result, Ok(Response::Bytes(bytes)) if bytes.is_empty()),
            "an idle peer must leave a host-owned read pending"
        );
        let activator = crate::fakes::FakeActivator::new(crate::fakes::FakeBehaviour::done());
        let instance = activator
            .activate(CellBindings::boxed(bindings.ctx.clone()))
            .expect("the slot fixture must activate");
        let slot = crate::slot::Slot::new(bindings.ctx.instance, bindings.ctx.clone(), instance);
        slot.reset()
            .expect("reset must return an idle slot to its template");
        let runtime = bindings
            .rt()
            .expect("the pending input requires its reactor")
            .clone();
        assert_eq!(
            runtime.futures.pending_total(slot.id),
            0,
            "reset must remove native wake ownership before the reactor observes cancellation"
        );
        assert!(
            matches!(bindings.native_io(Request::Read { stream: input, length: ByteCount(1) }), Err(Error::InvalidHandle(handle)) if handle == input),
            "reset must invalidate every guest reference to the old input"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while runtime.services.egress.connections() != 0 {
                tokio::task::yield_now().await;
            }
            let mut byte = [0];
            assert_eq!(
                peer.read(&mut byte)
                    .await
                    .expect("the peer must observe reset cleanup"),
                0,
                "reset must close the actual host socket without running guest disposal"
            );
        })
        .await
        .expect("reset cleanup must finish before the test deadline");
    }

    #[tokio::test]
    async fn dropping_a_pending_input_removes_its_wake_before_late_peer_data() {
        use tokio::io::AsyncWriteExt;
        let (state, mut peer, bindings, _) = connected(ByteCount(1024)).await;
        let mut input = Input::new(state.clone());
        assert!(
            matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes.is_empty()),
            "an empty peer must arm input readiness without blocking the worker"
        );
        let runtime = bindings.rt().expect("the input test must have its reactor");
        assert_eq!(
            runtime.futures.pending_total(bindings.ctx.instance),
            1,
            "the pending read must own exactly one wake"
        );
        let abort = input
            .task
            .as_ref()
            .expect("the pending read must retain its task")
            .task
            .clone();
        drop(input);
        assert_eq!(
            runtime.futures.pending_total(bindings.ctx.instance),
            0,
            "dropping input must remove its future before cancellation is processed"
        );
        peer.write_all(b"late")
            .await
            .expect("the peer may race with host cancellation");
        drop(state);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the cancelled readiness task must terminate before the deadline");
        assert!(
            runtime.futures.take_ready(bindings.ctx.instance).is_none(),
            "late peer data must not recreate a dropped input wake"
        );
        assert_eq!(
            runtime.services.egress.connections(),
            0,
            "host cleanup must release the connection without managed disposal"
        );
    }

    #[tokio::test]
    async fn a_peer_reset_completes_a_pending_read_with_an_error() {
        let (state, peer, bindings, wake) = connected(ByteCount(1024)).await;
        let mut input = Input::new(state);
        assert!(
            matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes.is_empty()),
            "a read must wait for peer input before the controlled reset"
        );
        socket2::SockRef::from(&peer)
            .set_linger(Some(std::time::Duration::ZERO))
            .expect("the test must configure an immediate peer reset");
        drop(peer);
        tokio::time::timeout(std::time::Duration::from_secs(60), wake.0.notified())
            .await
            .expect("the peer reset must wake the pending reader");
        let runtime = bindings.rt().expect("the reset test must own a reactor");
        assert!(
            runtime.futures.take_ready(bindings.ctx.instance).is_some(),
            "the reset wake must belong to the waiting instance"
        );
        assert!(
            matches!(
                input.read(ByteCount(1), &bindings),
                Err(Error::Connection {
                    kind: std::io::ErrorKind::ConnectionReset,
                    ..
                })
            ),
            "a reset must be delivered as an error rather than clean EOF or empty input"
        );
        drop(input);
        assert_eq!(
            runtime.services.egress.connections(),
            0,
            "the reset connection must release quota when its last owner drops"
        );
    }

    #[tokio::test]
    async fn a_send_cannot_exceed_its_remaining_byte_quota() {
        let (state, mut peer, bindings, _) = connected(ByteCount(3)).await;
        let mut output = Output::new(state);
        assert!(
            matches!(
                output.send(b"four", &bindings),
                Ok(Response::Capacity(ByteCount(3)))
            ),
            "a send must stop at its remaining byte quota"
        );
        assert!(
            matches!(output.send(b"r", &bindings), Err(Error::Denied(_))),
            "an exhausted byte quota must refuse another send"
        );
        let mut received = [0; 3];
        peer.read_exact(&mut received)
            .await
            .expect("the peer must receive the admitted bytes");
        assert_eq!(
            &received, b"fou",
            "a denied suffix must never reach the real peer"
        );
    }
}
