use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use wisp_abi::calls::FetchRequest;
use wisp_abi::dispatch::ResponseHead;
use wisp_abi::result::ResultRecord;
use wisp_host::egress::{DenyReason, DestinationPolicy};

use super::{common, config, get, manifest, serve, InitSandbox, Node, Rig};

struct OwnedEndpoint(SocketAddr);

impl DestinationPolicy for OwnedEndpoint {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address == self.0 {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}

fn escaped(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
}

fn fetching_guest(destination: SocketAddr) -> Vec<u8> {
    let request = wisp_abi::encode(&FetchRequest {
        method: "GET".into(),
        url: format!("http://{destination}/owned"),
        headers: vec![],
        body: vec![],
    })
    .expect("the fetch request must encode");
    let response = |status| {
        wisp_abi::encode(&ResponseHead {
            status,
            headers: vec![],
            body: vec![],
            streaming: false,
        })
        .expect("the response head must encode")
    };
    let success = response(200);
    let failure = response(502);
    let ok_tag = wisp_abi::encode(&ResultRecord::Ok(vec![])).expect("result tag must encode")[0];
    let pending = wisp_abi::status::PollStatus::Pending.encode();
    wat::parse_str(format!(
        r#"(module
          (import "wisp" "http_fetch" (func $fetch (param i32 i32) (result i64)))
          (import "wisp" "resp_new" (func $response (param i32 i32) (result i32)))
          (memory (export "memory") 2 2)
          (data (i32.const 1024) "{}")
          (data (i32.const 2048) "{}")
          (data (i32.const 3072) "{}")
          (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 16384))
          (func (export "wisp_handle") (param i32 i32) (result i64)
            (drop (call $fetch (i32.const 1024) (i32.const {})))
            (i64.const {pending}))
          (func (export "wisp_wake") (param i64) (param $result i32) (param i32) (result i64)
            (i64.shl (i64.extend_i32_u
              (if (result i32) (i32.eq (i32.load8_u (local.get $result)) (i32.const {ok_tag}))
                (then (call $response (i32.const 2048) (i32.const {})))
                (else (call $response (i32.const 3072) (i32.const {})))))
              (i64.const 32))))"#,
        escaped(&request),
        escaped(&success),
        escaped(&failure),
        request.len(),
        success.len(),
        failure.len()
    ))
    .expect("the fetching guest must assemble")
}

async fn node_for(destination: SocketAddr, policy: Option<Arc<dyn DestinationPolicy>>) -> Rig {
    let directory = tempfile::tempdir().expect("the node must own a unique directory");
    let configuration = config(directory.path());
    let node = match policy {
        Some(policy) => Node::build_with_destinations(configuration, policy),
        None => Node::build(configuration),
    }
    .expect("the real node must assemble");
    node.real
        .as_ref()
        .expect("the node must use real backends")
        .deploy_version(
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
    let response = get(rig.proxy, "quotes.wisp.dev", "/").await;
    assert_eq!(
        response.status, 502,
        "the guest must receive a refused fetch"
    );
    let denied: u64 = rig
        .node
        .real
        .as_ref()
        .expect("real cells")
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
    let response = get(rig.proxy, "quotes.wisp.dev", "/").await;
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
