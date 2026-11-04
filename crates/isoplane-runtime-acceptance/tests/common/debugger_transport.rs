//! Fragmented debugger packets through the runtime's real socket transport.

use isoplane_harness::logging::GuestLogs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_abi::native_io::{ByteCount, Error, Request, Response};
use isoplane_engine::{HostBindings, ImportCall, OccupancyBudget, PlatformFile, PlatformFiles};
use isoplane_harness::{Network, Reactor};
use isoplane_host::bindings::CellBindings;
use isoplane_host::debug::DebugActivator;
use isoplane_host::egress::{AdditionalDestinations, PublicDestinations};
use isoplane_host::{Activator, InstanceId, ModuleId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, Debug)]
enum Fragment {
    Header,
    Payload,
}

struct FragmentedReads {
    inner: CellBindings,
    fragment: Fragment,
    received_bytes: usize,
    injected: Arc<AtomicBool>,
}

impl HostBindings for FragmentedReads {
    fn begin_slice(&mut self) {}
    fn end_slice(&mut self) {}
    fn native_io(&mut self, request: Request) -> Result<Response, Error> {
        let Request::Read { stream, length } = request else {
            return self.inner.native_io(request);
        };
        // The handshake is 13 bytes. Pause after part of the first 11-byte
        // header, or after its entire header before any payload is available.
        let gap_after_bytes = match self.fragment {
            Fragment::Header => 16,
            Fragment::Payload => 24,
        };
        if self.received_bytes == gap_after_bytes && !self.injected.swap(true, Ordering::SeqCst) {
            return Ok(Response::Bytes(Vec::new()));
        }
        let length = if matches!(self.fragment, Fragment::Header) && self.received_bytes == 13 {
            ByteCount(length.0.min(3))
        } else {
            length
        };
        let result = self.inner.native_io(Request::Read { stream, length })?;
        if let Response::Bytes(bytes) = &result {
            self.received_bytes += bytes.len();
        }
        Ok(result)
    }

    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        self.inner.call_future(call)
    }

    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        self.inner.call_sync(call)
    }

    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        output: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        self.inner.call_fill(call, output)
    }

    fn slice_clock_ms(&self) -> u64 {
        self.inner.slice_clock_ms()
    }
}

struct StopOnDrop(Arc<DebugActivator>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

async fn packet(stream: &mut TcpStream) -> Vec<u8> {
    let mut header = [0; 11];
    stream
        .read_exact(&mut header)
        .await
        .expect("debugger packet header");
    let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    assert!(
        (11..65536).contains(&length),
        "invalid debugger packet length: {length}"
    );
    let mut result = vec![0; length];
    result[..11].copy_from_slice(&header);
    stream
        .read_exact(&mut result[11..])
        .await
        .expect("debugger packet payload");
    result
}

async fn command(stream: &mut TcpStream, id: u32, command: u8, payload: &[u8]) -> Vec<u8> {
    let mut request = Vec::new();
    request.extend_from_slice(&u32::try_from(11 + payload.len()).unwrap().to_be_bytes());
    request.extend_from_slice(&id.to_be_bytes());
    request.extend_from_slice(&[0, 1, command]);
    request.extend_from_slice(payload);
    stream
        .write_all(&request)
        .await
        .expect("send debugger command");
    let response = packet(stream).await;
    assert_eq!(&response[4..8], id.to_be_bytes());
    assert_eq!(
        &response[8..11],
        &[0x80, 0, 0],
        "command {command} must succeed"
    );
    response
}

async fn exchange(listener: &TcpListener) {
    let (mut stream, _) = listener.accept().await.expect("guest debugger connects");
    println!("The guest connected to the debugger peer.");
    let mut handshake = [0; 13];
    stream
        .read_exact(&mut handshake)
        .await
        .expect("guest debugger handshake");
    assert_eq!(&handshake, b"DWP-Handshake");
    stream
        .write_all(&handshake)
        .await
        .expect("acknowledge debugger handshake");
    let start = packet(&mut stream).await;
    println!("The debugger handshake and initial event arrived.");
    assert_eq!(
        &start[8..11],
        &[0, 64, 100],
        "guest must send its initial event"
    );
    // Mono's SetProtocolVersion has a body; Version has none.
    command(&mut stream, 1, 8, &[0, 0, 0, 2, 0, 0, 0, 64]).await;
    let version = command(&mut stream, 2, 1, &[]).await;
    assert!(version[11..].windows(5).any(|bytes| bytes == b"mono "));
    // Closing at a suspended command boundary must release the guest too.
}

#[tokio::test]
async fn partial_debugger_headers_and_delayed_payloads_preserve_packet_boundaries() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let mut files = super::package("alpha");
    files.push(PlatformFile {
        path: isoplane_abi::filesystem::DEBUG_AGENT_PATH.into(),
        bytes: endpoint.to_string().into_bytes().into(),
        modified_ns: 0,
    });
    let engine = Arc::new(
        isoplane_engine::IsoplaneEngine::new(isoplane_harness::engine_configuration(
            PlatformFiles::new(files).unwrap(),
        ))
        .unwrap(),
    );
    let template = engine
        .load_template(&isoplane_fixtures::mono_debug_runtime())
        .unwrap();
    let activator = Arc::new(
        DebugActivator::new(engine, ModuleId::new("debug-transport", "v1"), template).unwrap(),
    );
    let _stop = StopOnDrop(activator.clone());
    for fragment in [Fragment::Header, Fragment::Payload] {
        let network = Network::default().with_destinations(Arc::new(AdditionalDestinations {
            endpoints: vec![endpoint],
            baseline: Arc::new(PublicDestinations),
        }));
        let reactor = Reactor::with_network(InstanceId::alloc(), network);
        reactor.context.begin_dispatch(1, 1);
        let injected = Arc::new(AtomicBool::new(false));
        let bindings = FragmentedReads {
            inner: CellBindings::new(reactor.context.clone()),
            fragment,
            received_bytes: 0,
            injected: injected.clone(),
        };
        let mut instance = activator.activate(Box::new(bindings)).unwrap();
        let guest = tokio::task::spawn_blocking(move || {
            instance.dispatch(
                &DispatchRecord {
                    dispatch_id: 1,
                    kind: DispatchKind::Http(HttpRequest {
                        method: "GET".into(),
                        url: "http://guest.test/greeting".into(),
                        path: "/greeting".into(),
                        query: vec![],
                        headers: vec![],
                        body: vec![],
                    }),
                    trace: TraceContext::default(),
                    cancel_future: 0,
                    occupancy_budget_ms: 180_000,
                    pending_futures_limit: 64,
                    now_ms: 1,
                },
                OccupancyBudget {
                    occupancy_ms: 180_000,
                    slice_wall_ms_cap: 180_000,
                    spent_ns: 0,
                },
            )
        });
        let completed = tokio::time::timeout(Duration::from_secs(180), async {
            tokio::join!(exchange(&listener), async {
                let result = guest.await.expect("guest worker must return to the host");
                // Alpha parks on a host timer after the debugger releases its initial stop.
                assert_eq!(
                    result.completion,
                    isoplane_engine::Completion::Parked,
                    "{fragment:?}: {result:?}; logs: {:?}",
                    reactor.context.logs()
                );
            });
        })
        .await;
        assert!(
            completed.is_ok(),
            "the {fragment:?} exchange must finish and the detached guest must reach its host timer; injected: {}; logs: {:?}",
            injected.load(Ordering::SeqCst),
            reactor.context.logs()
        );
        assert!(
            injected.load(Ordering::SeqCst),
            "the {fragment:?} read gap must be exercised"
        );
    }
}
