//! **The end-to-end proof**: a stock .NET module through
//! `wisp_control::validate`.
//!
//! A stock .NET module declares a `wasi:*` surface the raw capability table
//! rejects, and only a couple of those imports are ever reached. The
//! delegating adapter resolves them under the no-new-channels rule.
//!
//! Both directions or the check means nothing, so this file runs four
//! artifacts:
//!
//! | artifact | shape | expected |
//! |---|---|---|
//! | `wisp_guest_dotnet_stock` | 4 `wisp` + **32 `wasi:*`** | ACCEPTED via the adapter |
//! | `wisp_guest_dotnet_compat_stock` | 4 `wisp` + **44 `wasi:*`** | ACCEPTED via the adapter |
//! | `wisp_guest_dotnet` | 7 `wisp` + 0 `wasi:*` | ACCEPTED, adapter not needed |
//! | either stock module, one import renamed | 1 import off-whitelist | REJECTED, V1 |
//!
//! ## Where the stock artifacts come from
//!
//! `guests/dotnet/native/wisp_pal.c` is the *guest-side* answer: it defines
//! the `__wasm_import_*` symbols wasi-libc leaves undefined so wasm-ld
//! resolves them locally and emits no import at all. That fix works, and it is
//! why `wisp_guest_dotnet.wasm` already validates. It is also a workaround
//! every tenant would otherwise have to carry, and the surface it papers over
//! is a **closure over the BCL, not a fixed list** — LINQ, `Regex` and
//! `System.Text.Json` widen it. The adapter is the platform-side answer, so
//! the artifacts it must be proved against are the ones built with the
//! guest-side PAL disabled:
//!
//! ```bash
//! WISP_OUT=wisp_guest_dotnet_stock \
//!   scripts/dev guests/dotnet/build.sh -p:WispPalObject=
//! WISP_OUT=wisp_guest_dotnet_compat_stock \
//!   scripts/dev guests/dotnet/build.sh -p:WispPalObject= -p:WispCompatProbe=true
//! ```
//!
//! (`build.sh` exits 1 on its own import gate — that gate is the guest-side
//! rule and this test is about the platform-side one — but the module is
//! written out before it fires.) They are built on demand by the helper below
//! rather than committed, and a missing one is a hard failure rather than a
//! skip: skipping would trade a real assertion for a green test that asserted
//! nothing.

use wasmtime::{Engine, Linker, Module};
use wisp_abi::config::Limits;
use wisp_control::pal::{self, PalSource};
use wisp_control::validator::{self, ViolationCode as V};

/// Build the C# guest fixtures if they are missing or stale, then hand back the
/// bytes.
///
/// These modules used to be committed, and this function used to be a `panic!`
/// telling you the checkout was broken. The argument for committing them was
/// that building them needs a .NET SDK most checkouts lack — which describes
/// checkouts that cannot run this test either. The SDK is a prerequisite of the
/// test, not of the build, and `scripts/dev` already has it installed. The cost
/// of the old rule was six tracked binaries that went stale the moment anyone
/// edited the shared PAL, with tests still asserting against them.
///
/// So the fixture is built the way a Rust test dependency is: on demand, by the
/// thing that needs it. `--if-stale` makes the common case a 39 ms no-op, and
/// the script takes an flock because cargo runs test binaries concurrently.
fn dotnet_fixture(name: &str) -> Vec<u8> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/<crate> has a grandparent")
        .to_path_buf();
    let script = root.join("guests/build-dotnet.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("--if-stale")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "{} failed, so the C# fixtures could not be built:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let p = root.join("guests/dotnet/out").join(format!("{name}.wasm"));
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "{} is still missing after a successful build: {e}\n\n\
             Either the build script no longer produces this fixture, or its \n\
             name changed. Both are bugs in guests/build-dotnet.sh rather than \n\
             in your checkout.",
            p.display()
        )
    })
}

fn artifact(name: &str) -> Vec<u8> {
    dotnet_fixture(name)
}

/// A host with the two capabilities the adapter draws on. Nothing is called
/// here — these tests compose the adapter over real modules without ever
/// running guest code.
struct Host;
impl PalSource for Host {
    fn slice_clock_ms(&mut self) -> u64 {
        0
    }
    fn entropy_fill(&mut self, _out: &mut [u8]) {}
}

fn report(name: &str, wasm: &[u8]) -> validator::ValidationReport {
    match validator::validate(wasm, Limits::default()) {
        Ok(r) => {
            println!("{name}.wasm ({} bytes): ACCEPTED", wasm.len());
            println!("{}", r.manifest());
            r
        }
        Err(vs) => {
            for v in &vs {
                println!("    -> {v}");
            }
            panic!("{name} was rejected by {} violation(s)", vs.len());
        }
    }
}

// ---------------------------------------------------------------------------
// direction 1: it passes
// ---------------------------------------------------------------------------

/// **The headline.** A module with 32 undeclared `wasi:*` imports — 32 V1
/// violations before the adapter existed — now deploys, with no exemption and
/// no widening of `wisp_abi::imports::IMPORTS`.
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
    println!("delegated: {delegated:#?}");
    // Which imports are actually reached is observed, not guessed. All three
    // are here and nothing else is.
    assert_eq!(
        delegated,
        vec![
            "wasi:cli/environment@0.2.0::get-environment",
            "wasi:clocks/monotonic-clock@0.2.0::now",
            "wasi:random/random@0.2.0::get-random-bytes",
        ]
    );
    assert_eq!(r.pal.len() - delegated.len(), 29, "29 dead declarations");

    // **The manifest stays honest.** This module imports neither
    // `clock_now_coarse` nor `entropy_fill` from `wisp` — it reaches both
    // through WASI — so both must appear as capabilities it holds, and the
    // report must say through which door.
    assert!(r.capabilities.contains(&"clock_now_coarse"));
    assert!(r.capabilities.contains(&"entropy_fill"));
    let m = r.manifest();
    assert!(m.contains("wasi:clocks/monotonic-clock@0.2.0::now -> delegates to `clock_now_coarse`"));
    assert!(m.contains("wasi:random/random@0.2.0::get-random-bytes -> delegates to `entropy_fill`"));
    assert!(m.contains("trap-on-call"));

    // It is still a deployable module by every other rule.
    assert!(r.has_initialize(), "the pre-init entry point");

    // And the composition the validator promised actually links: every one of
    // the 32 gets a body, delegated or trapping. A validator that accepts a
    // module the linker cannot satisfy has moved the failure from deploy to
    // first request, which is strictly worse.
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).expect("real module must compile");
    let mut linker: Linker<Host> = Linker::new(&engine);
    let composed = pal::link(&mut linker, &module).expect("the adapter must compose all 32");
    assert_eq!(composed.len(), 32);
}

/// The same, for the module that drags LINQ, source-generated `Regex` and
/// `System.Text.Json` out of the BCL. This is the artifact that decides
/// whether the whitelist was sized to a hello-world or to the measured
/// closure: it declares **44** `wasi:*` imports, and a new NuGet reference can
/// widen the set again.
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

/// The shipped module, which carries the guest-side C PAL and therefore has
/// no `wasi:*` imports at all. It validated before the adapter existed and
/// still does; the point of asserting it here is that the adapter changed
/// nothing for a module that does not need it — `report.pal` is empty and the manifest
/// grows no `wasi-pal` section.
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

// ---------------------------------------------------------------------------
// direction 2: it still fails
// ---------------------------------------------------------------------------

/// Rename one import inside a real module, in place, preserving every length
/// so the binary stays well-formed. The name appears in the import section and
/// (harmlessly) in wasi-libc's `component-type` custom section; replacing both
/// is fine, because custom sections are opaque to the validator.
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

/// **The other direction, on the real artifact.** One byte of one import name
/// changed in the actual 900 kB .NET module, and the deploy is refused. The
/// whitelist is a whitelist: it admits the functions it enumerates, not the
/// interfaces they live in and not names that merely look like them.
#[test]
fn a_real_module_with_one_off_whitelist_wasi_import_still_fails_v1() {
    let wasm = artifact("wisp_guest_dotnet_stock");

    // 1. A trap-on-call entry, renamed. Same interface, same shape, one
    //    character different: still an attempted capability.
    let mutated = rename_import(&wasm, "get-directories", "get-directoryZZ");
    let vs = validator::validate(&mutated, Limits::default())
        .expect_err("an off-whitelist wasi import must be rejected");
    for v in &vs {
        println!("    -> {v}");
    }
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));
    let subject = vs
        .iter()
        .find(|v| v.code == V::V1UndeclaredImport)
        .and_then(|v| v.subject.clone())
        .unwrap_or_default();
    assert_eq!(subject, "wasi:filesystem/preopens@0.2.0.get-directoryZZ");
    // The rejection must name the PAL whitelist, not the `wisp` table — a
    // tenant who reads "not in the wisp:platform capability table" for a
    // `wasi:` import has been told the wrong thing.
    assert!(vs.iter().any(|v| v.detail.contains("PAL whitelist")));

    // 2. A *delegated* entry, renamed. The three reachable imports are not
    //    special-cased anywhere: they are table entries like the rest.
    let mutated = rename_import(&wasm, "get-random-bytes", "get-random-byteZ");
    let vs = validator::validate(&mutated, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));

    // 3. The interface renamed rather than the function — a WASI generation
    //    the adapter does not implement.
    let mutated = rename_import(
        &wasm,
        "wasi:random/random@0.2.0",
        "wasi:random/random@0.9.0",
    );
    let vs = validator::validate(&mutated, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));
}

/// **The negative control for that rejection.** A rejection test proves
/// nothing unless the same harness can also produce an *acceptance*: otherwise
/// "rejected" might only mean the rewrite broke the binary. So the same
/// in-place rename, pointed at another name that IS on the whitelist, must
/// leave the module deployable — and the count must still be 32.
#[test]
fn negative_control_renaming_to_another_whitelisted_import_still_passes() {
    let wasm = artifact("wisp_guest_dotnet_stock");

    // Both trap-on-call entries in `wasi:filesystem/types`, both 23 bytes.
    // The stock module imports `stat` and not `read`, so this is a real
    // substitution rather than a no-op — and the result is still 32
    // whitelisted imports.
    let swapped = rename_import(&wasm, "[method]descriptor.stat", "[method]descriptor.read");
    let r = validator::validate(&swapped, Limits::default())
        .expect("renaming one whitelisted import to another must stay deployable");
    assert_eq!(r.pal.len(), 32);
    assert!(r.pal.iter().any(|p| p.func == "[method]descriptor.read"));
    assert!(!r.pal.iter().any(|p| p.func == "[method]descriptor.stat"));

    // One character further and it is off the whitelist again, which is what
    // makes the pair a control rather than two unrelated assertions.
    let broken = rename_import(&wasm, "[method]descriptor.stat", "[method]descriptor.staZ");
    let vs = validator::validate(&broken, Limits::default()).expect_err("must be rejected");
    assert!(vs.iter().any(|v| v.code == V::V1UndeclaredImport));

    // And the identity rewrite leaves the artifact byte-identical in effect.
    let untouched = rename_import(&wasm, "get-directories", "get-directories");
    assert_eq!(
        validator::validate(&untouched, Limits::default())
            .expect("identity rewrite")
            .pal
            .len(),
        32
    );
}

// ---------------------------------------------------------------------------
// past the validator: the sealed init sandbox composes the adapter too
// ---------------------------------------------------------------------------

/// Validation gets a module *deployed*; pre-initialization is what makes a
/// .NET guest activate at a sensible cost, so a module that validates and then
/// cannot be pre-initialized has not really been made deployable. The init
/// sandbox therefore composes the same table: delegated entries under the init
/// sandbox's scope rule, the remainder trapping.
///
/// This is the test that catches the thing found the hard way — `--wasi=trap`
/// does not merely leave 29 imports unused, it *fails inside `_initialize`* on
/// `wasi:cli/environment@0.2.0::get-environment`. The runtime reaches for the
/// environment before tenant code exists.
#[test]
fn the_stock_dotnet_module_pre_initializes_through_the_adapter() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    validator::validate(&wasm, Limits::default()).expect("must validate first");

    let out = wisp_control::PreInitializer::new()
        // The managed runtime comes up outside any init unit, so its clock and
        // entropy reads are served deterministically from the escrowed seed;
        // a tenant static constructor doing the same still traps and defers.
        .with_pal(wisp_control::InitPal::RuntimeBringUp)
        .run(
            &wasm,
            &wisp_control::PreInitRequest {
                module_hash: wisp_engine::ModuleHash::of(&wasm),
                init_blob: Vec::new(),
            },
        )
        .expect("a stock .NET module must pre-initialize through the PAL adapter");

    println!(
        "stock -> template: {} -> {} bytes, {} pass(es), deferred {:?}, {} refusals",
        wasm.len(),
        out.template_bytes,
        out.passes,
        out.deferred_units,
        out.capability_refusals
    );
    println!(
        "capabilities the .NET runtime reached during bring-up: {:?}",
        out.pal_calls
    );
    assert!(out.template_bytes > 0);

    // Reproducible-given-seed: the entropy the runtime consumed came from
    // the escrowed seed, so a second run with the same seed is identical.
    let again = wisp_control::PreInitializer::new()
        .with_pal(wisp_control::InitPal::RuntimeBringUp)
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

// ---------------------------------------------------------------------------
// the last seam: the data plane loads it
// ---------------------------------------------------------------------------

/// **The whole chain, on the real artifact**: validate → pre-initialize →
/// load the resulting template into the engine the cells run.
///
/// This is the test that would have caught the failure `wispd` hit after the
/// control plane was already green:
///
/// ```text
/// V1: undeclared import `wasi:io/error@0.2.0::[resource-drop]error`
/// ```
///
/// — `wisp-engine`'s own structural check, which repeats V1 cheaply so that
/// an import outside the table never reaches a slot, and which knew about
/// `imports::IMPORTS` but not yet about `pal::PAL_IMPORTS`. A capability
/// decision that passes deploy and fails at activation is the worst possible
/// place for it to surface: the tenant has already been told yes.
///
/// The template — not the raw module — is what a cell loads, so that is what
/// this hands the engine.
#[test]
fn the_stock_dotnet_template_loads_into_the_data_plane() {
    let wasm = artifact("wisp_guest_dotnet_stock");
    validator::validate(&wasm, Limits::default()).expect("must validate");

    let out = wisp_control::PreInitializer::new()
        .with_pal(wisp_control::InitPal::RuntimeBringUp)
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
        // The .NET module declares 19..2048 pages; a smaller ceiling would
        // fail for a reason that has nothing to do with the adapter.
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
    let composed: Vec<String> = t.pal_composed().iter().map(|d| d.full_name()).collect();
    println!("data plane composed {} wasi imports", composed.len());
    assert_eq!(
        composed.len(),
        32,
        "all 32, the whole way through to the cell"
    );

    // Same two capabilities as the validator reported, reached through the
    // same table. If these ever disagree, one of the two is lying to the
    // tenant about what their code can do.
    let mut caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    caps.sort_unstable();
    caps.dedup();
    assert_eq!(caps, vec!["clock_now_coarse", "entropy_fill"]);
}

/// **The negative control** for the engine-side check, on the same real
/// artifact: rename one import off the whitelist and the data plane must
/// refuse the template. Otherwise the assertion above only proves that
/// `load_template` accepts things.
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
    println!("{msg}");
    assert!(msg.contains("V1"));
    assert!(msg.contains("get-directoryZZ"));
}
