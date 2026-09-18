//! The maintained ASP.NET adapter through the real deploy pipeline and HTTP listener.
#![cfg(feature = "real-backends")]

use super::common;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use isoplane_abi::config::InitSandbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aspnet_route_receives_host_fields_and_returns_its_buffered_response() {
    let rig = common::start_with(
        isoplane_fixtures::dotnet("isoplane_guest_aspnet"),
        InitSandbox::RuntimeBringUp,
    )
    .await;
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/echo?tag=one&tag=two")
            .header("Host", "quotes.isoplane.dev")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aspnet_upload_is_read_and_echoed_before_the_client_finishes_sending() {
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::Frame;
    use std::convert::Infallible;
    use std::time::Duration;

    let rig = common::start_with(
        isoplane_fixtures::dotnet("isoplane_guest_aspnet"),
        InitSandbox::RuntimeBringUp,
    )
    .await;
    let (send, receive) = tokio::sync::mpsc::channel::<Bytes>(1);
    let stream = futures::stream::unfold(receive, |mut receive| async move {
        receive
            .recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), receive))
    });
    let request = Request::builder()
        .method("POST")
        .uri("/echo")
        .header("Host", "quotes.isoplane.dev")
        .body(StreamBody::new(stream))
        .expect("valid streaming upload request");
    let mut connection = common::HttpConnection::connect(rig.proxy).await;
    send.send(Bytes::from_static(b"A"))
        .await
        .expect("upload receiver is live");
    let mut response = tokio::time::timeout(Duration::from_secs(30), connection.request(request))
        .await
        .expect("the response must start without waiting for upload EOF");
    assert_eq!(
        response.status(),
        201,
        "ASP.NET starts its response during upload"
    );
    let first = tokio::time::timeout(Duration::from_secs(30), response.body_mut().frame())
        .await
        .expect("the first upload chunk must be echoed while the second is withheld")
        .expect("the echo response contains its first frame")
        .expect("the first echo frame succeeds")
        .into_data()
        .expect("the first echo frame contains data");
    assert_eq!(
        first,
        Bytes::from_static(b"A"),
        "the guest observes the first upload chunk independently"
    );
    send.send(Bytes::from_static(b"B"))
        .await
        .expect("upload remains live after the first echo");
    drop(send);
    let rest = tokio::time::timeout(Duration::from_secs(30), response.into_body().collect())
        .await
        .expect("the response settles after upload EOF")
        .expect("the remaining echo body succeeds")
        .to_bytes();
    assert_eq!(
        rest,
        Bytes::from_static(b"B"),
        "the upload and response preserve the remaining chunk"
    );
    drop(connection);
    rig.shutdown().await;
}
