//! Name resolution through the source-built asynchronous WASI path (runtime
//! patch 008): forward lookups await the host resolver on a pollable,
//! literals never leave the guest, host failures reach the ordinary socket
//! errors, and the lookups that would have to block fail explicitly.

#[macro_use]
mod common;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;
#[path = "common/resolvers.rs"]
mod resolvers;

use std::sync::Arc;

use common::{guest, GuestRuntime};
use fixed_dns::FixedDns;
use no_egress::reactor_without_egress;
use resolvers::{reactor_for_dns, FailingDns, PendingDns};
use wisp_abi::native_io::DnsFailure;
use wisp_engine::Completion;
use wisp_harness::EmittedGuestLogs as _;
use wisp_harness::WAKE_DEADLINE;

const PUBLIC_V4: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34));
const PUBLIC_V6: std::net::IpAddr = std::net::IpAddr::V6(std::net::Ipv6Addr::new(
    0x2606, 0x2800, 0x220, 0x1, 0x248, 0x1893, 0x25c8, 0x1946,
));

fn dual_stack() -> Box<FixedDns> {
    Box::new(FixedDns(vec![PUBLIC_V4, PUBLIC_V6]))
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_forward_lookup_parks_on_the_host_resolver_and_returns_its_answer(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(30, dual_stack());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/dns/resolve/owned.test");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "GetHostAddressesAsync must park on the lookup's pollable: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "one lookup owns one host wake"
        );
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.response_text(1),
            format!("{PUBLIC_V4},{PUBLIC_V6}"),
            "every address the host answered must reach the caller in order"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a finished lookup must drop its stream and pollable"
        );
    }
}

in_both_runtimes! {
    /// A parked lookup survives Store restoration: the answer reaches the continuation
    /// after the guest's captured memory was restored into a new Store.
    #[tokio::test]
    async fn a_parked_lookup_survives_a_store_restoration_delta_round_trip(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(40, dual_stack());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/dns/resolve/owned.test");
        assert_eq!(outcome.completion, Completion::Parked, "{outcome:?}");
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.response_text(1),
            format!("{PUBLIC_V4},{PUBLIC_V6}"),
            "the host's answer must reach the lookup parked across the Store restoration"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn address_family_filters_apply_to_the_host_answer(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(31, dual_stack());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        assert_eq!(
            guest.answer(&reactor, "/dns/resolve4/owned.test", 2).await,
            PUBLIC_V4.to_string(),
            "an InterNetwork lookup must keep only IPv4 answers"
        );
        reactor.context.begin_slice(2, 2);
        assert_eq!(
            guest.answer(&reactor, "/dns/resolve6/owned.test", 2).await,
            PUBLIC_V6.to_string(),
            "an InterNetworkV6 lookup must keep only IPv6 answers"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_host_entry_carries_the_name_and_addresses(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(32, dual_stack());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        assert_eq!(
            guest.answer(&reactor, "/dns/entry/owned.test", 2).await,
            format!("owned.test|0|{PUBLIC_V4},{PUBLIC_V6}"),
            "GetHostEntryAsync must return the queried name, no aliases and the host's addresses"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn an_ip_literal_resolves_without_a_host_lookup_or_a_park(runtime: GuestRuntime) {
        let reactor = reactor_without_egress(33);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/dns/literal/10.1.2.3");
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "a literal must never reach the resolver or the event loop: {outcome:?}"
        );
        assert_eq!(reactor.response_text(1), "10.1.2.3");
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn resolver_failures_map_to_the_matching_socket_errors(runtime: GuestRuntime) {
        for (kind, expected) in [
            (DnsFailure::NotFound, "HostNotFound"),
            (DnsFailure::Temporary, "TryAgain"),
            (DnsFailure::Timeout, "TimedOut"),
            (DnsFailure::Permanent, "NoRecovery"),
        ] {
            let reactor = reactor_for_dns(34, Box::new(FailingDns(kind)));
            reactor.context.begin_slice(1, 1);
            let mut guest = guest(&reactor, runtime);
            assert_eq!(
                guest.answer(&reactor, "/dns/error/failing.test", 2).await,
                expected,
                "the WASI error code for {kind:?} must reach SocketException as the ordinary SocketError"
            );
            assert_eq!(
                reactor
                    .runtime()
                    .metrics
                    .egress_denials
                    .load(std::sync::atomic::Ordering::Relaxed),
                0,
                "a resolver failure is not a policy denial"
            );
            assert_eq!(
                reactor.futures.pending_total(reactor.context.instance),
                0,
                "a failed lookup must release its host work"
            );
        }
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_policy_refusal_is_access_denied_and_counted_once(runtime: GuestRuntime) {
        let mixed = Box::new(FixedDns(vec![
            PUBLIC_V4,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        ]));
        let reactor = reactor_for_dns(35, mixed);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        assert_eq!(
            guest.answer(&reactor, "/dns/error/mixed.test", 2).await,
            "AccessDenied",
            "an answer with a private address must be refused as a whole"
        );
        assert_eq!(
            reactor
                .runtime()
                .metrics
                .egress_denials
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the policy refusal must be recorded exactly once"
        );
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn cancelling_a_lookup_drops_the_host_lookup_and_its_wake(runtime: GuestRuntime) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let stopped = Arc::new(tokio::sync::Notify::new());
        let reactor = reactor_for_dns(
            36,
            Box::new(PendingDns {
                entered: entered.clone(),
                stopped: stopped.clone(),
            }),
        );
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/dns/cancel/pending.test/50");
        assert_eq!(outcome.completion, Completion::Parked, "{outcome:?}");
        tokio::time::timeout(WAKE_DEADLINE, entered.notified())
            .await
            .expect("the lookup must reach the resolver");
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            2,
            "the lookup and its cancellation timer are both host-owned"
        );
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.response_text(1), "cancelled");
        tokio::time::timeout(WAKE_DEADLINE, stopped.notified())
            .await
            .expect("cancellation must drop the in-flight host lookup");
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "cancellation must reclaim the lookup's and the timer's wakes"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn the_legacy_begin_end_pair_completes_through_its_callback(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(37, dual_stack());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        assert_eq!(
            guest.answer(&reactor, "/dns/legacy/owned.test", 2).await,
            format!("{PUBLIC_V4},{PUBLIC_V6}"),
            "BeginGetHostAddresses must complete asynchronously and EndGetHostAddresses return the answer"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn the_host_name_is_answered_in_the_guest_without_a_lookup(runtime: GuestRuntime) {
        let reactor = reactor_without_egress(39);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/dns/hostname");
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "GetHostName has no host resource to wait on: {outcome:?}"
        );
        let name = reactor.response_text(1);
        assert!(
            !name.is_empty() && !name.contains("Exception"),
            "GetHostName must answer with libc's host name, not fail: {name}"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn lookups_that_would_have_to_block_fail_explicitly(runtime: GuestRuntime) {
        let reactor = reactor_without_egress(38);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        for (index, (path, what)) in [
            ("/dns/reverse/93.184.216.34", "a reverse lookup"),
            ("/dns/sync/owned.test", "a synchronous forward lookup"),
        ]
        .into_iter()
        .enumerate()
        {
            reactor
                .context
                .begin_slice(index as u64 + 1, index as u64 + 1);
            assert_eq!(
                guest.answer(&reactor, path, 2).await,
                "PlatformNotSupportedException",
                "{what} cannot park on an empty stack and must say so instead of trapping or blocking"
            );
        }
    }
}
