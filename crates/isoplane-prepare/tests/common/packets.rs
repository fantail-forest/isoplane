//! WAT functions that reach filesystem and native I/O operations the way the
//! SDK's native wrappers do: a packet of scalar arguments through an
//! `isoplane` import.

use isoplane_abi::imports::{Sig, Ty};

/// Where the functions lay out a packet. The guests these tests build keep
/// their own data below it.
const PACKET: u32 = 60_000;

#[allow(
    dead_code,
    reason = "a suite sends its packets through one of the two imports"
)]
pub const FILESYSTEM_IMPORT: &str =
    r#"(import "isoplane" "filesystem_call" (func $filesystem_call (param i32 i32 i32)))"#;
#[allow(
    dead_code,
    reason = "a suite sends its packets through one of the two imports"
)]
pub const SOCKET_IMPORT: &str =
    r#"(import "isoplane" "socket_call" (func $socket_call (param i32 i32 i32) (result i64)))"#;

fn wat_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    }
}

/// A function `$name` with the canonical signature `sig` that sends
/// `operation` through the imported function `$import`.
pub fn function(name: &str, import: &str, operation: u32, sig: Sig) -> String {
    let parameters = sig
        .params
        .iter()
        .map(wat_type)
        .collect::<Vec<_>>()
        .join(" ");
    let stores = sig
        .params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let value = match ty {
                Ty::I32 => format!("(i64.extend_i32_u (local.get {index}))"),
                Ty::I64 => format!("(local.get {index})"),
            };
            format!(
                "(i64.store (i32.const {}) {value})",
                PACKET + index as u32 * 8
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let call = format!(
        "(call ${import} (i32.const {operation}) (i32.const {PACKET}) (i32.const {}))",
        sig.params.len()
    );
    let (result, body) = match (import, sig.results.first()) {
        ("filesystem_call", _) => (String::new(), call),
        (_, None) => (String::new(), format!("(drop {call})")),
        (_, Some(Ty::I32)) => ("(result i32)".to_owned(), format!("(i32.wrap_i64 {call})")),
        (_, Some(Ty::I64)) => ("(result i64)".to_owned(), call),
    };
    format!("(func ${name} (param {parameters}) {result} {stores} {body})")
}
