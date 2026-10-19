//! Bounded stream buffers and their reactor ownership.

use super::*;
use isoplane_abi::native_io::ByteCount;
use std::future::Future;

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

/// Runs `operation` on the reactor as a bounded socket future and settles
/// it when the operation completes: the wake source for one stream's
/// readiness.
fn watch(
    bindings: &dyn Context,
    operation: impl Future<Output = ()> + Send + 'static,
) -> Result<ReactorWork, Error> {
    let runtime = bindings
        .runtime()
        .ok_or(Error::Unsupported("stream operation without reactor"))?
        .clone();
    let future = bindings
        .alloc(isoplane_abi::native_io::canonical::SOCKET_IMPORT)
        .ok_or(Error::ResourceLimit)?;
    let completion = runtime.clone();
    let span = tracing::info_span!("native_readiness", tenant = %bindings.tenant(), instance = %bindings.instance());
    let task = runtime.spawn(tracing::Instrument::instrument(
        async move {
            operation.await;
            completion.settle_future(future, ResultRecord::Ok(Vec::new()));
        },
        span,
    ));
    runtime.futures.attach_task(future, task.abort_handle());
    Ok(ReactorWork {
        future: FutureId::from(future),
        owner: bindings.instance(),
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
        bindings: &dyn Context,
    ) -> Result<Response, Error> {
        let length = usize::try_from(length.0)
            .unwrap_or(usize::MAX)
            .min(isoplane_abi::imports::MAX_OUT_BUFFER_BYTES);
        if length == 0 {
            return Ok(Response::Bytes(Vec::new()));
        }
        let stream = connection(&self.state)?;
        let mut bytes = vec![0; length];
        match stream.stream.try_read(&mut bytes) {
            Ok(0) => Ok(Response::Closed),
            Ok(count) => {
                bytes.truncate(count);
                Ok(Response::Bytes(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                self.arm(bindings)?;
                Ok(Response::Bytes(Vec::new()))
            }
            Err(error) => Err(Error::from(error)),
        }
    }

    /// Arms the wake for the stream becoming readable, unless one is armed.
    pub(super) fn arm(&mut self, bindings: &dyn Context) -> Result<(), Error> {
        if self.task.as_ref().is_some_and(ReactorWork::is_pending) {
            return Ok(());
        }
        let stream = connection(&self.state)?;
        self.task = Some(watch(bindings, async move {
            if let Err(error) = stream.stream.readable().await {
                tracing::warn!(%error, "native input readiness failed");
            }
        })?);
        Ok(())
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

    pub(super) fn send(&mut self, bytes: &[u8], bindings: &dyn Context) -> Result<Response, Error> {
        let stream = connection(&self.state)?;
        match stream.stream.try_write(bytes) {
            Ok(count) => Ok(Response::Capacity(ByteCount(count as u64))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                self.arm(bindings)?;
                Err(Error::WouldBlock)
            }
            Err(error) => Err(Error::from(error)),
        }
    }

    /// Arms the wake for the stream becoming writable, unless one is armed.
    pub(super) fn arm(&mut self, bindings: &dyn Context) -> Result<(), Error> {
        if self.task.as_ref().is_some_and(ReactorWork::is_pending) {
            return Ok(());
        }
        let stream = connection(&self.state)?;
        self.task = Some(watch(bindings, async move {
            if let Err(error) = stream.stream.writable().await {
                tracing::warn!(%error, "native output readiness failed");
            }
        })?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{CellRuntime, InstanceCtx, Services, WakeSink};
    use crate::egress::{DestinationPolicy, HostEgressPolicy, Resolver};
    use crate::fakes::FakeFetcher;
    use isoplane_abi::config::Limits;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    async fn connected() -> (Arc<Mutex<SocketState>>, TcpStream, CellBindings, Arc<Wake>) {
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
            HostEgressPolicy::new(
                Box::new(NoDns),
                crate::egress::ConnectionLimits::uniform(1).expect("connection limits"),
            )
            .with_destinations(Arc::new(OwnedPeer(address))),
        );
        // The test opens the socket itself to control its send buffer, so the
        // attempt only supplies the permit.
        let permit = policy
            .begin_connect(
                crate::egress::ConnectionOwner::new("tenant".into(), InstanceId(83))
                    .expect("fixture owner"),
            )
            .await
            .expect("the instance must acquire its open permit")
            .established();
        let wake = Arc::new(Wake(tokio::sync::Notify::new()));
        let mut context = InstanceCtx::for_test(InstanceId(83), Limits::default());
        context.rt = Some(Arc::new(CellRuntime {
            runtime_configuration: Default::default(),
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
            _permit: permit,
        }))));
        (
            state,
            peer.expect("the owned peer must accept").0,
            CellBindings::new(Arc::new(context)),
            wake,
        )
    }

    #[tokio::test]
    async fn polling_an_unreadable_input_arms_the_wake_a_blocked_read_would() {
        use super::super::{Request, ResourceCount, ResourceLimits, Resources};
        let (state, mut peer, bindings, wake) = connected().await;
        let mut resources = Resources::new(ResourceLimits {
            handles: ResourceCount(4),
        });
        let input = resources
            .insert(Resource::Input(Input::new(state)))
            .expect("the test input must fit its resource budget");
        let Ok(Response::Handle(subscription)) =
            resources.call(Request::Subscribe(input), &bindings)
        else {
            panic!("subscribing to an input must return its pollable")
        };
        assert!(
            matches!(
                resources.call(Request::Poll(vec![subscription]), &bindings),
                Err(Error::Unsupported(_))
            ),
            "a poll finding nothing ready still refuses to wait synchronously"
        );
        let runtime = bindings.rt().expect("the stream test must have a reactor");
        assert_eq!(
            runtime.futures.pending_total(bindings.ctx.instance),
            1,
            "a zero-byte read implemented as a poll must leave the guest one readiness wake to park on"
        );
        assert!(
            matches!(
                resources.call(Request::Poll(vec![subscription]), &bindings),
                Err(Error::Unsupported(_))
            ) && runtime.futures.pending_total(bindings.ctx.instance) == 1,
            "polling again while the wake is armed must not arm a second one"
        );
        peer.write_all(b"ping")
            .await
            .expect("the peer must deliver bytes");
        tokio::time::timeout(std::time::Duration::from_secs(60), wake.0.notified())
            .await
            .expect("the peer's bytes must wake the polling instance");
        assert!(
            runtime.futures.take_ready(bindings.ctx.instance).is_some(),
            "the readable wake must belong to the polling instance"
        );
        assert!(
            matches!(
                resources.call(Request::Poll(vec![subscription]), &bindings),
                Ok(Response::ReadyIndexes(indexes)) if indexes == vec![0]
            ),
            "the input reads as ready once the bytes arrived"
        );
    }

    #[tokio::test]
    async fn a_full_socket_parks_one_output_wake_and_preserves_partial_send_counts() {
        let (state, mut peer, bindings, wake) = connected().await;
        let mut output = Output::new(state);
        let sent = fill_output(&mut output, &bindings);
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

    struct HeldWake {
        signalled: tokio::sync::Notify,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl WakeSink for HeldWake {
        fn signal(&self, _: InstanceId) {
            let release = self.release.lock().take();
            self.signalled.notify_one();
            if let Some(release) = release {
                release
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect(
                    "the test must explicitly release the settled watcher before the hang deadline",
                );
            }
        }
    }

    fn hold_first_wake(
        bindings: &mut CellBindings,
    ) -> (Arc<HeldWake>, std::sync::mpsc::SyncSender<()>) {
        let (release, receiver) = std::sync::mpsc::sync_channel(0);
        let wake = Arc::new(HeldWake {
            signalled: tokio::sync::Notify::new(),
            release: Mutex::new(Some(receiver)),
        });
        let context = Arc::get_mut(&mut bindings.ctx)
            .expect("the newly connected fixture owns its context exclusively");
        let runtime = Arc::get_mut(context.rt.as_mut().expect("test runtime"))
            .expect("the new context exclusively owns its runtime");
        runtime.wake = wake.clone();
        (wake, release)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delivered_readiness_wake_can_rearm_before_its_old_task_returns() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (state, mut peer, mut bindings, _) = connected().await;
            let (wake, release) = hold_first_wake(&mut bindings);
            let mut input = Input::new(state);
            assert!(matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes.is_empty()), "the first read must wait for the real peer");
            let old = input.task.as_ref().expect("first watcher").task.clone();
            peer.write_all(b"a").await.expect("first peer byte");
            wake.signalled.notified().await;
            let runtime = bindings.rt().expect("runtime");
            assert!(runtime.futures.take_ready(bindings.ctx.instance).is_some(), "the first readiness result must already be deliverable");
            assert!(!old.is_finished(), "the explicit wake barrier must keep the settled watcher running");
            assert!(matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes == b"a"), "the first ready read must consume the peer byte");
            assert!(matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes.is_empty()), "the drained stream must wait again");
            assert_eq!(runtime.futures.pending_total(bindings.ctx.instance), 1, "a drained stream must acquire a new wake even while the delivered watcher is still returning");
            peer.write_all(b"b").await.expect("second peer byte");
            wake.signalled.notified().await;
            assert!(runtime.futures.take_ready(bindings.ctx.instance).is_some(), "later input must wake the rearmed stream without a timer or retry");
            assert!(matches!(input.read(ByteCount(1), &bindings), Ok(Response::Bytes(bytes)) if bytes == b"b"), "the second wake must expose the second real peer byte");
            release.send(()).expect("release old watcher");
        }).await.expect("the rearm regression must not hang");
    }

    fn fill_output(output: &mut Output, bindings: &CellBindings) -> usize {
        let bytes = vec![0x5a; 65536];
        let mut sent = 0;
        loop {
            match output.send(&bytes, bindings) {
                Ok(Response::Capacity(count)) => {
                    assert!(
                        count.0 > 0 && count.0 <= bytes.len() as u64,
                        "a real partial send must report exactly accepted bytes"
                    );
                    sent += count.0 as usize;
                }
                Err(Error::WouldBlock) => return sent,
                result => panic!(
                    "controlled output backpressure produced an unexpected result: {result:?}"
                ),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delivered_output_wake_can_rearm_before_its_old_task_returns() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (state, mut peer, mut bindings, _) = connected().await;
            let (wake, release) = hold_first_wake(&mut bindings);
            let mut output = Output::new(state);
            let first_bytes = fill_output(&mut output, &bindings);
            let old = output.task.as_ref().expect("first output watcher").task.clone();
            let mut received = vec![0; first_bytes];
            peer.read_exact(&mut received).await.expect("drain first output");
            wake.signalled.notified().await;
            let runtime = bindings.rt().expect("runtime");
            assert!(runtime.futures.take_ready(bindings.ctx.instance).is_some(), "the first writable result must already be deliverable");
            assert!(!old.is_finished(), "the explicit wake barrier must keep the settled output watcher running");
            let second_bytes = fill_output(&mut output, &bindings);
            assert_eq!(runtime.futures.pending_total(bindings.ctx.instance), 1, "new backpressure must acquire a new wake before the delivered output watcher returns");
            received.resize(second_bytes, 0);
            peer.read_exact(&mut received).await.expect("drain rearmed output");
            wake.signalled.notified().await;
            assert!(runtime.futures.take_ready(bindings.ctx.instance).is_some(), "later peer drainage must wake the rearmed sender without a timer or retry");
            release.send(()).expect("release old output watcher");
        }).await.expect("the output rearm regression must not hang");
    }

    /// Releases a slot's host resources without guest disposal and checks that
    /// its real socket closes, its pending input is cancelled and its open
    /// permit is returned.
    async fn host_cleanup_closes_the_socket_and_returns_its_permit(
        cleanup: impl FnOnce(&crate::slot::Slot),
    ) {
        use crate::Activator;
        use isoplane_engine::HostBindings;
        let (state, mut peer, mut bindings, _) = connected().await;
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
        let runtime = bindings
            .rt()
            .expect("the pending input requires its reactor")
            .clone();
        assert_eq!(
            runtime.services.egress.instance_connections(slot.id),
            1,
            "the connected socket holds its instance's open permit"
        );
        cleanup(&slot);
        assert_eq!(
            runtime.futures.pending_total(slot.id),
            0,
            "cleanup must remove native wake ownership before the reactor observes cancellation"
        );
        assert!(
            matches!(bindings.native_io(Request::Read { stream: input, length: ByteCount(1) }), Err(Error::InvalidHandle(handle)) if handle == input),
            "cleanup must invalidate every guest reference to the old input"
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while runtime.services.egress.instance_connections(slot.id) != 0 {
                tokio::task::yield_now().await;
            }
            let mut byte = [0];
            assert_eq!(
                peer.read(&mut byte)
                    .await
                    .expect("the peer must observe cleanup"),
                0,
                "cleanup must close the actual host socket without running guest disposal"
            );
        })
        .await
        .expect("cleanup must finish before the test deadline");
    }

    #[tokio::test]
    async fn evicting_a_slot_closes_its_real_socket_and_returns_its_permit() {
        host_cleanup_closes_the_socket_and_returns_its_permit(crate::slot::Slot::discard).await;
    }

    #[tokio::test]
    async fn dropping_a_pending_input_removes_its_wake_before_late_peer_data() {
        use tokio::io::AsyncWriteExt;
        let (state, mut peer, bindings, _) = connected().await;
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
            runtime.services.egress.instance_connections(InstanceId(83)),
            0,
            "host cleanup must release the connection without managed disposal"
        );
    }

    #[tokio::test]
    async fn a_peer_reset_completes_a_pending_read_with_an_error() {
        let (state, peer, bindings, wake) = connected().await;
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
            runtime.services.egress.instance_connections(InstanceId(83)),
            0,
            "the reset connection must release quota when its last owner drops"
        );
    }
}
