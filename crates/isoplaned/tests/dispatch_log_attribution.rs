//! Logs from work a managed dispatch leaves running after its response.
#![cfg(feature = "real-backends")]

pub mod common;

/// Shared logging boundary test support.
#[path = "common/otlp.rs"]
pub mod otlp;

use isoplane_abi::config::InitSandbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "unfixed: a timer left by one dispatch runs in the next dispatch's slices, so its logs carry the wrong dispatch; per-dispatch slices fix this"]
async fn a_dispatchs_unawaited_work_logs_as_that_dispatch() {
    let output = otlp::output::LogOutput::default();
    let writer = output.clone();
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, writer, "customer-log-test", None, None)
    })
    .await
    .expect("logging setup task")
    .expect("one test subscriber");
    {
        let before = otlp::output::guest_logs(&output).len();
        let wasm = tokio::task::spawn_blocking(move || {
            common::preparation::Guest::from("isoplane_guest_dotnet_node")
        })
        .await
        .expect("the managed logging fixture must build");
        let rig = common::start_with_policy(wasm, InitSandbox::RuntimeBringUp, None, |_| {}).await;
        let left_over = common::get(rig.proxy, "quotes.isoplane.dev", "/logs/after-response").await;
        assert_eq!(
            left_over.body, "returned",
            "the probe must answer before its unawaited work logs"
        );
        let following = common::get(rig.proxy, "quotes.isoplane.dev", "/logs/logging").await;
        assert_eq!(
            following.body, "logged",
            "the following dispatch must return its own response"
        );
        let records: Vec<_> = otlp::output::guest_logs(&output)
            .into_iter()
            .skip(before)
            .collect();
        let find = |text: &str| {
            records
                .iter()
                .find(|record| record.fields.contains(text))
                .unwrap_or_else(|| panic!("the Mono host did not emit {text}: {:?}", records))
        };
        assert_ne!(
            find("left-over-work").dispatch,
            find("console-before").dispatch,
            "work a dispatch leaves running must log as that dispatch, never as the one after it"
        );
        rig.shutdown().await;
    }
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("logging shutdown task")
        .expect("logging flush");
}
