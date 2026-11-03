use crate::common::preparation::LocalDeployment;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::owned_endpoint::OwnedEndpoint;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use isoplane_abi::calls::FetchRequest;
use isoplane_abi::dispatch::ResponseHead;
use isoplane_abi::result::ResultRecord;
use isoplane_host::egress::DestinationPolicy;

use super::{common, config, get, manifest, serve, InitSandbox, Node, Rig};

fn fetching_guest(destination: SocketAddr) -> Vec<u8> {
    let request = isoplane_abi::encode(&FetchRequest {
        method: "GET".into(),
        url: format!("http://{destination}/owned"),
        headers: vec![],
        body: vec![],
    })
    .expect("the fetch request must encode");
    let response = |status| {
        isoplane_abi::encode(&ResponseHead {
            status,
            headers: vec![],
            body: vec![],
            streaming: false,
        })
        .expect("the response head must encode")
    };
    let success = response(200);
    let failure = response(502);
    let ok_tag =
        isoplane_abi::encode(&ResultRecord::Ok(vec![])).expect("result tag must encode")[0];
    let pending = isoplane_abi::status::PollStatus::Pending.encode();
    wat::parse_str(format!(
        r#"(module
          (import "isoplane" "http_fetch" (func $fetch (param i32 i32) (result i64)))
          (import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
          (memory (export "memory") 2 2)
          (data (i32.const 1024) "{}")
          (data (i32.const 2048) "{}")
          (data (i32.const 3072) "{}")
          (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 16384))
          (func (export "isoplane_handle") (param i32 i32) (result i64)
            (drop (call $fetch (i32.const 1024) (i32.const {})))
            (i64.const {pending}))
          ;; Steps past the wake record's dispatch, a varint, to its result.
          (func $result (param $at i32) (result i32)
            (block $end (loop $byte
              (br_if $end (i32.eqz (i32.and (i32.load8_u (local.get $at)) (i32.const 128))))
              (local.set $at (i32.add (local.get $at) (i32.const 1)))
              (br $byte)))
            (i32.add (local.get $at) (i32.const 1)))
          (func (export "isoplane_wake") (param i64) (param $wake i32) (param i32) (result i64)
            (i64.shl (i64.extend_i32_u
              (if (result i32) (i32.eq (i32.load8_u (call $result (local.get $wake))) (i32.const {ok_tag}))
                (then (call $response (i32.const 2048) (i32.const {})))
                (else (call $response (i32.const 3072) (i32.const {})))))
              (i64.const 32))))"#,
        isoplane_test_support::wat_bytes(&request),
        isoplane_test_support::wat_bytes(&success),
        isoplane_test_support::wat_bytes(&failure),
        request.len(),
        success.len(),
        failure.len()
    ))
    .expect("the fetching guest must assemble")
}

async fn node_for(destination: SocketAddr, policy: Option<Arc<dyn DestinationPolicy>>) -> Rig {
    let directory =
        crate::common::private_directory().expect("the node must own a unique directory");
    let configuration = config(directory.path());
    let node = match policy {
        Some(policy) => Node::build_with_destinations(configuration, policy),
        None => Node::build(configuration),
    }
    .expect("the real node must assemble");
    node.real
        .as_ref()
        .deploy_locally(
            "quotes",
            "v1",
            "acme",
            manifest("quotes"),
            &fetching_guest(destination),
            InitSandbox::default(),
        )
        .await
        .expect("the guest must deploy through the real pipeline");
    serve(node, directory).await
}

#[tokio::test]
async fn a_default_node_refuses_private_outbound_destinations() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the test must reserve its destination");
    let rig = node_for(
        listener.local_addr().expect("the listener has an address"),
        None,
    )
    .await;
    let response = get(rig.proxy, "quotes.isoplane.dev", "/").await;
    assert_eq!(
        response.status, 502,
        "the guest must receive a refused fetch"
    );
    let denied: u64 = rig
        .node
        .real
        .as_ref()
        .cells()
        .iter()
        .map(|cell| cell.metrics().egress_denials)
        .sum();
    assert_eq!(
        denied, 1,
        "the default node must deny the private destination before transport"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn a_nodes_injected_policy_reaches_its_real_outbound_transport() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the test must own its upstream listener");
    let address = listener.local_addr().expect("the listener has an address");
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("the checked connection must arrive");
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                hyper::service::service_fn(
                    |request: hyper::Request<hyper::body::Incoming>| async move {
                        assert_eq!(
                            request.uri().path(),
                            "/owned",
                            "the real upstream must receive the guest request"
                        );
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                            Bytes::from_static(b"owned"),
                        )))
                    },
                ),
            )
            .await
            .expect("the owned HTTP connection must complete");
    });
    let rig = node_for(address, Some(Arc::new(OwnedEndpoint(address)))).await;
    let response = get(rig.proxy, "quotes.isoplane.dev", "/").await;
    assert_eq!(
        response.status, 200,
        "the injected policy must authorize the real upstream fetch"
    );
    rig.shutdown().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), upstream)
        .await
        .expect("the owned connection must not hang")
        .expect("the upstream task must not panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_managed_network_stream_exchanges_partial_payloads_through_a_real_node() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the native stream test must own its upstream listener");
    let address = listener
        .local_addr()
        .expect("the upstream must have an endpoint");
    let directory =
        crate::common::private_directory().expect("the native stream node must own its directory");
    let node =
        Node::build_with_destinations(config(directory.path()), Arc::new(OwnedEndpoint(address)))
            .expect("the real native stream node must assemble");
    node.real
        .as_ref()
        .deploy_locally(
            "quotes",
            "v1",
            "acme",
            manifest("quotes"),
            &common::preparation::Guest::from("isoplane_guest_dotnet_native_io"),
            InitSandbox::RuntimeBringUp,
        )
        .await
        .expect("the ordinary NetworkStream fixture must deploy");
    let rig = serve(node, directory).await;
    let cell = rig.node.real.as_ref().cells()[0].clone();
    let proxy = rig.proxy;
    let request = tokio::spawn(async move {
        get(
            proxy,
            "quotes.isoplane.dev",
            &format!("/stream/{}/{}", address.ip(), address.port()),
        )
        .await
    });
    let mut stage = "accept";
    let exchange = async {
        let (mut server, _) = listener
            .accept()
            .await
            .expect("the managed connect must reach its checked peer");
        stage = "first hibernation";
        hibernate_pending_stream(&cell).await;
        server
            .write_all(b"pi")
            .await
            .expect("the first partial payload must reach the guest");
        stage = "partial-read hibernation";
        hibernate_pending_stream(&cell).await;
        server
            .write_all(b"ng")
            .await
            .expect("the second partial payload must reach the guest");
        stage = "echo";
        let mut echoed = [0; 4];
        server
            .read_exact(&mut echoed)
            .await
            .expect("the managed stream must echo every byte");
        assert_eq!(
            &echoed, b"ping",
            "the managed stream must preserve partial input without duplication or loss"
        );
        let mut extra = [0];
        assert_eq!(
            server
                .read(&mut extra)
                .await
                .expect("the peer must observe disposal"),
            0,
            "managed disposal must close the host-owned connection"
        );
        let response = request.await.expect("the HTTP request task must complete");
        assert_eq!(
            response.status, 200,
            "the real listener must return the managed result"
        );
        assert_eq!(
            response.body, "transferred",
            "the managed handler must complete its stream exchange"
        );
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), exchange).await;
    let owners: Vec<_> = cell
        .resident_ids()
        .into_iter()
        .map(|owner| {
            (
                owner,
                cell.instance_state(owner),
                cell.futures().pending_total(owner),
                cell.futures().has_ready(owner),
            )
        })
        .collect();
    assert!(result.is_ok(), "the real-node stream exchange must complete before the deadline: stage={stage}, owners={owners:?}");
    rig.shutdown().await;
}

async fn hibernate_pending_stream(cell: &Arc<isoplane_host::Cell>) {
    use isoplane_abi::config::LifecycleTier;
    let owner = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let owners = cell.resident_ids();
            if let [owner] = owners.as_slice() {
                if cell.instance_state(*owner) == Some(LifecycleTier::Parked)
                    && !cell.futures().has_ready(*owner)
                    && cell.futures().pending_total(*owner) == 2
                {
                    match tokio::task::block_in_place(|| cell.force_hibernate(*owner)) {
                        Ok(_) => break *owner,
                        Err(isoplane_host::hibernate_policy::CaptureRefused::NotParked) => {}
                        Err(error) => {
                            panic!("the node must capture its pending native stream: {error:?}")
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the managed stream must park with its I/O and cancellation waits");
    assert_eq!(
        cell.instance_state(owner),
        Some(LifecycleTier::Hibernated),
        "the native connection and partial managed read must survive actual hibernation"
    );
}
