//! Synchronous waits and lock acquisitions on the guest's one thread: a call
//! that can be satisfied at once succeeds, a zero-timeout try that cannot
//! returns false, and a call that would have to block throws
//! `PlatformNotSupportedException` within the invocation that made it. A wait
//! that parked, trapped or ran into the budget shows as that completion
//! instead of an answer.

#[macro_use]
mod common;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;
use isoplane_harness::{Reactor, ReturnedGuest};

const ACQUIRED: &str = "acquired";
const REFUSED: &str = "refused";
const UNSUPPORTED: &str = "PlatformNotSupportedException";

/// Runs each scenario as its own dispatch and compares every outcome at once,
/// so a failure shows the whole primitive's table.
fn assert_outcomes(runtime: GuestRuntime, instance: u64, expected: &[(&str, &str)]) {
    let reactor = Reactor::new(isoplane_host::InstanceId(instance));
    let mut guest = guest(&reactor, runtime);
    let actual: Vec<(&str, String)> = expected
        .iter()
        .zip(1u64..)
        .map(|((scenario, _), dispatch)| {
            (*scenario, outcome(&reactor, &mut guest, scenario, dispatch))
        })
        .collect();
    let expected: Vec<(&str, String)> = expected
        .iter()
        .map(|(scenario, outcome)| (*scenario, (*outcome).to_owned()))
        .collect();
    assert_eq!(actual, expected);
}

/// The scenario's answer, or the completion that took the place of one.
fn outcome(reactor: &Reactor, guest: &mut ReturnedGuest, scenario: &str, dispatch: u64) -> String {
    reactor.context.begin_slice(dispatch, dispatch);
    let outcome = guest.start(&format!("/sync/{scenario}"));
    let answer = match outcome.completion {
        Completion::Done => guest.text_of_outcome(outcome),
        other => format!("{other:?}"),
    };
    reactor
        .futures
        .end_dispatch(reactor.context.instance, dispatch);
    answer
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_semaphore_slim_is_taken_when_free_and_refuses_only_a_wait_that_would_block(runtime: GuestRuntime) {
        assert_outcomes(runtime, 301, &[
            ("semaphore-slim/free/wait", ACQUIRED),
            ("semaphore-slim/free/wait-zero", ACQUIRED),
            ("semaphore-slim/free/wait-timeout", ACQUIRED),
            ("semaphore-slim/free/wait-token", ACQUIRED),
            ("semaphore-slim/held/wait-zero", REFUSED),
            ("semaphore-slim/held/wait", UNSUPPORTED),
            ("semaphore-slim/held/wait-timeout", UNSUPPORTED),
            ("semaphore-slim/held/wait-token", UNSUPPORTED),
            ("semaphore-slim/held/wait-cancelled", "OperationCanceledException"),
            ("semaphore-slim/async-waiter/wait", UNSUPPORTED),
        ]);
    }

    #[tokio::test]
    async fn a_signalled_event_or_wait_handle_is_acquired_and_an_unsignalled_one_refuses(runtime: GuestRuntime) {
        assert_outcomes(runtime, 302, &[
            ("manual-reset-event-slim/set/wait", ACQUIRED),
            ("manual-reset-event-slim/set/wait-zero", ACQUIRED),
            ("manual-reset-event-slim/unset/wait-zero", REFUSED),
            ("manual-reset-event-slim/unset/wait", UNSUPPORTED),
            ("manual-reset-event-slim/unset/wait-timeout", UNSUPPORTED),
            ("manual-reset-event/set/wait-one", ACQUIRED),
            ("manual-reset-event/unset/wait-one-zero", REFUSED),
            ("manual-reset-event/unset/wait-one", UNSUPPORTED),
            ("manual-reset-event/unset/wait-one-timeout", UNSUPPORTED),
            ("auto-reset-event/set/wait-one", ACQUIRED),
            ("auto-reset-event/unset/wait-one", UNSUPPORTED),
            ("wait-handle/wait-any/one-set", ACQUIRED),
            ("wait-handle/wait-any/none-set-zero", REFUSED),
            ("wait-handle/wait-any/none-set", UNSUPPORTED),
            ("wait-handle/wait-all/all-set", ACQUIRED),
            ("wait-handle/wait-all/one-unset-zero", REFUSED),
            ("wait-handle/wait-all/one-unset", UNSUPPORTED),
            ("semaphore/free/wait-one", ACQUIRED),
            ("semaphore/held/wait-one-zero", REFUSED),
            ("semaphore/held/wait-one", UNSUPPORTED),
            ("mutex/free/wait-one", ACQUIRED),
            ("mutex/owned/wait-one", ACQUIRED),
        ]);
    }

    #[tokio::test]
    async fn monitors_and_locks_are_always_acquired_and_a_monitor_wait_refuses_to_block(runtime: GuestRuntime) {
        assert_outcomes(runtime, 303, &[
            ("monitor/enter", ACQUIRED),
            ("monitor/try-enter", ACQUIRED),
            ("monitor/try-enter-timeout", ACQUIRED),
            ("monitor/lock", ACQUIRED),
            ("monitor/wait-zero", REFUSED),
            ("monitor/wait", UNSUPPORTED),
            ("monitor/wait-timeout", UNSUPPORTED),
            ("lock/enter", ACQUIRED),
            ("lock/try-enter", ACQUIRED),
            ("lock/try-enter-timeout", ACQUIRED),
            ("lock/enter-scope", ACQUIRED),
        ]);
    }

    #[tokio::test]
    async fn a_spin_lock_is_acquired_when_free_and_refuses_to_spin_on_itself(runtime: GuestRuntime) {
        assert_outcomes(runtime, 304, &[
            ("spin-lock/enter", ACQUIRED),
            ("spin-lock/try-enter", ACQUIRED),
            ("spin-lock/try-enter-timeout", ACQUIRED),
            ("spin-lock/held/try-enter-zero", REFUSED),
            ("spin-lock/held/enter", UNSUPPORTED),
            ("spin-lock/owned/enter", "LockRecursionException"),
        ]);
    }

    #[tokio::test]
    async fn reader_writer_locks_are_acquired_in_every_mode(runtime: GuestRuntime) {
        assert_outcomes(runtime, 305, &[
            ("reader-writer-lock-slim/read", ACQUIRED),
            ("reader-writer-lock-slim/read-zero", ACQUIRED),
            ("reader-writer-lock-slim/read-timeout", ACQUIRED),
            ("reader-writer-lock-slim/write", ACQUIRED),
            ("reader-writer-lock-slim/write-zero", ACQUIRED),
            ("reader-writer-lock-slim/write-timeout", ACQUIRED),
            ("reader-writer-lock-slim/upgradeable", ACQUIRED),
            ("reader-writer-lock-slim/upgradeable-zero", ACQUIRED),
            ("reader-writer-lock-slim/upgradeable-timeout", ACQUIRED),
            ("reader-writer-lock-slim/upgrade", ACQUIRED),
            ("reader-writer-lock-slim/read-held/write", "LockRecursionException"),
            ("reader-writer-lock/read", ACQUIRED),
            ("reader-writer-lock/read-timeout", ACQUIRED),
            ("reader-writer-lock/write", ACQUIRED),
            ("reader-writer-lock/write-timeout", ACQUIRED),
            ("reader-writer-lock/upgrade", ACQUIRED),
        ]);
    }

    #[tokio::test]
    async fn a_reached_countdown_or_barrier_does_not_wait_and_an_unreached_one_refuses(runtime: GuestRuntime) {
        assert_outcomes(runtime, 306, &[
            ("countdown-event/reached/wait", ACQUIRED),
            ("countdown-event/pending/wait-zero", REFUSED),
            ("countdown-event/pending/wait", UNSUPPORTED),
            ("barrier/one-participant/signal-and-wait", ACQUIRED),
            ("barrier/two-participants/signal-and-wait-zero", REFUSED),
            ("barrier/two-participants/signal-and-wait", UNSUPPORTED),
        ]);
    }

    #[tokio::test]
    async fn waiting_on_a_finished_task_returns_and_on_an_unfinished_one_refuses(runtime: GuestRuntime) {
        assert_outcomes(runtime, 307, &[
            ("task/completed/wait", ACQUIRED),
            ("task/completed/result", ACQUIRED),
            ("task/completed/wait-all", ACQUIRED),
            ("task/completed/wait-any", ACQUIRED),
            ("task/faulted/wait", "AggregateException"),
            ("task/pending/wait-zero", REFUSED),
            ("task/pending/wait", UNSUPPORTED),
            ("task/pending/result", UNSUPPORTED),
            ("task/pending/wait-all-zero", REFUSED),
            ("task/pending/wait-all", UNSUPPORTED),
            ("task/pending/wait-any-zero", REFUSED),
            ("task/pending/wait-any", UNSUPPORTED),
            ("task/queued/wait", ACQUIRED),
        ]);
    }

    #[tokio::test]
    async fn spinning_and_thread_calls_return_unless_they_would_have_to_wait(runtime: GuestRuntime) {
        assert_outcomes(runtime, 308, &[
            ("spin-wait/until-true", ACQUIRED),
            ("spin-wait/until-false-zero", REFUSED),
            ("spin-wait/until-false-timeout", UNSUPPORTED),
            ("spin-wait/until-false", UNSUPPORTED),
            ("thread/sleep-zero", ACQUIRED),
            ("thread/yield", ACQUIRED),
            ("thread/sleep", UNSUPPORTED),
            ("thread/join-self-zero", REFUSED),
            ("thread/join-self-timeout", UNSUPPORTED),
            ("thread/join-self", UNSUPPORTED),
            ("thread/join-unstarted", "ThreadStateException"),
        ]);
    }

    #[tokio::test]
    async fn a_blocking_collection_takes_and_adds_when_it_can_and_refuses_when_it_would_block(runtime: GuestRuntime) {
        assert_outcomes(runtime, 309, &[
            ("blocking-collection/item/take", ACQUIRED),
            ("blocking-collection/item/try-take", ACQUIRED),
            ("blocking-collection/item/take-from-any", ACQUIRED),
            ("blocking-collection/empty/try-take", REFUSED),
            ("blocking-collection/empty/take", UNSUPPORTED),
            ("blocking-collection/room/add", ACQUIRED),
            ("blocking-collection/full/try-add", REFUSED),
            ("blocking-collection/full/add", UNSUPPORTED),
        ]);
    }

    #[tokio::test]
    async fn thread_safe_lazy_values_and_concurrent_collections_work_on_one_thread(runtime: GuestRuntime) {
        assert_outcomes(runtime, 310, &[
            ("lazy/execution-and-publication", ACQUIRED),
            ("concurrent/dictionary", ACQUIRED),
            ("concurrent/queue", ACQUIRED),
            ("concurrent/stack", ACQUIRED),
            ("concurrent/bag", ACQUIRED),
        ]);
    }
}
