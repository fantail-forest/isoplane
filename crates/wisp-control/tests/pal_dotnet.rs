//! **The end-to-end proof**: a stock .NET module through `wisp_control::validate`
//! (spec.md §4.3, Appendix C **F7**, claim **C10**).
//!
//! F7 is the finding this resolves:
//!
//! > *A stock .NET module declares 32 `wasi:*` imports the validator rejects;
//! > recording stubs show 2–3 reached … Resolved by the delegating adapter
//! > under the no-new-channels rule.*
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
//! `guests/dotnet/native/wisp_pal.c` is a *guest-side* answer to F7: it
//! defines the `__wasm_import_*` symbols wasi-libc leaves undefined so
//! wasm-ld resolves them locally and emits no import at all. That fix works,
//! and it is why `wisp_guest_dotnet.wasm` already validates. It is also a
//! workaround every tenant would otherwise have to carry, and
//! `docs/dotnet-findings.md` §1.4 measured that it is a **closure over the
//! BCL surface, not a fixed list** — LINQ + Regex + `System.Text.Json` grew
//! it by 12 entries. The v1.4 adapter is the platform-side answer, so the
//! artifacts it must be proved against are the ones built with the guest-side
//! PAL disabled:
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
//! written out before it fires.) They are large binaries and are not
//! committed, so each test skips loudly when its artifact is absent, in the
//! same style as `real_guests.rs`.

use wasmtime::{Engine, Linker, Module};
use wisp_abi::config::Limits;
use wisp_control::pal::{self, PalSource};
use wisp_control::validator::{self, ViolationCode as V};

fn artifact(name: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../guests/dotnet/out")
        .join(format!("{name}.wasm"));
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!(
                "skipped: {} not built (see this file's header)",
                p.display()
            );
            None
        }
    }
}

/// A host with the two capabilities the adapter draws on. Nothing is called
/// here — these tests compose the adapter over real modules without running
/// guest code, which belongs in the sandbox.
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

/// **The headline.** The module F7 describes — 32 undeclared `wasi:*` imports,
/// rejected by 32 V1 violations before v1.4 — now deploys, with no exemption
/// and no widening of `wisp_abi::imports::IMPORTS`.
#[test]
fn the_stock_dotnet_module_now_passes_v1_through_the_pal_adapter() {
    let Some(wasm) = artifact("wisp_guest_dotnet_stock") else {
        return;
    };
    let r = report("wisp_guest_dotnet_stock", &wasm);

    // F7's number, confirmed from a second build path.
    assert_eq!(
        r.pal.len(),
        32,
        "F7 measured 32 wasi imports on a stock module"
    );
    let delegated: Vec<String> = r
        .pal
        .iter()
        .filter(|p| p.disposition.is_delegated())
        .map(|p| p.full_name())
        .collect();
    println!("delegated: {delegated:#?}");
    // F7: "2–3 are ever reached", and which three is measured, not guessed —
    // `docs/dotnet-findings.md` §1.2. All three are here and nothing else is.
    assert_eq!(
        delegated,
        vec![
            "wasi:cli/environment@0.2.0::get-environment",
            "wasi:clocks/monotonic-clock@0.2.0::now",
            "wasi:random/random@0.2.0::get-random-bytes",
        ]
    );
    assert_eq!(
        r.pal.len() - delegated.len(),
        29,
        "F7: 29 dead declarations"
    );

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
    assert!(r.has_initialize(), "§4.4's pre-init entry point");

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
/// closure: it declares **44** `wasi:*` imports, 12 more than the first, and
/// `docs/dotnet-findings.md` §1.4 is explicit that a new NuGet reference can
/// widen the set again.
#[test]
fn the_compat_stock_dotnet_module_passes_v1_too() {
    let Some(wasm) = artifact("wisp_guest_dotnet_compat_stock") else {
        return;
    };
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
/// no `wasi:*` imports at all. It validated before v1.4 and still does; the
/// point of asserting it here is that the adapter changed nothing for a
/// module that does not need it — `report.pal` is empty and the manifest
/// grows no `wasi-pal` section.
#[test]
fn the_shipped_dotnet_module_still_passes_and_needs_no_adapter() {
    let Some(wasm) = artifact("wisp_guest_dotnet") else {
        return;
    };
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
    let Some(wasm) = artifact("wisp_guest_dotnet_stock") else {
        return;
    };

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

/// **The negative control for that rejection (§10).** A rejection test proves
/// nothing unless the same harness can also produce an *acceptance*: otherwise
/// "rejected" might only mean the rewrite broke the binary. So the same
/// in-place rename, pointed at another name that IS on the whitelist, must
/// leave the module deployable — and the count must still be 32.
#[test]
fn negative_control_renaming_to_another_whitelisted_import_still_passes() {
    let Some(wasm) = artifact("wisp_guest_dotnet_stock") else {
        return;
    };

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
