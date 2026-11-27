mod common;
#[path = "common/guest.rs"]
mod guest;

use isoplane_engine::{Completion, InstanceOps};
use isoplane_harness::preparation::{
    self, activate, dispatch, staged_guest, staged_guest_with_handle, Prepared,
};
use isoplane_prepare::PrepareError;

fn request(wasm: &[u8]) -> isoplane_prepare::BuildRequest {
    preparation::request(wasm, common::engine_config(), common::request())
}

/// Prepares `wasm` the way a node prepares an application on a shared runtime.
fn prepare(wasm: &[u8]) -> Result<Prepared, PrepareError> {
    preparation::prepare(&request(wasm))
}

#[test]
fn an_unused_passive_segment_survives_preparation() {
    let wasm = guest::guest(r#"(data "retained")"#, "");
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
fn memory_init_addresses_its_original_passive_segment_in_a_prepared_template() {
    let wasm = staged_guest_with_handle(
        r#"(data (i32.const 0) "active") (data $payload "payload")"#,
        "",
        "(memory.init $payload (i32.const 64) (i32.const 0) (i32.const 7))",
        "(memory.init $payload (i32.const 128) (i32.const 0) (i32.const 7)) (i64.const 0)",
    );
    let prepared = prepare(&wasm).expect("memory.init does not drop passive state");
    let mut live = activate(&prepared.engine, &prepared.template);
    assert_eq!(
        &live.memory()[64..71],
        b"payload",
        "the initializer's passive-data copy must be in the template's memory"
    );
    assert_eq!(
        dispatch(&mut live),
        Completion::Done,
        "memory.init must still address its original passive segment"
    );
    assert_eq!(
        &live.memory()[128..135],
        b"payload",
        "runtime memory.init must copy the payload in an instance of the template"
    );
    let state = live
        .capture_mutable_state()
        .expect("capture after passive copy");
    let mut restored = activate(&prepared.engine, &prepared.template);
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
fn an_originally_active_segment_is_empty_for_memory_init_in_a_prepared_template() {
    let wasm = staged_guest_with_handle(
        r#"(data $active (i32.const 0) "active")"#,
        "",
        "",
        "(memory.init $active (i32.const 128) (i32.const 0) (i32.const 1)) (i64.const 0)",
    );
    let prepared = prepare(&wasm).expect("active data is capturable");
    let engine = common::engine();
    let unprepared = engine
        .load_template(&wasm)
        .expect("the module as submitted");
    for (engine, template) in [
        (&engine, &unprepared),
        (&*prepared.engine, &prepared.template),
    ] {
        let mut live = activate(engine, template);
        assert_eq!(
            dispatch(&mut live),
            Completion::Trapped,
            "an originally active segment must be empty in an instance, prepared or not"
        );
    }
}

#[test]
fn dropping_passive_state_remains_explicitly_unsupported() {
    let wasm = staged_guest(r#"(data $payload "payload")"#, "", "(data.drop $payload)");
    let error = preparation::baseline_refusal(&request(&wasm));
    assert!(
        matches!(error, PrepareError::State(_)),
        "unrepresented data.drop state must still refuse preparation: {error}"
    );
}
