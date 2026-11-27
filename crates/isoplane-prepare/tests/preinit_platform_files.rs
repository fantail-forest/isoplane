#![forbid(unsafe_code)]
//! A declared platform file is visible inside the init sandbox: an
//! initialiser that reads it leaves its bytes in the template, through the
//! same `filesystem_call` packets the native wrappers send.

use std::sync::Arc;

use isoplane_abi::filesystem::{Operation, IMPORTS};
use isoplane_engine::{PlatformFile, PlatformFiles};
use isoplane_harness::preparation;

mod common;
#[path = "common/packets.rs"]
mod packets;

const BUNDLE: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIBinit\n-----END CERTIFICATE-----\n";
const PATH: &str = "etc/ssl/certs/ca-certificates.crt";
/// Where the initialiser leaves the open-at result, the read result and a
/// copy of the bytes it read.
const OPEN_AT: usize = 1200;
const READ_AT: usize = 1300;
const COPY_AT: usize = 4096;

fn declared() -> PlatformFiles {
    PlatformFiles::new(vec![PlatformFile {
        path: format!("/{PATH}"),
        bytes: Arc::from(BUNDLE),
        modified_ns: 7,
    }])
    .expect("one declared file")
}

/// A guest whose `isoplane_initialize` opens and reads the declared path through
/// filesystem packets and copies what it read to `COPY_AT`.
fn guest() -> Vec<u8> {
    let imports = packets::FILESYSTEM_IMPORT;
    let operations = IMPORTS
        .iter()
        .map(|definition| {
            packets::function(
                &format!("fs{}", definition.operation as u32),
                "filesystem_call",
                definition.operation as u32,
                definition.sig,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"(module
  {imports}
  (memory (export "memory") 4 4)
  (global $heap (export "heap") (mut i32) (i32.const 40000))
  (data (i32.const 2000) "{PATH}")
  {operations}
  (func (export "cabi_realloc") (param i32 i32 i32) (param $size i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap
      (i32.and (i32.add (i32.add (global.get $heap) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))
  (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (call $fs{directories} (i32.const 1000))
    (i32.store (i32.const 1010) (i32.load (i32.load (i32.const 1000))))
    (call $fs{open_at_op} (i32.load (i32.const 1010)) (i32.const 0)
      (i32.const 2000) (i32.const {path_len}) (i32.const 0) (i32.const 0) (i32.const {open_at}))
    (if (i32.eqz (i32.load8_u (i32.const {open_at})))
      (then
        (call $fs{read_op} (i32.load (i32.const {open_at_handle})) (i64.const 4096) (i64.const 0)
          (i32.const {read_at}))
        (memory.copy (i32.const {copy_at})
          (i32.load (i32.const {read_pointer})) (i32.load (i32.const {read_length}))))) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#,
        directories = Operation::Directories as u32,
        open_at_op = Operation::OpenAt as u32,
        open_at = OPEN_AT,
        open_at_handle = OPEN_AT + 4,
        read_op = Operation::Read as u32,
        read_at = READ_AT,
        read_pointer = READ_AT + 4,
        read_length = READ_AT + 8,
        copy_at = COPY_AT,
        path_len = PATH.len(),
    );
    wat::parse_str(source).expect("the fixture assembles")
}

/// The memory of the template `guest` prepares to on a node that declares `files`.
fn template_memory(files: PlatformFiles) -> Vec<u8> {
    let wasm = guest();
    let mut engine = common::engine_config();
    engine.platform_files = files;
    preparation::prepare(&preparation::request(&wasm, engine, common::request()))
        .expect("the initializer completes")
        .memory()
}

#[test]
fn a_declared_file_read_during_initialization_is_captured_in_the_template() {
    let memory = template_memory(declared());
    assert_eq!(
        memory[OPEN_AT],
        0,
        "the declared path opens inside the init sandbox (error code {})",
        memory[OPEN_AT + 4]
    );
    assert_eq!(
        memory[READ_AT], 0,
        "the declared file reads inside the init sandbox"
    );
    assert_eq!(
        &memory[COPY_AT..COPY_AT + BUNDLE.len()],
        BUNDLE,
        "the bytes the initialiser read are the declared bytes, captured in the template"
    );
}

#[test]
fn without_a_declared_set_the_same_path_is_no_entry_during_initialization() {
    let memory = template_memory(PlatformFiles::default());
    assert_eq!(
        (memory[OPEN_AT], memory[OPEN_AT + 4]),
        (1, 20),
        "an undeclared path is no-entry inside the init sandbox"
    );
}
