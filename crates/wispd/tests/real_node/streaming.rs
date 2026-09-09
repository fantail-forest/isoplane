//! Customer-visible response streaming through the managed adapter, real cell and proxy.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wisp_abi::config::InitSandbox;

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
        wisp_fixtures::dotnet("wisp_guest_aspnet"),
        InitSandbox::RuntimeBringUp,
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
        .header("host", "quotes.wisp.dev")
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
        let cell = &rig.node.real.as_ref().expect("real cells").cells()[0];
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
        let denied = common::get(rig.proxy, "quotes.wisp.dev", "/echo").await;
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
        let admitted = common::get(rig.proxy, "quotes.wisp.dev", "/echo").await;
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
        let cell = rig.node.real.as_ref().expect("real cells").cells()[0].clone();
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
                Err(wisp_host::hibernate_policy::CaptureRefused::OpenResponse)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_requester_before_headers_cancels_the_parked_dispatch() {
    let (rig, _gate) = start().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let address = rig.proxy;
        let request =
            tokio::spawn(async move { common::get(address, "quotes.wisp.dev", "/cancel").await });
        let cell = &rig.node.real.as_ref().expect("real cells").cells()[0];
        loop {
            if cell.resident_ids().iter().any(|id| {
                cell.slot(*id)
                    .is_some_and(|slot| slot.state() == wisp_host::slot::LifecycleTier::Parked)
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
        request.abort();
        assert!(
            matches!(request.await, Err(error) if error.is_cancelled()),
            "the requester must leave before headers arrive"
        );
        while cell.metrics_raw().request_us.count() == 0 || cell.futures().live_futures() != 0 {
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
