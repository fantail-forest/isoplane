//! The host-driven WASI event loop (runtime patch 006): timers park the guest
//! on host-owned wake sources, ready work runs before a park, continuations
//! resume across destroyed Stores, and registrations are held once and
//! released with their owner.

use isoplane_harness::logging::GuestLogs;
#[macro_use]
mod common;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn a_delay_parks_the_guest_on_one_host_timer_and_resumes_on_its_wake(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/delay/50");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "Task.Delay must park the guest instead of blocking: {outcome:?}; logs: {:?}",
            reactor.context.logs()
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "one awaited delay must own exactly one host timer"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "the continuation must complete on the wake that restored the guest: {outcome:?}"
        );
        assert_eq!(
            reactor.response_text(1),
            "resumed",
            "the continuation after the delay must run"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a fired timer must leave no host registration behind"
        );
    }
}

in_mono_runtime! {
    /// The timer queue arms one host timer for its earliest deadline and re-arms
    /// it as timers fire, so concurrent delays share a park and a registration.
    #[tokio::test]
    async fn concurrent_delays_share_one_park_and_the_queues_one_host_timer(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/delays/3");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "three awaited delays must park once: {outcome:?}"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "the timer queue must hold exactly one host timer, for its earliest deadline"
        );
        let mut outcome = outcome;
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            assert_eq!(
                reactor.futures.pending_total(reactor.context.instance),
                1,
                "a check must re-arm the queue's one timer, never duplicate it across wakes"
            );
            wakes += 1;
            assert!(
                wakes <= 3,
                "three delays of 10, 20 and 30 ms need at most three wakes"
            );
            reactor.context.begin_slice(1 + 10 * wakes, 1);
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(reactor.response_text(1), "3", "every delay must complete");
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "finished timers must release their registration"
        );
    }
}

in_mono_runtime! {
    /// A parked timer survives hibernation: its continuation runs after the
    /// guest's memory went through the local snapshot.
    #[tokio::test(start_paused = true)]
    async fn a_parked_delay_survives_a_hibernation_delta_round_trip(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/delay/50");
        assert_eq!(outcome.completion, Completion::Parked, "{outcome:?}");
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let outcome = guest.wake_through_delta(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.logs()
        );
        assert_eq!(
            reactor.response_text(1),
            "resumed",
            "the continuation must run after the hibernation"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn sequential_awaits_each_take_their_own_wake(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let mut outcome = guest.start("/loop/sequence/5");
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            assert_eq!(
                reactor.futures.pending_total(reactor.context.instance),
                1,
                "a sequential await holds exactly the timer it is waiting on"
            );
            wakes += 1;
            assert!(wakes <= 5, "five sequential delays need exactly five wakes");
            reactor.context.begin_slice(1 + 10 * wakes, 1);
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(wakes, 5, "each awaited delay must park the guest once");
        assert_eq!(
            reactor.response_text(1),
            "5",
            "the sequence must run to its end"
        );
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn cancelling_a_delay_releases_its_registration_and_resumes_with_cancellation(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/cancel/50");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the long delay must park: {outcome:?}"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "the delay and its cancellation share the timer queue's one host timer"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "cancellation must complete the request: {outcome:?}"
        );
        assert_eq!(
            reactor.response_text(1),
            "cancelled",
            "the awaiting code must observe OperationCanceledException"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a cancelled delay must release its timer with its registration"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn queued_thread_pool_work_runs_before_the_guest_parks(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/ready-work");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the delay after the ready work must park: {outcome:?}"
        );
        reactor.context.begin_slice(11, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(outcome.completion, Completion::Done, "{outcome:?}");
        assert_eq!(
            reactor.response_text(1),
            "ran 1",
            "a queued work item must run in the invocation that queued it, before the park"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_yield_completes_through_ready_wakes_without_a_timer(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.run(&reactor, "/loop/yield", 8).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "Task.Yield must finish through ready-work wakes: {outcome:?}"
        );
        assert_eq!(reactor.response_text(1), "yielded");
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a yield must register nothing with the host"
        );
    }
}

/// How many wakes `path` needs to finish after its first slice parks.
async fn wakes_to_finish(runtime: GuestRuntime, path: &str, answer: &str) -> usize {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let outcome = guest.start(path);
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "{path} must return control to the host at its first yield: {outcome:?}"
    );
    let (answered, wakes) = guest.finish_counting(&reactor, 16).await;
    assert_eq!(answered, answer, "{path} must run to its end");
    wakes
}

in_mono_runtime! {
    #[tokio::test]
    async fn each_yield_ends_the_slice_that_awaited_it(runtime: GuestRuntime) {
        let one = wakes_to_finish(runtime, "/loop/yields/1", "yielded 1").await;
        let four = wakes_to_finish(runtime, "/loop/yields/4", "yielded 4").await;
        assert_eq!(
            four - one,
            3,
            "each further yield must take exactly one further slice"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn each_yield_in_thread_pool_work_ends_the_slice_that_awaited_it(runtime: GuestRuntime) {
        let one = wakes_to_finish(runtime, "/loop/pool-yields/1", "yielded 1").await;
        let four = wakes_to_finish(runtime, "/loop/pool-yields/4", "yielded 4").await;
        assert_eq!(
            four - one,
            3,
            "each further yield must take exactly one further slice"
        );
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn request_timers_progress_while_retained_timer_work_is_withheld(runtime: GuestRuntime) {
        use isoplane_host::futures::INSTANCE_WORK;
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_dispatch(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/abandon");
        assert_eq!(outcome.completion, Completion::Done, "{outcome:?}");
        assert!(!reactor.futures.end_dispatch(reactor.context.instance, 1));
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(200)).await;

        // Withhold background slices as an exhausted pool does. Its timer is overdue.
        for (dispatch, path) in [(2, "/loop/delay/50"), (3, "/loop/delay/100")] {
            reactor.context.begin_dispatch(201, dispatch);
            let outcome = guest.start(path);
            assert_eq!(outcome.completion, Completion::Parked, "{outcome:?}");
            assert_eq!(
                reactor
                    .futures
                    .pending_of_dispatch(reactor.context.instance, dispatch, "poll_call")
                    .len(),
                1,
                "each request must have a wake independent of the overdue background timer"
            );
        }
        for (dispatch, now_ms, advance_ms) in [(2, 261, 60), (3, 311, 50)] {
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_millis(advance_ms)).await;
            reactor.context.begin_slice(now_ms, dispatch);
            let outcome = guest.wake(&reactor).await;
            assert_eq!(
                outcome.completion,
                Completion::Done,
                "request {dispatch}: {outcome:?}"
            );
            assert_eq!(reactor.response_text(dispatch as u32), "resumed");
            reactor
                .futures
                .end_dispatch(reactor.context.instance, dispatch);
        }
        reactor.context.begin_dispatch(311, 4);
        let outcome = guest.start("/loop/rearm");
        assert_eq!(outcome.completion, Completion::Done, "{outcome:?}");
        assert_eq!(
            reactor.response_text(4),
            "4:0",
            "requests must not run the retained callback"
        );
        reactor.futures.end_dispatch(reactor.context.instance, 4);

        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        reactor.context.begin_dispatch(331, INSTANCE_WORK);
        let outcome = guest.poll();
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "background work must resume: {outcome:?}"
        );
        reactor.context.begin_dispatch(331, 5);
        let outcome = guest.start("/loop/observe");
        assert_eq!(outcome.completion, Completion::Done, "{outcome:?}");
        assert_eq!(
            reactor.response_text(5),
            "5:2",
            "handover must preserve both the pending and rearmed callbacks"
        );
        assert_eq!(reactor.futures.pending_total(reactor.context.instance), 0);
    }
}

in_mono_runtime! {
    /// Every synchronous wait the BCL offers on work that needs a host wake
    /// refuses explicitly: the guest has no thread to block and no way to park
    /// inside a call, so the alternative to an exception is a trap or a hang.
    #[tokio::test]
    async fn every_synchronous_wait_on_host_backed_work_fails_explicitly(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        let mut guest = guest(&reactor, runtime);
        for (index, (path, what)) in [
            ("/loop/sync-wait", "Task.Wait"),
            ("/loop/sync-result", "Task<T>.Result"),
            ("/loop/wait-all", "Task.WaitAll"),
            ("/loop/wait-any", "Task.WaitAny"),
            ("/loop/thread-sleep", "Thread.Sleep"),
            ("/loop/semaphore-wait", "SemaphoreSlim.Wait"),
            ("/loop/reset-event-wait", "ManualResetEventSlim.Wait"),
            ("/loop/thread-start", "Thread.Start"),
            ("/loop/blocking-enumerable", "ToBlockingEnumerable"),
        ]
        .into_iter()
        .enumerate()
        {
            let dispatch = index as u64 + 1;
            reactor.context.begin_slice(dispatch, dispatch);
            let answer = guest.answer(&reactor, path, 2).await;
            assert_eq!(
                answer, "PlatformNotSupportedException",
                "{what} cannot be honoured without threads and must say so instead of trapping or hanging"
            );
            // The delay a refused wait was waiting on ends with its dispatch.
            reactor
                .futures
                .end_dispatch(reactor.context.instance, dispatch);
            assert_eq!(
                reactor.futures.live_futures(),
                0,
                "the delay {what} was waiting on must end with its dispatch"
            );
        }
    }
}

in_mono_runtime! {
    /// Waiting on work that is already complete needs no wake and is not
    /// blocking; refusing it would break ordinary code.
    #[tokio::test]
    async fn waiting_on_completed_work_is_not_blocking(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest.answer(&reactor, "/loop/completed-wait", 1).await;
        assert_eq!(answer, "waited", "a wait that needs no wake must complete");
    }
}

async fn replacement_timer_resumes(runtime: GuestRuntime, path: &str) {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let outcome = guest.start(path);
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the replacement delay must await a timer: {outcome:?}"
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        1,
        "cancelling the last timer must not prevent its replacement from arming"
    );
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(201)).await;
    reactor.context.begin_slice(202, 1);
    let outcome = guest.wake_through_delta(&reactor).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the replacement timer must resume after restoration: {outcome:?}"
    );
    assert_eq!(
        reactor.response_text(1),
        "resumed",
        "the replacement delay must complete"
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        0,
        "replacement completion must release the host timer"
    );
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn cancelling_the_last_delay_allows_a_later_replacement_to_fire(runtime: GuestRuntime) {
        replacement_timer_resumes(runtime, "/loop/cancel-then-delay/200").await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_the_last_delay_allows_an_earlier_replacement_to_fire(runtime: GuestRuntime) {
        replacement_timer_resumes(runtime, "/loop/cancel-then-delay/50").await;
    }

    #[tokio::test(start_paused = true)]
    async fn disposing_the_last_timer_allows_a_replacement_to_fire(runtime: GuestRuntime) {
        replacement_timer_resumes(runtime, "/loop/dispose-then-delay/200").await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_delay_can_be_armed_after_the_cancelled_deadline_has_passed(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/loop/cancel-only");
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "cancelling the delay must finish without waiting: {outcome:?}"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "the cancelled delay must leave no timer"
        );
        reactor.context.begin_slice(201, 2);
        let outcome = guest.start("/loop/delay/200");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the new request must await its timer: {outcome:?}"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "an elapsed cancelled deadline must not suppress a new timer"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(201)).await;
        reactor.context.begin_slice(402, 2);
        let outcome = guest.wake_through_delta(&reactor).await;
        assert_eq!(outcome.completion, Completion::Done, "the next request's delay must resume: {outcome:?}");
        assert_eq!(reactor.response_text(2), "resumed", "the next request's delay must complete");
    }
}

async fn scheduling_survives_delta(runtime: GuestRuntime, scenario: &str) {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let outcome = guest.start(&format!("/loop/{scenario}"));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "{scenario} must await its host timer: {outcome:?}; logs: {:?}",
        reactor.context.logs()
    );
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    reactor.context.begin_slice(61, 1);
    let answer = guest.finish_through_deltas(&reactor, 8).await;
    assert_eq!(
        answer,
        format!("{scenario} resumed"),
        "the customer scheduling mechanism must run its actual continuation"
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        0,
        "completed scheduling work must release its host timer"
    );
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn a_customer_synchronization_context_resumes_after_a_delta(runtime: GuestRuntime) {
        scheduling_survives_delta(runtime, "custom-context").await;
    }
    #[tokio::test(start_paused = true)]
    async fn a_customer_task_scheduler_resumes_after_a_delta(runtime: GuestRuntime) {
        scheduling_survives_delta(runtime, "custom-scheduler").await;
    }
    #[tokio::test(start_paused = true)]
    async fn a_customer_awaiter_resumes_after_a_delta(runtime: GuestRuntime) {
        scheduling_survives_delta(runtime, "custom-awaiter").await;
    }
    #[tokio::test(start_paused = true)]
    async fn async_void_work_observed_by_its_dispatch_resumes_after_a_delta(runtime: GuestRuntime) {
        scheduling_survives_delta(runtime, "async-void").await;
    }
}
