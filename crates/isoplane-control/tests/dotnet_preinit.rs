//! The shipped .NET guest through pre-initialization: the template answers
//! its first call with the managed runtime already up and no init entry left
//! to run.

use isoplane_abi::config::{GuestMemoryBytes, Limits};
use isoplane_control::validator;
use isoplane_engine::pal::{self, PalSource};
#[path = "common/managed.rs"]
mod managed;

#[test]
fn the_dotnet_template_activates_without_running_the_runtime_start_up() {
    let wasm = isoplane_fixtures::mono_runtime();
    validator::validate_with_memory_limit(
        &wasm,
        Limits::default(),
        Some(GuestMemoryBytes::WASM32_MAX),
    )
    .expect(".NET guest must validate");

    let out = managed::prepare("isoplane_guest_dotnet", Default::default())
        .expect("pre-init must succeed on the managed application");
    assert!(
        !out.initialized.pal_calls.is_empty(),
        "the runtime reaches the PAL while coming up; nothing reached means no runtime start-up \
         was captured"
    );

    let engine = engine();
    let module = wasmtime::Module::new(
        &engine,
        out.portable_template().expect("portable prepared output"),
    )
    .expect("the template compiles");
    let mut store = wasmtime::Store::new(&engine, Host);
    let mut linker = host_linker(&engine);
    // The runtime's readiness imports compose through the adapter, as in the
    // data plane.
    pal::link(&mut linker, &module).expect("the template's readiness imports compose");
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("the template instantiates");
    assert!(
        instance
            .get_typed_func::<(), i64>(&mut store, "isoplane_initialize")
            .is_err(),
        "isoplane_initialize must be stripped from the template"
    );
    let alloc = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "isoplane_alloc")
        .expect("isoplane_alloc");
    let p = alloc
        .call(&mut store, (1, 8))
        .expect("the first call into a template must not need any init entry to have run");
    assert!(p >= 0, "isoplane_alloc must return a guest pointer");
}

/// The adapter is composed over the template, never reached: a template
/// answers its first call without waiting on anything.
struct Host;
impl PalSource for Host {
    fn record_initialization_call(&mut self, _module: &'static str) {}

    fn slice_clock_ms(&mut self) -> u64 {
        0
    }
    fn entropy_fill(&mut self, _out: &mut [u8]) -> wasmtime::Result<()> {
        Ok(())
    }
}

fn engine() -> wasmtime::Engine {
    let mut cfg = wasmtime::Config::new();
    isoplane_control::aot::engine_profile(&mut cfg);
    wasmtime::Engine::new(&cfg).expect("engine")
}

/// Every capability defined, init-only imports bound to traps: a template
/// still declares them, and one that called them at runtime would be
/// re-reading the escrowed seed.
fn host_linker(engine: &wasmtime::Engine) -> wasmtime::Linker<Host> {
    use isoplane_abi::imports::{self, Ty};
    let mut linker: wasmtime::Linker<Host> = wasmtime::Linker::new(engine);
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
                        "init-only import `{name}` called at runtime: the template is not sealed"
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
            .expect("a host function links");
    }
    linker
}

#[test]
fn a_failed_managed_application_initializer_refuses_pre_initialization() {
    let result = managed::prepare("isoplane_guest_dotnet_failing_boot", Default::default());
    let Err(error) = result else {
        panic!("a failed application initializer must fail deployment");
    };
    assert_eq!(
        error.stage,
        isoplane_prepare::BuildStage::Initialization,
        "the deployment must fail while initializing the application"
    );
}
