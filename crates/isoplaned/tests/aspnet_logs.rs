//! Packaged ASP.NET ILogger output through the real node logging boundary.

pub mod common;

use isoplane_abi::config::InitSandbox;
use isoplane_harness::logging::LogCapture;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_packaged_aspnet_provider_emits_attributed_customer_logs() {
    let capture = LogCapture::default();
    let writer = capture.clone();
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, writer, "customer-log-test", None, None)
    })
    .await
    .expect("logging setup task")
    .expect("one test subscriber");
    let aspnet =
        tokio::task::spawn_blocking(|| common::preparation::Guest::from("isoplane_guest_aspnet"))
            .await
            .expect("the ASP.NET logging fixture must build");
    let rig = common::start_with(aspnet, InitSandbox::RuntimeBringUp).await;
    let response = common::get(rig.proxy, "quotes.isoplane.dev", "/logs").await;
    assert_eq!(
        response.body, "aspnet logged",
        "the actual ASP.NET adapter must complete its logging route"
    );
    let records = capture.records();
    assert!(records.iter().any(|record| record.level == 3 && record.fields.contains("AspNetLoggingFixture")
        && record.fields.contains("aspnet-application-log") && record.app == "quotes" && record.version == "v1"),
        "the normally registered ASP.NET ILogger provider must reach the same attributed customer log boundary");
    rig.shutdown().await;
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("logging shutdown task")
        .expect("logging flush");
}
