//! HTTP routing, admission and guest response tests over real TCP.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;

use wisp_abi::host::{Dispatcher};
use common::fake::FakeCell;
use wisp_proxy::{ProxyConfig, ShedConfig};


// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_request_resolves_to_a_version_and_reaches_the_cell() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let r = get(rig.addr, "quotes.wisp.dev", "/api/quote", &[]).await;

    assert_eq!(r.status, 200);
    assert_eq!(r.header("x-wisp-version"), Some("v1"));
    assert_eq!(r.header("x-wisp-tier"), Some("optimized"));
    assert_eq!(r.json()["app"], "quotes");
    assert_eq!(r.json()["version"], "v1");
    assert_eq!(r.json()["tenant"], "acme");
    assert_eq!(r.json()["path"], "/api/quote");
    assert_eq!(rig.cell.http_dispatches(), 1);
}

#[tokio::test]
async fn an_unrouted_host_is_a_404_and_never_reaches_a_cell() {
    let rig = start(
        Arc::new(FakeCell::new()),
        standard_routes(),
        ProxyConfig::default(),
    )
    .await;
    let r = get(rig.addr, "nobody.wisp.dev", "/", &[]).await;
    assert_eq!(r.status, 404);
    assert_eq!(rig.cell.http_dispatches(), 0);
}


// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------


#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_proxy_sheds_before_the_cells_saturate() {
    struct HeldHttp(tokio::sync::Semaphore);

    #[async_trait::async_trait]
    impl Dispatcher for HeldHttp {
        async fn dispatch_http(
            &self,
            _: &wisp_abi::host::RouteTarget,
            _: wisp_abi::dispatch::HttpRequest,
        ) -> anyhow::Result<wisp_abi::host::HttpReply> {
            self.0.acquire().await.expect("test semaphore stays open").forget();
            Ok(wisp_abi::host::HttpReply {
                status: 200,
                headers: vec![],
                body: b"admitted".to_vec(),
                streaming: false,
            })
        }
    }

    let cell = Arc::new(HeldHttp(tokio::sync::Semaphore::new(0)));
    let metrics = Arc::new(wisp_proxy::ProxyMetrics::new());
    let cfg = ProxyConfig {
        shed: ShedConfig { max_in_flight: 1, max_queued: 1 },
        ..Default::default()
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await.expect("test listener");
    let addr = listener.local_addr().expect("test listener address");
    let proxy = wisp_proxy::Proxy::new(standard_routes(), cell.clone(), metrics.clone(), cfg);
    let serving = tokio::spawn(proxy.serve(listener));
    let mut tasks = Vec::new();
    for i in 0..8 {
        tasks.push(tokio::spawn(async move {
            get(addr, "quotes.wisp.dev", &format!("/admission/{i}"), &[])
                .await.status.as_u16()
        }));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while metrics.requests_shed.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    }).await.expect("a blocked HTTP dispatch exposes the bounded admission queue");
    cell.0.add_permits(8);
    let codes: Vec<u16> = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let shed = codes.iter().filter(|c| **c == 503).count();
    let served = codes.iter().filter(|c| **c == 200).count();
    assert!(shed > 0, "expected some requests to be shed, got {codes:?}");
    assert!(
        served > 0,
        "shedding must not refuse everything, got {codes:?}"
    );
    assert_eq!(shed + served, 8);
    assert_eq!(
        metrics
            .requests_shed
            .load(std::sync::atomic::Ordering::Relaxed) as usize,
        shed
    );
    serving.abort();
    let _ = serving.await;
}


// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------








#[tokio::test]
async fn a_cell_s_503_reaches_the_client_with_its_headers() {
    let cell = Arc::new(FakeCell::new());
    let rig = start(cell.clone(), standard_routes(), ProxyConfig::default()).await;
    assert_eq!(
        get(rig.addr, "quotes.wisp.dev", "/spin", &[]).await.status,
        200
    );

    cell.trip_breaker("quotes:/spin");
    let r = get(rig.addr, "quotes.wisp.dev", "/spin", &[]).await;
    assert_eq!(r.status, 503);
    assert_eq!(r.header("x-wisp-breaker"), Some("tripped"));
    // Other routes on the same app are unaffected — blast radius is the
    // offending (module, route), not the tenant.
    assert_eq!(
        get(rig.addr, "quotes.wisp.dev", "/healthy", &[])
            .await
            .status,
        200
    );

    cell.reset_breaker("quotes:/spin");
    assert_eq!(
        get(rig.addr, "quotes.wisp.dev", "/spin", &[]).await.status,
        200
    );
}
