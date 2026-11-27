//! The debug runtime's native console output in the guest log.

use std::time::Duration;

use isoplane_harness::logging::GuestLogs;
use isoplane_host::bindings::CellBindings;
use isoplane_host::Activator;
use tokio::net::TcpListener;

use super::debugger_transport::{
    debug_activator, debug_reactor, exchange, first_dispatch, StopOnDrop,
};

#[tokio::test]
async fn a_debug_runtimes_native_console_writes_arrive_whole_without_their_line_endings() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let activator = debug_activator("diagnostics", endpoint);
    let _stop = StopOnDrop(activator.clone());
    let reactor = debug_reactor(endpoint);
    let mut instance = activator
        .activate(Box::new(CellBindings::new(reactor.context.clone())))
        .unwrap();
    let guest = tokio::task::spawn_blocking(move || {
        let (record, budget) = first_dispatch("/native-console");
        instance.dispatch(&record, budget)
    });
    let completed = tokio::time::timeout(Duration::from_secs(180), async {
        tokio::join!(exchange(&listener), async {
            let outcome = guest.await.expect("guest worker must return to the host");
            assert_eq!(
                outcome.completion,
                isoplane_engine::Completion::Done,
                "the native console writes must not end the instance: {outcome:?}; logs: {:?}",
                reactor.context.logs()
            );
        });
    })
    .await;
    assert!(
        completed.is_ok(),
        "the guest must answer once the debugger releases it; logs: {:?}",
        reactor.context.logs()
    );
    let records: Vec<(u8, String)> = reactor
        .context
        .logs()
        .into_iter()
        .map(|record| (record.level, record.fields))
        .collect();
    // The package also writes an empty buffer and a lone line ending between
    // the second record and the last, which are no records at all.
    let expected = [
        (2, "standard\0output".to_owned()),
        (3, "standard\0error".to_owned()),
        (2, "end".to_owned()),
    ];
    assert!(
        records.windows(expected.len()).any(|logged| logged == expected),
        "each write must be one record holding every byte up to its line endings, standard output as information and standard error as a warning: expected {expected:?} in {records:?}"
    );
}
