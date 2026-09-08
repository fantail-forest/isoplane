//! The maintained ASP.NET adapter through the real deploy pipeline and HTTP listener.
#![cfg(feature = "real-backends")]

use super::common;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use wisp_abi::config::InitSandbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aspnet_route_receives_host_fields_and_returns_its_buffered_response() {
    let rig = common::start_with(
        wisp_fixtures::dotnet("wisp_guest_aspnet"),
        InitSandbox::RuntimeBringUp,
    )
    .await;
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/echo?tag=one&tag=two")
            .header("Host", "quotes.wisp.dev")
            .header("X-Input", "first")
            .header("X-Input", "second")
            .body(Full::new(Bytes::from_static(b"buffered body")))
            .expect("valid HTTP fixture request");
        let response = common::request(rig.proxy, request).await;
        assert_eq!(
            response.status, 201,
            "ASP.NET chooses the HTTP response status"
        );
        assert_eq!(
            response.body, "buffered body",
            "ASP.NET receives and returns the host request body"
        );
        assert_header_values(&response, "set-cookie", &["a=one", "b=two"]);
        assert_header_values(&response, "x-query", &["one", "two"]);
        assert_header_values(&response, "x-input", &["first", "second"]);
    }
    rig.shutdown().await;
}

fn assert_header_values(response: &common::Resp, name: &str, expected: &[&str]) {
    let values: Vec<_> = response
        .headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("fixture headers must be text"))
        .collect();
    assert_eq!(
        values, expected,
        "ASP.NET must preserve every {name} header value in order"
    );
}
