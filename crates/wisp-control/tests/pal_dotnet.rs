//! The real .NET modules through the validator, the init sandbox and the
//! data plane. The `_stock` fixtures are built without the guest-side PAL
//! object, so they carry the `wasi:*` surface NativeAOT-LLVM emits and the
//! adapter has to compose; the shipped module resolves that surface in-module
//! and needs no adapter. Each acceptance has a rejection beside it.

use wasmtime::{Engine, Linker, Module};
use wisp_abi::config::{InitSandbox, Limits};
use wisp_control::validator::{self, ViolationCode as V};
use wisp_engine::pal::{self, PalSource};

fn artifact(name: &str) -> Vec<u8> {
    wisp_fixtures::dotnet(name)
}

/// The adapter is composed over these modules, never run.
struct Host;
impl PalSource for Host {
    fn slice_clock_ms(&mut self) -> u64 {
        0
    }
    fn entropy_fill(&mut self, _out: &mut [u8]) -> wasmtime::Result<()> {
        Ok(())
    }
}

fn report(name: &str, wasm: &[u8]) -> validator::ValidationReport {
    match validator::validate(wasm, Limits::default()) {
        Ok(r) => r,
        Err(vs) => {
            let vs: Vec<String> = vs.iter().map(|v| format!("  - {v}")).collect();
            panic!("{name} was rejected:\n{}", vs.join("\n"));
        }
    }
}

#[test]
fn the_stock_dotnet_module_now_passes_v1_through_the_pal_adapter() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    let r = report("wisp_guest_dotnet_stock", &wasm);

    assert_eq!(r.pal.len(), 32, "a stock module declares 32 wasi imports");
    let delegated: Vec<String> = r
        .pal
        .iter()
        .filter(|p| p.disposition.is_delegated())
        .map(|p| p.full_name())
        .collect();
    assert_eq!(
        delegated,
        vec![
            "wasi:cli/environment@0.2.0::get-environment",
            "wasi:clocks/monotonic-clock@0.2.0::now",
            "wasi:random/random@0.2.0::get-random-bytes",
        ]
    );
    assert_eq!(r.pal.len() - delegated.len(), 29, "29 dead declarations");

    // Reached through WASI, so held as capabilities and named with the door.
    assert!(r.capabilities.contains(&"clock_now_coarse"));
    assert!(r.capabilities.contains(&"entropy_fill"));
    let m = r.manifest();
    assert!(m.contains("wasi:clocks/monotonic-clock@0.2.0::now -> delegates to `clock_now_coarse`"));
    assert!(m.contains("wasi:random/random@0.2.0::get-random-bytes -> delegates to `entropy_fill`"));
    assert!(m.contains("trap-on-call"));

    assert!(r.has_initialize(), "the pre-init entry point");

    // What the validator admitted, the linker composes.
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).expect("real module must compile");
    let mut linker: Linker<Host> = Linker::new(&engine);
    let composed = pal::link(&mut linker, &module).expect("the adapter must compose all 32");
    assert_eq!(composed.len(), 32);
}

/// The module that drags LINQ, `Regex` and `System.Text.Json` out of the
/// BCL, which is what sizes the whitelist.
#[test]
fn the_compat_stock_dotnet_module_passes_v1_too() {
    let wasm = artifact("wisp_guest_dotnet_compat_stock");
    let r = report("wisp_guest_dotnet_compat_stock", &wasm);
    assert_eq!(r.pal.len(), 44);
    assert_eq!(
        r.pal
            .iter()
            .filter(|p| p.disposition.is_delegated())
            .count(),
        3
    );
    assert!(r.capabilities.contains(&"entropy_fill"));

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut linker: Linker<Host> = Linker::new(&engine);
    assert_eq!(pal::link(&mut linker, &module).unwrap().len(), 44);
}

#[test]
fn the_shipped_dotnet_module_still_passes_and_needs_no_adapter() {
    let wasm = artifact("wisp_guest_dotnet");
    let r = report("wisp_guest_dotnet", &wasm);
    assert!(
        r.pal.is_empty(),
        "the shipped build resolves wasi-libc's imports in-module"
    );
    assert!(!r.manifest().contains("wasi-pal"));
    for c in &r.capabilities {
        assert!(wisp_abi::imports::lookup(c).is_some());
    }
}

/// Rename one import in place, preserving every length so the binary stays
/// well-formed. The name also appears in a custom section, which the
/// validator ignores.
fn rename_import(wasm: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(
        from.len(),
        to.len(),
        "the rewrite must preserve every LEB length"
    );
    let (from, to) = (from.as_bytes(), to.as_bytes());
    let mut out = wasm.to_vec();
    let mut hits = 0;
    let mut i = 0;
    while i + from.len() <= out.len() {
        if &out[i..i + from.len()] == from {
            out[i..i + from.len()].copy_from_slice(to);
            hits += 1;
            i += from.len();
        } else {
            i += 1;
        }
    }
    assert!(
        hits > 0,
        "`{}` not found in the module",
        String::from_utf8_lossy(from)
    );
    out
}

#[test]
fn a_real_module_with_one_off_whitelist_wasi_import_still_fails_v1() {
    let wasm = artifact("wisp_guest_dotnet_stock");

    // A trap-on-call entry, renamed.
    let mutated = rename_import(&wasm, "get-directories", "get-directoryZZ");
    let vs = validator::validate(&mutated, Limits::default())
        .expect_err("an off-whitelist wasi import must be rejected");
    assert!(
        vs.iter().any(|v| v.code == V::V1UndeclaredImport),
        "a renamed import is undeclared: {vs:?}"
    );
    let subject = vs
        .iter()
        .find(|v| v.code == V::V1UndeclaredImport)
        .and_then(|v| v.subject.clone())
        .unwrap_or_default();
    assert_eq!(subject, "wasi:filesystem/preopens@0.2.0.get-directoryZZ");
    assert!(
        vs.iter().any(|v| v.detail.contains("PAL whitelist")),
        "a wasi: rejection names the whitelist, not the wisp table"
    );

    // A delegated entry, renamed.
    let mutated = rename_import(&wasm, "get-random-bytes", "get-random-byteZ");
    let vs = validator::validate(&mutated, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));

    // The interface renamed: a WASI generation the adapter does not implement.
    let mutated = rename_import(
        &wasm,
        "wasi:random/random@0.2.0",
        "wasi:random/random@0.9.0",
    );
    let vs = validator::validate(&mutated, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));
}

/// The same rewrite pointed at a whitelisted name stays deployable, so a
/// rejection above is not the rewrite breaking the binary.
#[test]
fn negative_control_renaming_to_another_whitelisted_import_still_passes() {
    let wasm = artifact("wisp_guest_dotnet_stock");

    // Both trap-on-call entries in `wasi:filesystem/types`, both 23 bytes;
    // the stock module imports `stat` and not `read`.
    let swapped = rename_import(&wasm, "[method]descriptor.stat", "[method]descriptor.read");
    let r = validator::validate(&swapped, Limits::default())
        .expect("renaming one whitelisted import to another must stay deployable");
    assert_eq!(r.pal.len(), 32);
    assert!(r.pal.iter().any(|p| p.func == "[method]descriptor.read"));
    assert!(!r.pal.iter().any(|p| p.func == "[method]descriptor.stat"));

    let broken = rename_import(&wasm, "[method]descriptor.stat", "[method]descriptor.staZ");
    let vs = validator::validate(&broken, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));

    let untouched = rename_import(&wasm, "get-directories", "get-directories");
    assert_eq!(
        validator::validate(&untouched, Limits::default())
            .expect("identity rewrite")
            .pal
            .len(),
        32
    );
}

/// The runtime reaches `wasi:cli/environment::get-environment` inside
/// `_initialize`, before any tenant code, so the init sandbox has to compose
/// the adapter too.
#[test]
fn the_stock_dotnet_module_pre_initializes_through_the_adapter() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    validator::validate(&wasm, Limits::default()).expect("must validate first");

    let out = wisp_control::PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(
            &wasm,
            &wisp_control::PreInitRequest {
                module_hash: wisp_engine::ModuleHash::of(&wasm),
                init_blob: Vec::new(),
            },
        )
        .expect("a stock .NET module must pre-initialize through the PAL adapter");
    assert!(
        !out.pal_calls.is_empty(),
        "the runtime reaches the PAL through the WASI door while coming up"
    );

    let again = wisp_control::PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(
            &wasm,
            &wisp_control::PreInitRequest {
                module_hash: wisp_engine::ModuleHash::of(&wasm),
                init_blob: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        out.reproducible_digest, again.reproducible_digest,
        "the WASI entropy delegation must draw from the escrowed seed, not host entropy"
    );
    assert_eq!(
        out.template_wasm, again.template_wasm,
        "templates must be byte-identical"
    );
}

/// The template, which is what a cell loads, through the engine's own V1
/// check: a capability decision must not pass deploy and fail at activation.
#[test]
fn the_stock_dotnet_template_loads_into_the_data_plane() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    validator::validate(&wasm, Limits::default()).expect("must validate");

    let out = wisp_control::PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(
            &wasm,
            &wisp_control::PreInitRequest {
                module_hash: wisp_engine::ModuleHash::of(&wasm),
                init_blob: Vec::new(),
            },
        )
        .expect("must pre-initialize");

    let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        max_instances: 8,
        // The .NET module declares up to 2048 pages.
        max_memory_bytes: 128 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
    })
    .expect("engine");

    let t = engine
        .load_template(&out.template_wasm)
        .expect("the data plane must load a template carrying a whitelisted wasi: surface");

    assert!(
        t.uses_pal(),
        "the runtime linker should have composed the adapter"
    );
    assert_eq!(
        t.pal_composed().len(),
        32,
        "all 32, the whole way through to the cell"
    );

    // The same two capabilities the validator reported.
    let mut caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    caps.sort_unstable();
    caps.dedup();
    assert_eq!(caps, vec!["clock_now_coarse", "entropy_fill"]);
}

#[test]
fn negative_control_the_data_plane_refuses_an_off_whitelist_wasi_import() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    let mutated = rename_import(&wasm, "get-directories", "get-directoryZZ");

    let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        max_instances: 8,
        max_memory_bytes: 128 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
    })
    .expect("engine");

    let err = engine
        .load_template(&mutated)
        .expect_err("an import outside the whitelist must never reach a slot");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("V1") && msg.contains("get-directoryZZ"),
        "the refusal names the rule and the import: {msg}"
    );
}
