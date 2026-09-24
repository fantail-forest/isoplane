/// A guest whose runtime stage runs `runtime` once, through either
/// `isoplane_initialize_runtime` or the start of `isoplane_initialize`, which
/// then runs `init`: the contract the SDK's generated exports keep.
pub fn staged_guest(imports: &str, runtime: &str, init: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {imports}
      (memory (export "memory") 2 64)
      (global $runtime_booted (mut i32) (i32.const 0))
      (global $state (mut i32) (i32.const 1))
      (func $boot_runtime
        (if (i32.eqz (global.get $runtime_booted))
          (then {runtime} (global.set $runtime_booted (i32.const 1)))))
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_initialize_runtime") (result i64) (call $boot_runtime) i64.const 0)
      (func (export "isoplane_initialize") (result i64) (call $boot_runtime) {init} i64.const 0))"#
    ))
    .expect("the staged test guest must assemble")
}
