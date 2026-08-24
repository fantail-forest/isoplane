//! §4.4 against a real .NET guest — the measurement that decides whether
//! pre-initialization is an optimization or a prerequisite.
//!
//! `docs/dotnet-findings.md` measured a stock C# guest at **~26.8 ms to the
//! first managed call** and ~49.5 MB RSS, against G1's p50 < 1 ms and 5,000
//! resident instances, and observed that every bit of the miss is .NET runtime
//! startup — i.e. exactly what §4.4 exists to delete:
//!
//! > *Cold activation therefore skips: assembly load (none), JIT (none),
//! > static init (already ran), config parse (already ran).*
//!
//! So this test does the honest version of that claim: take the same module,
//! run it through the real deploy-time pre-initializer, and time the first
//! call into the guest **before and after**, in the same process, under the
//! same engine. Rust guests cannot produce this number — they have no runtime
//! to pre-initialize — which is what makes it the most interesting result the
//! control plane can contribute.
//!
//! Runs in the sandbox like everything else that instantiates guest wasm.

use std::time::Instant;

use wisp_abi::config::Limits;
use wisp_control::preinit::{InitPal, PreInitRequest, PreInitializer};
use wisp_control::validator;

const DOTNET: &str = "../../guests/dotnet/out/wisp_guest_dotnet.wasm";

fn dotnet_guest() -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DOTNET);
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(_) => {
            panic!(
                "{} is missing.\n\n\
                 This artifact is COMMITTED, so its absence means a broken \n\
                 checkout rather than an unbuilt fixture — no legitimate \n\
                 configuration lacks it. Skipping here would trade a real \n\
                 assertion for a green test that asserted nothing, which is \n\
                 the trap docs/plan.md's fixture convention exists to close.\n\n\
                 Restore it with `git checkout -- guests/dotnet/out`, or \n\
                 rebuild with `scripts/dev guests/dotnet/build.sh`.",
                p.display()
            )
        }
    }
}

/// §4.3 against the real thing. `docs/dotnet-findings.md` §4 reported 32
/// undeclared `wasi:*@0.2.0` imports; spec v1.4 answers them with the
/// deploy-time PAL adapter (`crate::pal`, and see `tests/pal_dotnet.rs` for
/// the stock module going through it). *This* build needs none of that: the
/// guest-side shim in `guests/dotnet/native/wisp_pal.c` resolves wasi-libc's
/// imports inside the module, so it imports **only** `wisp` and the adapter
/// has nothing to compose. Both paths must work, and this test owns the
/// second one.
#[test]
fn the_dotnet_guest_satisfies_v1_with_no_pal_exception() {
    let Some(wasm) = dotnet_guest() else { return };
    let report = validator::validate(&wasm, Limits::default())
        .unwrap_or_else(|vs| panic!(".NET guest rejected:\n{}", render(&vs)));

    println!("wisp_guest_dotnet.wasm: {} bytes", wasm.len());
    println!("{}", report.manifest());
    println!(
        "  {} functions, memory {:?}..{:?} pages, {} custom-section bytes",
        report.function_count,
        report.memory_pages_initial,
        report.memory_pages_max,
        report.custom_section_bytes
    );
    assert!(
        report.has_initialize(),
        "the §4.4 pre-init entry point must be exported"
    );
    // The finding that matters: no `wasi:*` import survived into this build,
    // so invariant 4 holds for a real .NET guest with no widening of the
    // capability table.
    for c in &report.capabilities {
        assert!(wisp_abi::imports::lookup(c).is_some());
    }
}

/// The headline measurement.
#[test]
fn pre_initialization_deletes_dotnet_runtime_startup() {
    let Some(wasm) = dotnet_guest() else { return };
    validator::validate(&wasm, Limits::default()).expect(".NET guest must validate");

    let t0 = Instant::now();
    let out = PreInitializer::new()
        // A managed runtime comes up *outside* any init unit, so there is
        // nothing to defer when it reaches for the clock or for entropy —
        // see `InitPal`. Tenant static constructors are unaffected: inside an
        // init unit these capabilities still trap and still defer.
        .with_pal(InitPal::RuntimeBringUp)
        .run(
            &wasm,
            &PreInitRequest {
                module_hash: wisp_engine::ModuleHash::of(&wasm),
                init_blob: Vec::new(),
            },
        )
        .expect("pre-init must succeed on the .NET guest");
    let preinit_ms = t0.elapsed().as_secs_f64() * 1e3;

    println!(
        "pre-init: {:.1} ms, {} pass(es), deferred {:?} {:?}, {} refusals, seed read {}x",
        preinit_ms,
        out.passes,
        out.deferred_units,
        out.deferred_reasons,
        out.capability_refusals,
        out.seed_reads
    );
    println!(
        "runtime PAL reached during .NET bring-up: {:?}",
        out.pal_calls
    );
    println!(
        "module {} bytes -> template {} bytes ({:+.1}%)",
        wasm.len(),
        out.template_bytes,
        (out.template_bytes as f64 / wasm.len() as f64 - 1.0) * 100.0
    );

    // Same engine, same process, same measurement on both sides.
    let engine = engine();
    let cold = Cold::measure(&engine, &wasm, /*call_init=*/ true);
    let warm = Cold::measure(&engine, &out.template_wasm, /*call_init=*/ false);

    println!("\n                        stock module      template (§4.4)");
    println!(
        "  compile               {:>9.2} ms   {:>12.2} ms",
        cold.compile_ms, warm.compile_ms
    );
    println!(
        "  instantiate           {:>9.3} ms   {:>12.3} ms",
        cold.instantiate_ms, warm.instantiate_ms
    );
    println!(
        "  _initialize           {:>9.3} ms   {:>12}",
        cold.c_ctors_ms, "-- (pre-ran)"
    );
    println!(
        "  wisp_initialize       {:>9.3} ms   {:>12}",
        cold.managed_init_ms, "-- (pre-ran)"
    );
    println!(
        "  first call into guest {:>9.3} ms   {:>12.3} ms",
        cold.first_call_ms, warm.first_call_ms
    );
    println!("  ---");
    println!(
        "  ACTIVATION (instantiate -> first call returns)\n                        {:>9.3} ms   {:>12.3} ms   {:.1}x faster",
        cold.activation_ms(),
        warm.activation_ms(),
        cold.activation_ms() / warm.activation_ms().max(1e-6)
    );
    println!(
        "  linear memory         {:>9} KiB  {:>12} KiB",
        cold.memory_bytes / 1024,
        warm.memory_bytes / 1024
    );
    println!(
        "\n  G1 (p50 activation < 1 ms): stock {} / template {}",
        verdict(cold.activation_ms() < 1.0),
        verdict(warm.activation_ms() < 1.0)
    );

    // The claim under test. Anything less than a large factor means either
    // pre-init did not capture .NET startup or the guest's `wisp_initialize`
    // does not reach managed code — both are findings, so the assertion is
    // deliberately loose and the numbers above are the actual deliverable.
    assert!(
        warm.activation_ms() < cold.activation_ms(),
        "pre-init must not make activation slower: {:.3} ms vs {:.3} ms",
        warm.activation_ms(),
        cold.activation_ms()
    );
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "MET"
    } else {
        "MISSED"
    }
}

fn render(vs: &[wisp_control::Violation]) -> String {
    vs.iter()
        .map(|v| format!("  - {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------

struct Cold {
    compile_ms: f64,
    instantiate_ms: f64,
    c_ctors_ms: f64,
    managed_init_ms: f64,
    first_call_ms: f64,
    memory_bytes: usize,
}

impl Cold {
    /// What a cold activation costs: instantiate, run whatever initialization
    /// the artifact still needs, and make the first call into the guest.
    /// Compilation is excluded — §4.4 does it once at deploy time per µarch.
    fn activation_ms(&self) -> f64 {
        self.instantiate_ms + self.c_ctors_ms + self.managed_init_ms + self.first_call_ms
    }

    fn measure(engine: &wasmtime::Engine, wasm: &[u8], call_init: bool) -> Cold {
        let t = Instant::now();
        let module = wasmtime::Module::new(engine, wasm).expect("compile");
        let compile_ms = ms(t);

        let mut store = wasmtime::Store::new(engine, ());
        let linker = host_linker(engine);
        let t = Instant::now();
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate");
        let instantiate_ms = ms(t);

        let mut c_ctors_ms = 0.0;
        let mut managed_init_ms = 0.0;
        if call_init {
            // `_initialize` is the wasip1 reactor entry: C constructors only,
            // ~0.03 ms. The .NET runtime does not come up here.
            if let Ok(f) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
                let t = Instant::now();
                f.call(&mut store, ()).expect("_initialize");
                c_ctors_ms = ms(t);
            }
            // ...it comes up on the first call across an
            // `[UnmanagedCallersOnly]` boundary, which is what
            // `wisp_initialize` is for (§4.4, and dotnet-findings §6.2).
            if let Ok(f) = instance.get_typed_func::<(), ()>(&mut store, "wisp_initialize") {
                let t = Instant::now();
                f.call(&mut store, ()).expect("wisp_initialize");
                managed_init_ms = ms(t);
            }
        } else {
            // A template must NOT still export the init entry: it already ran,
            // and a second way to re-run static init inside a live instance
            // would let memory drift from the handle manifest.
            assert!(
                instance
                    .get_typed_func::<(), ()>(&mut store, "wisp_initialize")
                    .is_err(),
                "wisp_initialize must be stripped from the template"
            );
        }

        // The cheapest possible call into the guest, so what is being timed is
        // the *entry*, not the work.
        let alloc = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "wisp_alloc")
            .expect("wisp_alloc");
        let t = Instant::now();
        let p = alloc.call(&mut store, (1, 8)).expect("wisp_alloc call");
        let first_call_ms = ms(t);
        assert!(p >= 0);

        let memory_bytes = instance
            .get_memory(&mut store, "memory")
            .map(|m| m.data_size(&store))
            .unwrap_or(0);

        Cold {
            compile_ms,
            instantiate_ms,
            c_ctors_ms,
            managed_init_ms,
            first_call_ms,
            memory_bytes,
        }
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn engine() -> wasmtime::Engine {
    let mut cfg = wasmtime::Config::new();
    wisp_control::pipeline::engine_profile(&mut cfg);
    wasmtime::Engine::new(&cfg).expect("engine")
}

/// The runtime import vtable: every capability defined, init-only imports
/// bound to **traps**. `docs/spike-findings.md` flags that wizer does not
/// strip the import section, so a template still *declares* the init-only
/// imports; binding them to traps is both required for instantiation and a
/// tripwire — if a template could re-read the escrowed hash seed at runtime,
/// §8.6's secret-bearing classification would widen and this would say so.
fn host_linker(engine: &wasmtime::Engine) -> wasmtime::Linker<()> {
    use wisp_abi::imports::{self, Ty};
    let mut linker: wasmtime::Linker<()> = wasmtime::Linker::new(engine);
    for def in imports::IMPORTS {
        let name = def.name;
        let cvt = |t: &Ty| match t {
            Ty::I32 => wasmtime::ValType::I32,
            Ty::I64 => wasmtime::ValType::I64,
        };
        let ty = wasmtime::FuncType::new(
            engine,
            def.sig.params.iter().map(cvt),
            def.sig.results.iter().map(cvt),
        );
        linker
            .func_new(imports::HOST_MODULE, name, ty, move |_c, _args, rets| {
                if name.starts_with("init_") {
                    wasmtime::bail!(
                        "init-only import `{name}` called at RUNTIME — template is not sealed"
                    );
                }
                if name == "abort" {
                    wasmtime::bail!("guest aborted");
                }
                for (r, want) in rets
                    .iter_mut()
                    .zip(imports::lookup(name).map(|d| d.sig.results).unwrap_or(&[]))
                {
                    *r = match want {
                        Ty::I32 => wasmtime::Val::I32(0),
                        Ty::I64 => wasmtime::Val::I64(0),
                    };
                }
                Ok(())
            })
            .unwrap();
    }
    linker
}
