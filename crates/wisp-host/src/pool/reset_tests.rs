//! The real engine's start function must only run for an acquisition, never pool release.

use super::{AcquireSource, Activator, InstancePool};
use crate::bindings::InstanceCtx;
use crate::disposition::policy_reset;
use crate::slot::{LifecycleTier, Slot};
use crate::{InstanceId, ModuleId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest};
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, HostBindings, ImportCall, Template, WispEngine,
};

struct FixedClock;

impl HostBindings for FixedClock {
    fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
        panic!("the reset-start fixture must not issue host work")
    }

    fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
        panic!("the reset-start fixture must only use the engine-owned clock")
    }

    fn slice_clock_ms(&self) -> u64 {
        1
    }
}

struct ResetStartActivator {
    module: ModuleId,
    engine: WispEngine,
    template: Template,
    refuse_activation: AtomicBool,
}

impl Activator for ResetStartActivator {
    fn module(&self) -> &ModuleId {
        &self.module
    }

    fn module_hash(&self) -> wisp_engine::ModuleHash {
        self.template.hash
    }

    fn abi_rev(&self) -> u32 {
        self.template.abi_rev
    }

    fn template_image(&self) -> Arc<wisp_engine::MemoryImage> {
        self.template.memory_image_arc()
    }

    fn activate(
        &self,
        _: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn wisp_engine::InstanceOps + Send>> {
        if self.refuse_activation.load(Ordering::Relaxed) {
            anyhow::bail!("fixture refuses replacement activation");
        }
        Ok(Box::new(
            self.engine.activate(&self.template, Box::new(FixedClock))?,
        ))
    }
}

fn context(id: InstanceId) -> Arc<InstanceCtx> {
    Arc::new(InstanceCtx::for_test(
        id,
        HandlerLifecycle::Fresh,
        Limits::default(),
    ))
}

fn completed_start_guest() -> (InstancePool, Arc<Slot>, Arc<ResetStartActivator>) {
    let engine = WispEngine::new(EngineConfig {
        max_instances: 4,
        max_memory_bytes: 4 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
        ..wisp_engine::EngineConfig::default()
    })
    .unwrap();
    let wasm = wat::parse_str(
        r#"(module
  (import "wisp" "clock_now_coarse" (func $clock (result i64)))
  (memory (export "memory") 2 2)
  (func $start (if (i64.ne (call $clock) (i64.const 0)) (then unreachable)))
  (start $start)
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )
    .unwrap();
    let template = engine.load_template(&wasm).unwrap();
    let activator = Arc::new(ResetStartActivator {
        module: ModuleId::new("reset-start", "v1"),
        engine,
        template,
        refuse_activation: AtomicBool::new(false),
    });
    let pool = InstancePool::new(activator.clone(), HandlerLifecycle::Fresh, 1);
    let (slot, _) = pool.acquire(context).unwrap();
    let dispatch = DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: Default::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1,
    };
    let outcome = slot
        .enter()
        .unwrap()
        .instance()
        .dispatch(&dispatch, CpuBudget::http());
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the guest must finish its request before testing post-response reset"
    );
    (pool, slot, activator)
}

#[test]
fn releasing_a_completed_guest_does_not_reexecute_its_wasm_start() {
    let (pool, slot, _) = completed_start_guest();
    pool.release(slot.clone(), policy_reset());
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "release must not execute the start function that traps after a completed dispatch"
    );
    assert_eq!(
        pool.idle_counts(),
        (0, 1),
        "the untouched guest must remain a reset candidate until another request needs it"
    );
}

#[test]
fn a_reset_start_trap_discards_the_candidate_and_activates_for_the_waiting_request() {
    let (pool, previous, _) = completed_start_guest();
    pool.release(previous.clone(), policy_reset());
    let (replacement, source) = pool
        .acquire(context)
        .expect("reset failure must fall back to activation for the same request");
    assert_eq!(
        source,
        AcquireSource::Activated,
        "a reset trap must fall back to a new template activation"
    );
    assert_ne!(
        replacement.id, previous.id,
        "a candidate that trapped during reset cannot serve the new request"
    );
    assert_eq!(
        previous.state(),
        LifecycleTier::Discarded,
        "reset failure must discard the old guest"
    );
    assert_eq!(
        pool.live(),
        1,
        "reset failure must remove the candidate from accounting before replacing it"
    );
}

#[test]
fn reset_failure_notifies_cleanup_even_when_replacement_activation_fails() {
    let (pool, slot, activator) = completed_start_guest();
    pool.release(slot.clone(), policy_reset());
    activator.refuse_activation.store(true, Ordering::Relaxed);
    let mut discarded = Vec::new();
    let result = pool.acquire_with_cleanup(context, |candidate| discarded.push(candidate.id));
    assert!(
        result.is_err(),
        "replacement activation failure must reach the caller"
    );
    assert_eq!(
        discarded,
        vec![slot.id],
        "reset candidate cleanup must run before attempting replacement activation"
    );
    assert_eq!(
        pool.live(),
        0,
        "failed replacement must leave no live pool instance"
    );
}
