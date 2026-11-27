use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use isoplane_host::{InstanceId, LifecycleTier};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::common;

struct Client {
    response: Response<Incoming>,
    _connection: common::HttpConnection,
}

async fn start() -> (common::Rig, TcpListener) {
    let gate = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the test must own its release endpoint");
    let endpoint = gate.local_addr().expect("the gate must have an address");
    let rig = common::start_with_policy(
        common::preparation::Guest::from("isoplane_guest_aspnet"),
        Some(Arc::new(crate::owned_endpoint::OwnedEndpoint(endpoint))),
        |cfg| {
            cfg.proxy.max_in_flight = 1;
            cfg.proxy.max_queued = 0;
            cfg.response_queue_bytes = 1;
        },
    )
    .await;
    (rig, gate)
}

async fn first_chunk(rig: &common::Rig, gate: &TcpListener) -> Client {
    let mut connection = common::HttpConnection::connect(rig.proxy).await;
    let request = Request::builder()
        .uri(format!(
            "/stream?port={}",
            gate.local_addr().expect("gate address").port()
        ))
        .header("host", "quotes.isoplane.dev")
        .body(Full::new(Bytes::new()))
        .expect("valid streaming request");
    let response = connection.request(request).await;
    let mut client = Client {
        response,
        _connection: connection,
    };
    assert_eq!(
        client.response.status(),
        StatusCode::OK,
        "the managed endpoint must start a successful response"
    );
    let first = client
        .response
        .body_mut()
        .frame()
        .await
        .expect("the first chunk must arrive before release")
        .expect("the first chunk must not be an error");
    assert_eq!(
        first.into_data().expect("the first frame must carry data"),
        Bytes::from_static(b"A"),
        "the client must receive A before the test releases B"
    );
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_managed_response_delivers_a_chunk_before_its_awaited_gate_is_released() {
    let (rig, gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut client = first_chunk(&rig, &gate).await;
        let (mut peer, _) = gate
            .accept()
            .await
            .expect("the guest must await the owned gate");
        peer.write_all(&[1])
            .await
            .expect("release B after observing A");
        let remaining = client
            .response
            .body_mut()
            .collect()
            .await
            .expect("the completed body must be valid")
            .to_bytes();
        assert_eq!(
            remaining,
            Bytes::from_static(b"B"),
            "the gate release must produce B and clean EOF"
        );
    })
    .await
    .expect("incremental delivery must not wait for the unreleased gate");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trap_after_response_headers_aborts_the_body_instead_of_reporting_clean_eof() {
    let (rig, gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut client = first_chunk(&rig, &gate).await;
        let (mut peer, _) = gate.accept().await.expect("the guest must reach its gate");
        peer.write_all(&[0])
            .await
            .expect("trigger the fixture's post-header exception");
        assert!(
            client.response.body_mut().collect().await.is_err(),
            "a post-header guest trap must abort the response body"
        );
    })
    .await
    .expect("a post-header trap must end the response");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_streaming_client_releases_guest_io_and_dispatch_futures() {
    let (rig, gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let client = first_chunk(&rig, &gate).await;
        let (mut peer, _) = gate.accept().await.expect("the guest must await the gate");
        drop(client);
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte)
                .await
                .expect("the gate connection must close"),
            0,
            "client disconnect must release the guest's pending TCP connection"
        );
        let cell = &rig.node.execution.as_ref().cells()[0];
        while cell.futures().live_futures() != 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "client disconnect must release every dispatch-owned future"
        );
    })
    .await
    .expect("client drop must clean up without requiring the gate to release");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_response_keeps_proxy_admission_until_body_completion() {
    let (rig, gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut client = first_chunk(&rig, &gate).await;
        let denied = common::get(rig.proxy, "quotes.isoplane.dev", "/echo").await;
        assert_eq!(
            denied.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "an open response must retain its only proxy admission permit"
        );
        let (mut peer, _) = gate.accept().await.expect("the guest must await the gate");
        peer.write_all(&[1]).await.expect("release response");
        client
            .response
            .body_mut()
            .collect()
            .await
            .expect("response completion");
        let admitted = common::get(rig.proxy, "quotes.isoplane.dev", "/echo").await;
        assert_ne!(
            admitted.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "body EOF must release proxy admission"
        );
    })
    .await
    .expect("stream admission must settle after release");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_http_response_cannot_be_captured_while_waiting_on_guest_io() {
    let (rig, gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut client = first_chunk(&rig, &gate).await;
        let (mut peer, _) = gate.accept().await.expect("the guest must await the gate");
        let cell = rig.node.execution.as_ref().cells()[0].clone();
        let instance = *cell
            .resident_ids()
            .first()
            .expect("the streaming guest must remain resident");
        let result = tokio::task::spawn_blocking(move || cell.force_hibernate(instance))
            .await
            .expect("capture attempt must return");
        assert!(
            matches!(
                result,
                Err(isoplane_host::hibernate_policy::CaptureRefused::OpenResponse)
            ),
            "an open HTTP transport must explicitly veto capture"
        );
        peer.write_all(&[1]).await.expect("release response");
        client
            .response
            .body_mut()
            .collect()
            .await
            .expect("a refused capture must leave the stream usable");
    })
    .await
    .expect("capture refusal must not interrupt response progress");
    rig.shutdown().await;
}

/// Waits for an instance whose dispatch has run and parked waiting on nothing but its
/// requester leaving. A slot is Parked from creation, so only its count of parks tells a
/// finished slice from one that has not started.
async fn awaiting_departure(cell: &isoplane_host::Cell) -> InstanceId {
    let futures = cell.futures();
    loop {
        if let Some(id) = cell.resident_ids().into_iter().find(|id| {
            cell.slot(*id)
                .is_some_and(|slot| slot.parks() > 0 && slot.state() == LifecycleTier::Parked)
                && !futures.has_runnable(*id)
                && futures.pending_total(*id) == 1
                && futures.pending_for_import(*id, "ctx_cancel") == 1
        }) {
            return id;
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_requester_before_headers_cancels_the_parked_dispatch() {
    let (rig, _gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let address = rig.proxy;
        let request =
            tokio::spawn(
                async move { common::get(address, "quotes.isoplane.dev", "/cancel").await },
            );
        let cell = &rig.node.execution.as_ref().cells()[0];
        awaiting_departure(cell).await;
        request.abort();
        assert!(
            matches!(request.await, Err(error) if error.is_cancelled()),
            "the requester must leave before headers arrive"
        );
        while cell.metrics_raw().dispatch_us.count() == 0 || cell.futures().live_futures() != 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "pre-header disconnect must release the parked dispatch's futures"
        );
    })
    .await
    .expect("pre-header disconnect must end the dispatch");
    rig.shutdown().await;
}

/// What a handler saw after its requester left, as the next request on its instance reports
/// it, and the outcomes the cell recorded for the departed dispatch.
struct Departure {
    seen: String,
    outcomes: BTreeMap<(&'static str, u16), u64>,
}

/// Sends `path`, leaves once its dispatch parks, and reports the departure.
async fn depart(rig: &common::Rig, path: &'static str) -> Departure {
    let address = rig.proxy;
    let request =
        tokio::spawn(async move { common::get(address, "quotes.isoplane.dev", path).await });
    let cell = &rig.node.execution.as_ref().cells()[0];
    let parked = awaiting_departure(cell).await;
    request.abort();
    assert!(
        matches!(request.await, Err(error) if error.is_cancelled()),
        "the requester must leave before headers arrive"
    );
    while cell.metrics_raw().dispatch_outcomes.lock().is_empty()
        || cell.futures().live_futures() != 0
    {
        tokio::task::yield_now().await;
    }
    let outcomes = cell.metrics_raw().dispatch_outcomes.lock().clone();
    let report = common::get(address, "quotes.isoplane.dev", "/departure").await;
    assert_eq!(report.status, StatusCode::OK, "{}", report.body);
    assert_eq!(
        cell.resident_ids(),
        [parked],
        "the instance that served the departed requester serves the next one"
    );
    assert_eq!(cell.metrics().traps, 0, "no trap is counted");
    Departure {
        seen: report.body,
        outcomes,
    }
}

async fn departure(rig: common::Rig, path: &'static str) -> Departure {
    let departure = tokio::time::timeout(Duration::from_secs(30), depart(&rig, path))
        .await
        .expect("the departed dispatch must end");
    rig.shutdown().await;
    assert_eq!(
        departure.outcomes,
        BTreeMap::from([(("request_cancelled", 499), 1)]),
        "the departed dispatch is recorded as its requester leaving, not as failed"
    );
    departure
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_written_after_its_requester_left_is_discarded_without_failing_the_dispatch() {
    let departure = departure(start().await.0, "/departed/write").await;
    assert_eq!(departure.seen, "aborted;written");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_given_the_cancelled_request_token_throws_operation_cancelled() {
    let departure = departure(start().await.0, "/departed/write-cancelled").await;
    assert_eq!(departure.seen, "aborted;OperationCanceledException");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_response_returned_after_its_requester_left_is_discarded() {
    let departure = departure(start().await.0, "/departed/return").await;
    assert_eq!(departure.seen, "aborted;returned");
}

async fn start_plain() -> common::Rig {
    common::start_with(common::preparation::Guest::from(
        "isoplane_guest_dotnet_node",
    ))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_handler_buffered_response_after_its_requester_left_is_discarded() {
    let departure = departure(start_plain().await, "/departed/return").await;
    assert_eq!(departure.seen, "aborted;returned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_handler_stream_drops_untokened_writes_and_cancels_tokened_ones_after_departure() {
    let departure = departure(start_plain().await, "/departed/stream").await;
    assert_eq!(departure.seen, "aborted;written;OperationCanceledException");
}
