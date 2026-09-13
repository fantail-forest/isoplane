//! Managed guest logging through real cells and the node JSON output.
#![cfg(feature = "real-backends")]

pub mod common;

/// Shared logging boundary test support.
#[path = "common/otlp.rs"]
pub mod otlp;

use wisp_abi::config::InitSandbox;
use wisp_fixtures::GuestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_logs_keep_host_ownership_across_park_wake_without_post_response_work() {
    let output = otlp::output::LogOutput::default();
    let writer = output.clone();
    let logging = tokio::task::spawn_blocking(move || {
        wispd::logs::init(true, writer, "customer-log-test", None)
    })
    .await
    .expect("logging setup task")
    .expect("one test subscriber");
    for runtime in [GuestRuntime::Mono, GuestRuntime::NativeAot] {
        let before = otlp::output::guest_logs(&output).len();
        let wasm = tokio::task::spawn_blocking(move || {
            wisp_fixtures::dotnet_for("wisp_guest_dotnet_logs", runtime)
        })
        .await
        .expect("the managed logging fixture must build");
        let rig = common::start_with_policy(wasm, InitSandbox::RuntimeBringUp, None, |cfg| {
            cfg.engine.max_memory_mib = 256;
        })
        .await;
        let completed = common::get(rig.proxy, "quotes.wisp.dev", "/after-response").await;
        assert_eq!(
            completed.body, "returned",
            "the unawaited log probe must complete its response"
        );
        let response = common::get(rig.proxy, "quotes.wisp.dev", "/logging").await;
        assert_eq!(
            response.status, 200,
            "the {runtime} handler must finish after its logged host future"
        );
        assert_eq!(
            response.body, "logged",
            "the {runtime} handler must return its own response"
        );
        let records: Vec<_> = otlp::output::guest_logs(&output)
            .into_iter()
            .skip(before)
            .collect();
        assert!(
            !records
                .iter()
                .any(|record| record.fields.contains("forbidden-after-response")),
            "a continuation from a completed response cannot log during the following request"
        );
        let find = |text: &str| {
            records
                .iter()
                .find(|record| record.fields.contains(text))
                .unwrap_or_else(|| panic!("the {runtime} host did not emit {text}: {records:?}"))
        };
        let first = find("console-before");
        let warning = find("sdk-before");
        let wake = find("wisp_wake enter");
        let last = find("console-after");
        let critical = find("sdk-after");
        assert_eq!(
            (first.level, warning.level, last.level, critical.level),
            (2, 3, 4, 4),
            "Console and framework severities must map to canonical host levels"
        );
        assert!(
            first.emission_index < wake.emission_index && wake.emission_index < last.emission_index,
            "the managed logging request must actually park and resume between its records"
        );
        for record in [first, warning, wake, last, critical] {
            assert_eq!(
                (&record.tenant, &record.module),
                (&"acme".to_string(), &"quotes@v1".to_string()),
                "guest records must carry host-established ownership and deployed version"
            );
            assert_eq!(
                (&record.instance, record.dispatch),
                (&first.instance, first.dispatch),
                "park/wake must retain instance and dispatch correlation"
            );
        }
        assert!(
            output
                .text()
                .lines()
                .any(|line| line.contains("console-before")),
            "managed output must also remain available to the formatted diagnostic output"
        );
        rig.shutdown().await;
    }
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("logging shutdown task")
        .expect("logging flush");
}
