//! The filesystem conformance rig: a `.wat` guest importing every filesystem
//! operation, with the strings a test's paths need laid out in its data
//! section, dispatched once so a test reads the result areas back out of
//! guest memory.

use crate::common::{activate, guest, DONE};
use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_abi::filesystem::{Operation, IMPORTS};
use isoplane_abi::imports::Ty;
use isoplane_engine::{Completion, InstanceOps, IsoplaneEngine, OccupancyBudget};

/// The preopened root descriptor, stored by the module prologue.
pub const ROOT: &str = "(i32.load (i32.const 1010))";
/// The default result area.
pub const RET: &str = "(i32.const 1100)";
/// Linear memory pages: room for a list as long as the ABI's out-buffer cap.
const PAGES: u32 = 24;

/// A guest whose `isoplane_handle` runs `body` after fetching the preopens.
pub fn module(body: &str, strings: &[(u32, &str)]) -> Vec<u8> {
    lifecycle_module(body, DONE, strings)
}

/// As [`module`], with `wake` as the body of `isoplane_wake`, for a test that
/// parks between two filesystem calls.
pub fn lifecycle_module(body: &str, wake: &str, strings: &[(u32, &str)]) -> Vec<u8> {
    let imports = IMPORTS
        .iter()
        .map(|definition| {
            let signature = definition.pal.sig.expect("filesystem signature");
            let types = signature
                .params
                .iter()
                .map(|ty| match ty {
                    Ty::I32 => "i32",
                    Ty::I64 => "i64",
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "(import \"{}\" \"{}\" (func $fs{} (param {types})))",
                definition.pal.module, definition.pal.func, definition.operation as u32
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let data = strings
        .iter()
        .map(|(offset, text)| format!("(data (i32.const {offset}) \"{text}\")"))
        .collect::<Vec<_>>()
        .join("\n");
    let functions = format!(
        r#"
        (global $heap (export "g_fs_heap") (mut i32) (i32.const 40000))
        (func (export "cabi_realloc") (param i32 i32 i32) (param $size i32) (result i32)
            (local $p i32)
            (local.set $p (global.get $heap))
            (global.set $heap
              (i32.and (i32.add (i32.add (global.get $heap) (local.get $size)) (i32.const 15))
                       (i32.const -16)))
            (local.get $p))
        {data}
    "#
    );
    let preopen = format!("(call $fs{} (i32.const 1000)) (i32.store (i32.const 1010) (i32.load (i32.load (i32.const 1000))))", Operation::Directories as u32);
    guest(
        PAGES,
        &imports,
        &functions,
        &format!("{preopen} {body} {DONE}"),
        wake,
    )
}

/// Dispatch `wasm` once on `engine` and return the guest's memory.
pub fn run(engine: &IsoplaneEngine, wasm: &[u8]) -> Vec<u8> {
    let template = engine
        .load_template(wasm)
        .expect("filesystem imports must validate and link");
    let mut instance = activate(engine, &template);
    assert_eq!(
        instance
            .dispatch(&dispatch(), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "a filesystem result must return normally through the engine"
    );
    instance.memory().to_vec()
}

pub fn dispatch() -> DispatchRecord {
    DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://filesystem.test/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 0,
    }
}

pub fn call(operation: Operation, arguments: &[&str]) -> String {
    format!("(call $fs{} {})", operation as u32, arguments.join(" "))
}
