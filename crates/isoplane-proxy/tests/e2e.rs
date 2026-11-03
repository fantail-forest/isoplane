//! End-to-end proxy tests over real TCP, driven as a browser or `curl` would.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::fake::FakeCell;
use common::*;
use isoplane_abi::host::CodeTier;
use isoplane_proxy::ProxyConfig;

#[tokio::test]
async fn http_request_resolves_to_a_version_and_reaches_the_cell() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let r = get(rig.addr, "quotes.isoplane.dev", "/api/quote", &[]).await;

    assert_eq!(r.status, 200, "a routed request is served");
    assert_eq!(
        r.header("x-isoplane-version"),
        Some("v1"),
        "the installed deployment served it"
    );
    assert_eq!(
        r.header("x-isoplane-tier"),
        Some("optimized"),
        "the version's tier is reported"
    );
    assert_eq!(r.json()["app"], "quotes", "the cell saw the routed app");
    assert_eq!(r.json()["version"], "v1", "the cell saw the routed version");
    assert_eq!(r.json()["tenant"], "acme", "the cell saw the tenant");
    assert_eq!(r.json()["path"], "/api/quote", "the cell saw the path");
    assert_eq!(
        rig.cell.http_dispatches(),
        1,
        "exactly one dispatch reached the cell"
    );
}

#[tokio::test]
async fn an_unrouted_host_is_a_404_and_never_reaches_a_cell() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let r = get(rig.addr, "nobody.isoplane.dev", "/", &[]).await;
    assert_eq!(r.status, 404, "an unrouted host is refused at the edge");
    assert_eq!(rig.cell.http_dispatches(), 0, "no cell was consulted");
}

#[tokio::test]
async fn malformed_hosts_never_dispatch_or_fall_back_to_an_installed_application() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    for host in [
        "quotes.isoplane.dev:invalid",
        "quotes.isoplane.dev:+80",
        "quotes.isoplane.dev:65536",
        "quotes.isoplane.dev:",
        "user@quotes.isoplane.dev",
        "https://quotes.isoplane.dev",
        "quotes.isoplane.dev/path",
    ] {
        let response = get(rig.addr, host, "/", &[]).await;
        assert_eq!(
            response.status, 404,
            "malformed Host {host} must not select an application"
        );
    }
    assert_eq!(rig.cell.http_dispatches(), 0);
}

#[tokio::test]
async fn one_ingress_port_routes_independent_applications_by_their_expected_hosts() {
    let routes = standard_routes();
    routes
        .insert(
            isoplane_abi::host::RouteTarget {
                app: "orders".into(),
                tenant: "acme".into(),
                version: "orders-current".into(),
                tier: CodeTier::Fast,
            },
            &["orders.isoplane.dev".into()],
        )
        .expect("install sibling application");
    let rig = start(Arc::new(FakeCell::new()), routes, ProxyConfig::default()).await;
    let quotes = get(rig.addr, "QUOTES.isoplane.dev:8080", "/anything", &[]).await;
    let orders = get(rig.addr, "orders.isoplane.dev", "/anything", &[]).await;
    assert_eq!(quotes.json()["app"], "quotes");
    assert_eq!(orders.json()["app"], "orders");
    assert_eq!(orders.json()["tenant"], "acme");
    assert_eq!(rig.cell.http_dispatches(), 2);
}

#[tokio::test]
async fn a_cell_s_503_reaches_the_client_with_its_headers() {
    let cell = Arc::new(FakeCell::new());
    let rig = start(cell.clone(), standard_routes(), ProxyConfig::default()).await;
    assert_eq!(
        get(rig.addr, "quotes.isoplane.dev", "/spin", &[])
            .await
            .status,
        200,
        "the route serves before the breaker trips"
    );

    cell.trip_breaker("quotes:/spin");
    let r = get(rig.addr, "quotes.isoplane.dev", "/spin", &[]).await;
    assert_eq!(r.status, 503, "the cell's status is passed through");
    assert_eq!(
        r.body, "hang breaker tripped",
        "the cell's body is passed through"
    );
    assert_eq!(
        r.header("retry-after"),
        Some("5"),
        "the cell's retry-after header is passed through"
    );
    assert_eq!(
        get(rig.addr, "quotes.isoplane.dev", "/healthy", &[])
            .await
            .status,
        200,
        "another route on the same app is unaffected"
    );

    cell.reset_breaker("quotes:/spin");
    assert_eq!(
        get(rig.addr, "quotes.isoplane.dev", "/spin", &[])
            .await
            .status,
        200,
        "the route serves again once the cell allows it"
    );
}

#[tokio::test]
async fn a_guest_cannot_forge_platform_headers() {
    let cell = Arc::new(FakeCell::new());
    let rig = start(cell.clone(), standard_routes(), ProxyConfig::default()).await;
    rig.routes
        .insert(
            quotes_target("v2", CodeTier::Fast),
            &["quotes.isoplane.dev".into()],
        )
        .expect("install deployment");
    let r = get(rig.addr, "quotes.isoplane.dev", "/", &[]).await;
    // The fake's reply sets x-isoplane-version itself.
    assert_eq!(
        r.headers.get_all("x-isoplane-version").iter().count(),
        1,
        "the guest's copy of a platform header is dropped, not merged"
    );
    assert_eq!(
        r.header("x-isoplane-version"),
        Some("v2"),
        "the surviving value is the proxy's"
    );
    assert_eq!(
        r.header("x-isoplane-tier"),
        Some("fast"),
        "the tier header is the proxy's"
    );
}

#[tokio::test]
async fn tier_is_reported_per_dispatch() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    rig.routes
        .insert(
            quotes_target("v2", CodeTier::Fast),
            &["quotes.isoplane.dev".into()],
        )
        .expect("install deployment");
    assert_eq!(
        get(rig.addr, "quotes.isoplane.dev", "/", &[])
            .await
            .header("x-isoplane-tier"),
        Some("fast"),
        "a fresh version reports the fast tier"
    );
    rig.routes
        .insert(
            quotes_target("v3", CodeTier::Optimized),
            &["quotes.isoplane.dev".into()],
        )
        .expect("replace deployment");
    assert_eq!(
        get(rig.addr, "quotes.isoplane.dev", "/", &[])
            .await
            .header("x-isoplane-tier"),
        Some("optimized"),
        "a version at the optimized tier reports it"
    );

    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(
        rig.metrics.dispatches_tier_fast.load(Relaxed),
        1,
        "one dispatch was counted against the fast tier"
    );
    assert_eq!(
        rig.metrics.dispatches_tier_optimized.load(Relaxed),
        1,
        "one dispatch was counted against the optimized tier"
    );
}

#[tokio::test]
async fn connection_headers_stop_at_the_proxy_while_application_headers_reach_the_guest() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let response = request(
        rig.addr,
        "POST",
        "quotes.isoplane.dev",
        "/headers",
        &[
            ("Connection", "keep-alive, X-Private"),
            ("Connection", "x-SECOND, TE"),
            ("x-private", "discard"),
            ("X-Second", "discard"),
            ("Keep-Alive", "timeout=5"),
            ("Proxy-Connection", "keep-alive"),
            ("Proxy-Authorization", "Basic proxy-only"),
            ("Proxy-Authenticate", "Basic"),
            ("TE", "trailers"),
            ("Trailer", "x-checksum"),
            ("Transfer-Encoding", "chunked"),
            ("Upgrade", "other-protocol"),
            ("Authorization", "Bearer app-token"),
            ("Cookie", "session=app-cookie"),
            ("Content-Type", "application/octet-stream"),
            ("X-Signature", "signed-body"),
            ("X-Custom", "first"),
            ("X-Custom", "second"),
        ],
        b"payload".to_vec(),
    )
    .await;
    assert_eq!(
        response.status, 200,
        "filtering must not prevent a valid request from reaching the guest"
    );
    let echoed = response.json();
    let headers: Vec<(String, String)> = serde_json::from_value(echoed["headers"].clone()).unwrap();
    for name in [
        "connection",
        "x-private",
        "x-second",
        "keep-alive",
        "proxy-connection",
        "proxy-authorization",
        "proxy-authenticate",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        assert!(
            !headers.iter().any(|(key, _)| key == name),
            "connection-specific {name} must not reach the guest"
        );
    }
    for (name, value) in [
        ("authorization", "Bearer app-token"),
        ("cookie", "session=app-cookie"),
        ("content-type", "application/octet-stream"),
        ("x-signature", "signed-body"),
    ] {
        assert!(
            headers.contains(&(name.into(), value.into())),
            "application header {name} must reach the guest unchanged"
        );
    }
    let custom: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name == "x-custom")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        custom,
        ["first", "second"],
        "repeated application values must preserve their order"
    );
    assert_eq!(
        echoed["body"],
        serde_json::json!(b"payload".to_vec()),
        "chunk framing must be decoded before delivering the body"
    );
}

#[tokio::test]
async fn proxy_shutdown_closes_keepalives_and_prevents_new_http_admission() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::Request;
    use tokio::net::TcpStream;

    let mut rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let (mut sender, client) = http_connection(rig.addr).await;
    let request = || {
        Request::builder()
            .uri("/")
            .header("host", "quotes.isoplane.dev")
            .body(Full::new(Bytes::new()))
            .unwrap()
    };
    sender
        .send_request(request())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    let before = rig.cell.http_dispatches();
    rig.shutdown().await;
    assert!(
        sender.send_request(request()).await.is_err(),
        "a stopped proxy cannot accept a second keepalive request"
    );
    assert_eq!(
        rig.cell.http_dispatches(),
        before,
        "shutdown must prevent further guest admission"
    );
    assert!(
        TcpStream::connect(rig.addr).await.is_err(),
        "the joined listener must no longer accept TCP connections"
    );
    client
        .await
        .expect("the client transport task must not panic")
        .expect("the idle keepalive must close cleanly");
}

#[tokio::test]
async fn proxy_shutdown_cancels_an_incomplete_upload_without_waiting_for_more_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let mut client = TcpStream::connect(rig.addr).await.unwrap();
    client.write_all(b"POST / HTTP/1.1\r\nHost: quotes.isoplane.dev\r\nContent-Length: 10\r\nExpect: 100-continue\r\n\r\n").await.unwrap();
    let mut accepted = [0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
    client.read_exact(&mut accepted).await.unwrap();
    assert_eq!(
        &accepted, b"HTTP/1.1 100 Continue\r\n\r\n",
        "the proxy must actually be waiting on the upload before shutdown"
    );
    rig.shutdown().await;
    assert_eq!(
        client.read(&mut [0]).await.unwrap(),
        0,
        "shutdown must drop the pending upload transport"
    );
    assert_eq!(
        rig.cell.http_dispatches(),
        0,
        "an incomplete legacy upload must not dispatch after cancellation"
    );
}

#[path = "common/shutdown.rs"]
mod shutdown;

#[tokio::test]
async fn proxy_shutdown_drops_a_blocked_response_body_before_returning() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Request;
    use tokio_util::sync::CancellationToken;

    let cell = Arc::new(FakeCell::new());
    let dispatcher = Arc::new(shutdown::BlockedResponse {
        cell: cell.clone(),
        polled: CancellationToken::new(),
        dropped: CancellationToken::new(),
    });
    let mut rig = start_with_dispatcher(
        cell,
        standard_routes(),
        ProxyConfig {
            shed: isoplane_proxy::shed::ShedConfig {
                max_in_flight: 1,
                max_queued: 0,
            },
            ..ProxyConfig::default()
        },
        dispatcher.clone(),
    )
    .await;
    let (mut sender, client) = http_connection(rig.addr).await;
    let request = tokio::spawn(async move {
        sender
            .send_request(
                Request::builder()
                    .uri("/")
                    .header("host", "quotes.isoplane.dev")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), dispatcher.polled.cancelled())
        .await
        .expect("the response body must actually be pending before shutdown");
    let refused = tokio::time::timeout(
        Duration::from_secs(10),
        get(rig.addr, "quotes.isoplane.dev", "/second", &[]),
    )
    .await
    .expect("overload refusal must not wait for the first response body");
    assert_eq!(
        refused.status, 503,
        "a streaming response retains its admission slot"
    );
    assert_eq!(refused.header("retry-after"), Some("1"));
    assert_eq!(
        rig.metrics
            .requests_shed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        rig.metrics
            .route_resolutions_total
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the refused request never reaches route selection"
    );
    rig.shutdown().await;
    assert!(
        dispatcher.dropped.is_cancelled(),
        "shutdown must release the owned response body before returning"
    );
    match request.await.expect("client request task must settle") {
        Ok(response) => {
            use http_body_util::BodyExt;
            assert!(
                response.into_body().collect().await.is_err(),
                "a cancelled pending response must not produce successful EOF"
            );
        }
        Err(error) => assert!(
            error.is_incomplete_message() || error.is_closed() || error.is_canceled(),
            "shutdown must terminate the client transport: {error}"
        ),
    }
    if let Err(error) = client.await.expect("client connection task must settle") {
        assert!(
            error.is_incomplete_message() || error.is_closed() || error.is_canceled(),
            "the client driver must report transport cancellation: {error}"
        );
    }
}
