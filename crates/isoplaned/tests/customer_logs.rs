//! Managed guest logs through real cells and the JSON output writer.

pub mod common;

use isoplane_abi::config::InitSandbox;
use isoplane_harness::logging::LogCapture;
use isoplane_host::futures::INSTANCE_WORK;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_logs_keep_request_and_background_ownership_across_park_wake() {
    let capture = LogCapture::default();
    let writer = capture.clone();
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, writer, "customer-log-test", None, None)
    })
    .await
    .expect("logging setup task")
    .expect("one test subscriber");
    {
        let wasm = tokio::task::spawn_blocking(move || {
            common::preparation::Guest::from("isoplane_guest_dotnet_node")
        })
        .await
        .expect("the prepared managed logging fixture must load");
        let rig = common::start_with(wasm, InitSandbox::RuntimeBringUp).await;
        let left_over = common::get(rig.proxy, "quotes.isoplane.dev", "/logs/after-response").await;
        assert_eq!(left_over.status, 200);
        assert_eq!(left_over.body, "returned");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if capture
                    .records()
                    .iter()
                    .any(|record| record.fields.contains("left-over-work"))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background logging must progress without another request");
        let response = common::get(rig.proxy, "quotes.isoplane.dev", "/logs/logging").await;
        assert_eq!(
            response.status, 200,
            "the Mono handler must finish after its logged host future"
        );
        assert_eq!(
            response.body, "logged",
            "the Mono handler must return its own response"
        );
        let records = capture.records();
        let find = |text: &str| {
            records
                .iter()
                .find(|record| record.fields.contains(text))
                .unwrap_or_else(|| panic!("the Mono host did not emit {text}: {records:?}"))
        };
        let original = find("before-response");
        let background = find("left-over-work");
        let first = find("console-before");
        assert_eq!(background.dispatch, INSTANCE_WORK);
        assert_ne!(original.dispatch, INSTANCE_WORK);
        assert_ne!(first.dispatch, INSTANCE_WORK);
        assert_ne!(original.dispatch, first.dispatch);
        assert_eq!(background.instance, original.instance);
        assert!(original.sequence < background.sequence && background.sequence < first.sequence);
        for record in [original, background] {
            assert_eq!(
                (&record.tenant, &record.app, &record.version, &record.module),
                (&first.tenant, &first.app, &first.version, &first.module),
                "handover must preserve the application's host-established ownership"
            );
        }
        let warning = find("sdk-before");
        let wake = records
            .iter()
            .find(|record| {
                record.dispatch == first.dispatch && record.fields.contains("isoplane_wake enter")
            })
            .expect("the logging request must resume through its own wake");
        let last = find("console-after");
        let critical = find("sdk-after");
        assert_eq!(
            (first.level, warning.level, last.level, critical.level),
            (2, 3, 4, 4),
            "Console and framework severities must map to canonical host levels"
        );
        assert!(
            first.sequence < wake.sequence && wake.sequence < last.sequence,
            "the managed logging request must actually park and resume between its records"
        );
        for record in [first, warning, wake, last, critical] {
            assert_eq!(
                (&record.tenant, &record.app, &record.version, &record.module),
                (
                    &"acme".to_string(),
                    &"quotes".to_string(),
                    &"v1".to_string(),
                    &"quotes@v1".to_string()
                ),
                "guest records must carry host-established ownership and deployed version"
            );
            assert_eq!(
                (&record.instance, record.dispatch),
                (&first.instance, first.dispatch),
                "park/wake must retain instance and dispatch correlation"
            );
        }
        assert!(
            capture.output().contains("console-before"),
            "managed output must also remain visible in ordinary JSON output"
        );
        rig.shutdown().await;
    }
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("logging shutdown task")
        .expect("logging flush");
}
