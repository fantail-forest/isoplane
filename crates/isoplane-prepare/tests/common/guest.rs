/// A guest that carries its whole application, as a module built with the
/// Rust SDK does: `isoplane_initialize` runs `init`, and there is no runtime stage.
pub fn guest(imports: &str, init: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {imports}
      (memory (export "memory") 1 64)
      (global $state (export "state") (mut i32) (i32.const 1))
      (table (export "table") 1 funcref)
      (elem (i32.const 0) $function)
      (func $function)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_initialize") (result i64) {init} i64.const 0))"#
    ))
    .expect("the test guest must assemble")
}
