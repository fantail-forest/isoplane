//! Fixture helpers shared by the validator corpus and the WASI PAL corpus.

use isoplane_abi::config::Limits;
use isoplane_control::validator::{self, ViolationCode as V};

/// The four exports every deployable module must have, with the
/// signatures implied by `PollStatus::encode` (a packed `u64`).
pub const REQUIRED_EXPORTS: &str = r#"
  (memory (export "memory") 17 2048)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
"#;

pub fn parse(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("fixture must be assemblable")
}

pub fn codes(wasm: &[u8], limits: Limits) -> Vec<V> {
    match validator::validate(wasm, limits) {
        Ok(_) => vec![],
        Err(vs) => {
            for v in &vs {
                println!("    -> {v}");
            }
            vs.iter().map(|v| v.code).collect()
        }
    }
}

/// Assert the fixture is rejected and that `want` is among the reasons.
/// Several fixtures legitimately trip two rules at once — a `shared` memory is
/// both a memory-shape violation (V3) and a threads-proposal violation (V2) —
/// and reporting both is better tenant feedback than picking one.
pub fn assert_violates(name: &str, wat: &str, limits: Limits, want: V) {
    println!("fixture `{name}`:");
    let got = codes(&parse(wat), limits);
    assert!(
        got.contains(&want),
        "fixture `{name}`: expected {want:?}, got {got:?}"
    );
}
