//! Packaged ASP.NET ILogger output through the real node logging boundary.
#![cfg(feature = "real-backends")]

pub mod common;

/// Shared logging boundary test support.
#[path = "common/otlp.rs"]
pub mod otlp;

use isoplane_abi::config::InitSandbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_packaged_aspnet_provider_emits_attributed_customer_logs() {
    let output = otlp::output::LogOutput::default();
    let writer = output.clone();
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
    let rig = common::start_with_policy(aspnet, InitSandbox::RuntimeBringUp, None, |_| {}).await;
    let response = common::get(rig.proxy, "quotes.isoplane.dev", "/logs").await;
    assert_eq!(
        response.body, "aspnet logged",
        "the actual ASP.NET adapter must complete its logging route"
    );
    let records = otlp::output::guest_logs(&output);
    assert!(records.iter().any(|record| record.level == 3 && record.fields.contains("AspNetLoggingFixture")
        && record.fields.contains("aspnet-application-log") && record.tenant == "acme" && record.module == "quotes@v1"),
        "the normally registered ASP.NET ILogger provider must reach the same attributed customer log boundary");
    rig.shutdown().await;
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("logging shutdown task")
        .expect("logging flush");
}
