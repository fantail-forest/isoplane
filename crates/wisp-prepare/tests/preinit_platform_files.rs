#![forbid(unsafe_code)]
//! A declared platform file is visible inside the sealed init sandbox: a
//! static initialiser that reads it bakes its bytes into the template,
//! through the same `wasi:filesystem` imports wasi-libc uses.

use std::sync::Arc;

use wasmtime as wt;
use wisp_abi::filesystem::{Operation, IMPORTS};
use wisp_abi::imports::Ty;
use wisp_engine::{ModuleHash, PlatformFile, PlatformFiles};
use wisp_prepare::{PreInitRequest, PreInitializer};

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

/// A guest whose `wisp_initialize` opens and reads the declared path through
/// the raw `wasi:filesystem` imports and copies what it read to `COPY_AT`.
fn guest() -> Vec<u8> {
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
    let source = format!(
        r#"(module
  {imports}
  (memory (export "memory") 4 4)
  (global $heap (mut i32) (i32.const 40000))
  (data (i32.const 2000) "{PATH}")
  (func (export "cabi_realloc") (param i32 i32 i32) (param $size i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap
      (i32.and (i32.add (i32.add (global.get $heap) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))
  (func (export "wisp_initialize") (result i64)
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
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#,
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

fn request(wasm: &[u8]) -> PreInitRequest {
    PreInitRequest {
        module_hash: ModuleHash::of(wasm),
        init_blob: Vec::new(),
    }
}

/// The template's linear memory after instantiation, with every import bound
/// to a trap: a template that reached one here would be baking a host
/// answer.
fn template_memory(template: &[u8]) -> Vec<u8> {
    let engine = wt::Engine::default();
    let module = wt::Module::new(&engine, template).expect("the template is a valid module");
    let mut linker: wt::Linker<()> = wt::Linker::new(&engine);
    for import in module.imports() {
        let wt::ExternType::Func(ty) = import.ty() else {
            panic!("the template imports only functions");
        };
        let name = format!("{}::{}", import.module(), import.name());
        linker
            .func_new(import.module(), import.name(), ty, move |_, _, _| {
                wt::bail!("the template must not call `{name}` while instantiating")
            })
            .expect("a trap stub links");
    }
    let mut store = wt::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("the template instantiates without reaching an import");
    instance
        .get_memory(&mut store, "memory")
        .expect("the template exports its memory")
        .data(&store)
        .to_vec()
}

#[test]
fn a_declared_file_read_during_pre_init_is_baked_into_the_template() {
    let wasm = guest();
    let outcome = PreInitializer::new()
        .with_platform_files(declared())
        .run(&wasm, &request(&wasm))
        .expect("pre-init converges");
    let memory = template_memory(&outcome.template_wasm);
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
        "the bytes the initialiser read are the declared bytes, baked into the template"
    );
}

#[test]
fn without_a_declared_set_the_same_path_is_no_entry_during_pre_init() {
    let wasm = guest();
    let outcome = PreInitializer::new()
        .run(&wasm, &request(&wasm))
        .expect("pre-init converges");
    let memory = template_memory(&outcome.template_wasm);
    assert_eq!(
        (memory[OPEN_AT], memory[OPEN_AT + 4]),
        (1, 20),
        "an undeclared path is no-entry inside the init sandbox"
    );
}

#[test]
fn the_local_template_identity_binds_its_declared_files() {
    let wasm = guest();
    let request = request(&wasm);
    let declared = PreInitializer::new()
        .with_platform_files(declared())
        .run(&wasm, &request)
        .expect("declared-file initialization");
    let empty = PreInitializer::new()
        .run(&wasm, &request)
        .expect("empty-file initialization");
    assert_ne!(
        declared.reproducible_digest, empty.reproducible_digest,
        "templates initialized from different declared file sets need different identities"
    );
}
