mod common;

use isoplane_engine::bindings::{BindingError, InitializationBindings};
use isoplane_engine::{Completion, ImportCall, Instance, InstanceOps, OccupancyBudget};

struct NoImports;
impl InitializationBindings for NoImports {
    fn begin_slice(&mut self) {}
    fn call_sync(&mut self, call: ImportCall<'_>) -> Result<i64, BindingError> {
        Err(BindingError::Unsupported(call.import))
    }
    fn call_fill(&mut self, call: ImportCall<'_>, _output: &mut [u8]) -> Result<i64, BindingError> {
        Err(BindingError::Unsupported(call.import))
    }
    fn runtime_bringup(&self) -> bool {
        false
    }
}

fn dispatch(instance: &mut Instance) -> Completion {
    let record = isoplane_abi::dispatch::DispatchRecord {
        dispatch_id: 1,
        kind: isoplane_abi::dispatch::DispatchKind::CronTick {
            name: "probe".into(),
            scheduled_ms: 0,
        },
        trace: Default::default(),
        cancel_future: 0,
        occupancy_budget_ms: 1000,
        now_ms: 0,
    };
    instance
        .dispatch(&record, OccupancyBudget::http())
        .completion
}

#[test]
fn an_unused_passive_segment_survives_preparation() {
    let wasm = common::guest(r#"(data "retained")"#, "");
    let output = isoplane_prepare::initialize(&common::engine(), &wasm, &common::request())
        .expect("passive data is representable");
    let mut passive = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(&output.wasm) {
        if let wasmparser::Payload::DataSection(reader) = payload.expect("baked section") {
            for segment in reader {
                let segment = segment.expect("baked data");
                if matches!(segment.kind, wasmparser::DataKind::Passive) {
                    passive.push(segment.data.to_vec());
                }
            }
        }
    }
    assert_eq!(
        passive,
        vec![b"retained".to_vec()],
        "baking must retain unused passive payloads"
    );
}

#[test]
fn memory_init_keeps_original_segment_indices_before_and_after_baking() {
    let wasm = common::guest_with_handle(
        r#"(data (i32.const 0) "active") (data $payload "payload")"#,
        "(memory.init $payload (i32.const 64) (i32.const 0) (i32.const 7))",
        "(memory.init $payload (i32.const 128) (i32.const 0) (i32.const 7)) (i64.const 0)",
    );
    let engine = common::engine();
    let output = isoplane_prepare::initialize(&engine, &wasm, &common::request())
        .expect("memory.init does not drop passive state");
    let input_hash = *blake3::hash(&wasm).as_bytes();
    let identity = isoplane_engine::prepared::PreparedIdentity {
        input_hash,
        runtime_profile: isoplane_prepare::RuntimeProfile::raw_wasm()
            .digest()
            .expect("raw profile"),
        initialization_hash: common::request()
            .digest(input_hash)
            .expect("initialization identity"),
    };
    let directory = tempfile::tempdir().expect("owned publication");
    let path = directory.path().join("prepared");
    engine
        .prepare_template(&output.wasm, identity)
        .expect("native preparation")
        .publish(&path)
        .expect("local publication");
    // SAFETY: nothing changes the published files while the template maps them.
    let template = unsafe { engine.load_prepared(&path, identity, &[]) }
        .expect("native passive-data artifact");
    let mut live = engine
        .activate_initialization(&template, Box::new(NoImports))
        .expect("baked activation");
    assert_eq!(
        &live.memory()[64..71],
        b"payload",
        "the initializer's passive-data copy must be baked into memory"
    );
    assert_eq!(
        dispatch(&mut live),
        Completion::Done,
        "memory.init must still address its original passive segment"
    );
    assert_eq!(
        &live.memory()[128..135],
        b"payload",
        "runtime memory.init must retain the payload after baking"
    );
    let state = live
        .capture_mutable_state()
        .expect("capture after passive copy");
    let mut restored = engine
        .activate_initialization(&template, Box::new(NoImports))
        .expect("independent activation");
    restored
        .restore_mutable_state(&state)
        .expect("restore explicit state");
    assert_eq!(
        dispatch(&mut restored),
        Completion::Done,
        "passive payloads must remain available after restoring into another store"
    );
    assert_eq!(
        &restored.memory()[128..135],
        b"payload",
        "restored runtime must copy the original passive bytes"
    );
}

#[test]
fn originally_active_segments_remain_empty_for_memory_init() {
    let wasm = common::guest_with_handle(
        r#"(data $active (i32.const 0) "active")"#,
        "",
        "(memory.init $active (i32.const 128) (i32.const 0) (i32.const 1)) (i64.const 0)",
    );
    let engine = common::engine();
    let output =
        isoplane_prepare::initialize(&engine, &wasm, &common::request()).expect("active data bake");
    for bytes in [&wasm, &output.wasm] {
        let template = engine.load_template(bytes).expect("module");
        let mut live = engine
            .activate_initialization(&template, Box::new(NoImports))
            .expect("activation");
        assert_eq!(
            dispatch(&mut live),
            Completion::Trapped,
            "an originally active segment must be empty after instantiation on both paths"
        );
    }
}

#[test]
fn dropping_passive_state_remains_explicitly_unsupported() {
    let wasm = common::guest(r#"(data $payload "payload")"#, "(data.drop $payload)");
    assert!(
        matches!(
            isoplane_prepare::initialize(&common::engine(), &wasm, &common::request()),
            Err(isoplane_prepare::PrepareError::State(_))
        ),
        "unrepresented data.drop state must still refuse preparation"
    );
}
