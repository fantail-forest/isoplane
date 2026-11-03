use super::*;

use isoplane_abi::status::PollStatus;
use isoplane_engine::ExecutionLimit;

fn guest(reactor: &Reactor, wake_status: PollStatus) -> ReturnedGuest {
    let module = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
            (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 1)
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const {}))"#,
        wake_status.encode(),
    ))
    .expect("the occupancy fixture must assemble");
    ReturnedGuest::new(&module, reactor)
}

fn ready(reactor: &Reactor) {
    let future = reactor
        .futures
        .alloc(
            reactor.context.instance,
            reactor.context.dispatch_id(),
            "sleep",
            isoplane_abi::config::Limits::default().pending_futures_per_dispatch,
        )
        .expect("allocate the controlled completion");
    reactor
        .futures
        .complete(future, ResultRecord::Ok(Vec::new()));
}

#[tokio::test]
async fn every_resume_path_accumulates_the_engines_reported_slice_occupancy() {
    let reactor = Reactor::new(InstanceId(1));
    let mut guest = guest(&reactor, PollStatus::Pending);
    let first = guest.start("/occupancy");
    assert_eq!(first.completion, Completion::Parked);
    let mut reported_ns = first.occupancy_ns;
    assert_eq!(guest.dispatch_budget.spent_ns, reported_ns, "initial slice");

    let polled = guest.poll();
    reported_ns = reported_ns.saturating_add(polled.occupancy_ns);
    assert_eq!(polled.completion, Completion::Parked);
    assert_eq!(guest.dispatch_budget.spent_ns, reported_ns, "direct poll");

    let restored = guest.poll_through_delta().await;
    reported_ns = reported_ns.saturating_add(restored.occupancy_ns);
    assert_eq!(restored.completion, Completion::Parked);
    assert_eq!(guest.dispatch_budget.spent_ns, reported_ns, "delta poll");

    ready(&reactor);
    let woken = guest.wake(&reactor).await;
    reported_ns = reported_ns.saturating_add(woken.occupancy_ns);
    assert_eq!(woken.completion, Completion::Parked);
    assert_eq!(guest.dispatch_budget.spent_ns, reported_ns, "direct wake");

    ready(&reactor);
    let restored = guest.wake_through_delta(&reactor).await;
    reported_ns = reported_ns.saturating_add(restored.occupancy_ns);
    assert_eq!(restored.completion, Completion::Parked);
    assert_eq!(guest.dispatch_budget.spent_ns, reported_ns, "delta wake");
}

fn exhaust_dispatch(guest: &mut ReturnedGuest, outcome: DispatchOutcome) {
    let snapshot = guest.snapshot.take();
    // Supply a controlled engine outcome at the same return boundary used by
    // real slices, so budget enforcement needs no elapsed-time threshold.
    guest.record_return(
        &DispatchOutcome {
            occupancy_ns: u64::MAX,
            ..outcome
        },
        snapshot,
    );
}

#[tokio::test]
async fn every_resume_path_passes_prior_occupancy_to_the_real_engine() {
    #[derive(Debug)]
    enum Resume {
        Poll,
        DeltaPoll,
        Wake,
        DeltaWake,
    }

    for path in [
        Resume::Poll,
        Resume::DeltaPoll,
        Resume::Wake,
        Resume::DeltaWake,
    ] {
        let reactor = Reactor::new(InstanceId(1));
        let mut guest = guest(&reactor, PollStatus::Done { response: 0 });
        let first = guest.start("/occupancy");
        assert_eq!(first.completion, Completion::Parked, "{path:?}: start");
        let budget_ns = guest.dispatch_budget.budget_ns();
        exhaust_dispatch(&mut guest, first);
        let outcome = match path {
            Resume::Poll => guest.poll(),
            Resume::DeltaPoll => guest.poll_through_delta().await,
            Resume::Wake => {
                ready(&reactor);
                guest.wake(&reactor).await
            }
            Resume::DeltaWake => {
                ready(&reactor);
                guest.wake_through_delta(&reactor).await
            }
        };
        assert!(
            matches!(
                outcome.completion,
                Completion::BudgetExit | Completion::BudgetKill
            ),
            "{path:?}: an exhausted dispatch must fail its engine budget: {outcome:?}"
        );
        assert_eq!(
            outcome.execution_limit,
            Some(ExecutionLimit::Occupancy),
            "{path:?}"
        );
        assert_eq!(
            outcome.overage_ns,
            u64::MAX - budget_ns,
            "{path:?}: accumulated overage"
        );
        assert_eq!(
            guest.dispatch_budget.spent_ns,
            u64::MAX,
            "{path:?}: saturating accumulation"
        );
    }
}

#[tokio::test]
async fn a_new_dispatch_and_an_image_activation_start_with_fresh_occupancy() {
    let reactor = Reactor::new(InstanceId(1));
    let mut guest = guest(&reactor, PollStatus::Pending);
    let first = guest.start("/first");
    assert_eq!(first.completion, Completion::Parked);
    exhaust_dispatch(&mut guest, first);

    let sibling = reactor.sibling(InstanceId(2));
    let mut activated = guest.activated_from_image(&sibling);
    assert_eq!(
        activated.dispatch_budget.spent_ns, 0,
        "an image carries guest state, not another dispatch's usage"
    );
    let copied = activated.start("/image");
    assert_eq!(copied.completion, Completion::Parked);
    assert_eq!(activated.dispatch_budget.spent_ns, copied.occupancy_ns);

    let next = guest.start("/next");
    assert_eq!(next.completion, Completion::Parked);
    assert_eq!(
        guest.dispatch_budget.spent_ns, next.occupancy_ns,
        "a new dispatch excludes the previous dispatch's slices"
    );
}
