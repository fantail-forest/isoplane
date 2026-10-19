use super::*;
use crate::disposition::{decide, DispatchEnd, Disposition};
use crate::fakes::{FakeActivator, FakeBehaviour};
use crate::LifecycleTier;
use isoplane_abi::config::Limits;
use isoplane_engine::Completion;
use std::time::Duration;

fn pool() -> InstancePool {
    InstancePool::new(Arc::new(FakeActivator::new(FakeBehaviour::default())), 1)
}

fn acquire(pool: &InstancePool) -> Arc<Slot> {
    pool.acquire(|id| Arc::new(InstanceCtx::for_test(id, Limits::default())))
        .expect("test pool activation must succeed")
        .0
}

/// The way back to the idle list: a completed dispatch stays warm.
fn warm() -> Disposition {
    let DispatchEnd::Disposed(warm) = decide(Completion::Done, false) else {
        panic!("a completed dispatch must have a pool disposition")
    };
    warm
}

fn release(pool: &InstancePool, slot: Arc<Slot>) {
    pool.release(slot, warm());
}

#[test]
fn an_idle_claim_prevents_acquisition_without_holding_the_pool_lock() {
    let disposition = warm();
    let pool = pool();
    let slot = acquire(&pool);
    pool.release(slot.clone(), disposition);
    std::thread::scope(|scope| {
        let claim = pool
            .claim_idle(slot.id)
            .expect("idle slot must be claimable");
        let (send, receive) = std::sync::mpsc::channel();
        let pool_ref = &pool;
        scope.spawn(move || {
            let other = acquire(pool_ref);
            send.send(other.id)
                .expect("acquisition observer must remain connected");
            pool_ref.release(other, disposition);
        });
        let other = receive
            .recv_timeout(Duration::from_secs(10))
            .expect("acquisition must not hang behind an idle reclamation claim");
        assert_ne!(
            other,
            claim.slot().id,
            "acquisition must not receive the slot currently being reclaimed"
        );
    });
    assert_eq!(
        pool.live(),
        1,
        "the reclaimed slot retains its capacity while a competing release is discarded"
    );
    assert_eq!(
        acquire(&pool).id,
        slot.id,
        "returning a claim must make its original slot available again"
    );
}

#[test]
fn claims_reserve_idle_capacity_until_they_are_returned() {
    let disposition = warm();
    let pool = pool();
    let first = acquire(&pool);
    let second = acquire(&pool);
    pool.release(first.clone(), disposition);
    let claim = pool.claim_idle(first.id).expect("first idle slot");
    pool.release(second.clone(), disposition);
    assert_eq!(
        second.state(),
        LifecycleTier::Discarded,
        "a claim must keep its idle capacity reserved against competing releases"
    );
    drop(claim);
    assert_eq!(
        pool.live(),
        1,
        "only the original claimed entry remains live"
    );
    assert_eq!(
        acquire(&pool).id,
        first.id,
        "the original idle entry must survive capacity contention"
    );
}

#[test]
fn a_claim_returns_a_hibernated_entry_to_the_idle_list() {
    let disposition = warm();
    let pool = pool();
    let slot = acquire(&pool);
    pool.release(slot.clone(), disposition);
    let claim = pool.claim_idle(slot.id).expect("idle slot");
    claim.slot().hibernate().expect("idle capture boundary");
    drop(claim);
    assert_eq!(
        slot.state(),
        LifecycleTier::Hibernated,
        "claim return must not park a hibernated guest"
    );
    assert_eq!(
        pool.idle(),
        1,
        "claim return must put the entry back on the idle list"
    );
    assert_eq!(
        pool.live(),
        1,
        "a local hibernation retains its live pool identity"
    );
}

#[test]
fn a_discarded_claim_leaves_pool_accounting_once() {
    let pool = pool();
    let slot = acquire(&pool);
    release(&pool, slot.clone());
    let claim = pool.claim_idle(slot.id).expect("idle slot");
    claim.slot().discard();
    drop(claim);
    assert_eq!(
        pool.live(),
        0,
        "discarding a claimed entry must decrement live accounting"
    );
    assert_eq!(
        pool.idle(),
        0,
        "a discarded claim must not return to acquisition"
    );
    assert!(
        pool.claim_idle(slot.id).is_none(),
        "a discarded entry cannot be claimed again"
    );
}

#[test]
fn an_outstanding_claim_cannot_repopulate_a_drained_pool() {
    let pool = pool();
    let slot = acquire(&pool);
    release(&pool, slot.clone());
    let claim = pool.claim_idle(slot.id).expect("idle slot");
    assert_eq!(
        pool.drain_idle(),
        0,
        "a claimed entry remains owned by its in-progress reclamation"
    );
    drop(claim);
    assert_eq!(
        slot.state(),
        LifecycleTier::Discarded,
        "a claim from before the drain must be discarded on return"
    );
    assert_eq!(
        pool.live(),
        0,
        "a drained claim must release its pool accounting"
    );
    assert_eq!(
        pool.idle(),
        0,
        "a drain must not be undone by a returning claim"
    );
}

#[test]
fn only_one_reclaimer_can_claim_an_idle_entry() {
    let pool = pool();
    let slot = acquire(&pool);
    assert!(
        pool.claim_idle(slot.id).is_none(),
        "an acquired slot is not available to reclamation"
    );
    release(&pool, slot.clone());
    let claim = pool.claim_idle(slot.id).expect("idle slot");
    assert!(
        pool.claim_idle(slot.id).is_none(),
        "a claimed entry must be unavailable to another reclaimer"
    );
    drop(claim);
    assert!(
        pool.claim_idle(slot.id).is_some(),
        "returning an undiscarded claim must allow future reclamation"
    );
}
