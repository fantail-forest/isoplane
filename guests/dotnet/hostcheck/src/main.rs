//! Checks the C# HTTP fixture, its completed-state memory copy and response ABI.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use wasmtime::{
    Caller, Config, Engine, Extern, ExternType, Global, Instance, Linker, Module, Store, Val,
    ValType,
};

use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, ResponseHead, TraceContext};
use wisp_abi::status::PollStatus;

/// Everything the fake host observed, so the assertions can be about
/// behaviour rather than about return codes alone.
#[derive(Default)]
struct HostLog {
    logs: Vec<String>,
    responses: Vec<ResponseHead>,
    /// Names of imports outside the `wisp` module that the guest actually
    /// invoked. §5.3/rule V1 cares about the declared set; this measures the
    /// *reachable* set, which is the number that decides whether the residual
    /// wasi imports are a real capability leak or dead weight.
    non_wisp_calls: Vec<String>,
}

type Shared = Arc<Mutex<HostLog>>;
fn initialize_sample(store: &mut Store<()>, instance: &Instance) -> Result<()> {
    use wisp_abi::imports::{exports, Ty};
    if let Some(function) = instance.get_func(&mut *store, exports::INITIALIZE) {
        let actual = function.ty(&*store);
        let expected = exports::SIG_INITIALIZE;
        let types_match = actual.results().zip(expected.results.iter()).all(
            |(actual, expected)| match expected {
                Ty::I32 => matches!(actual, ValType::I32),
                Ty::I64 => matches!(actual, ValType::I64),
            },
        );
        if !expected.params.is_empty()
            || actual.params().len() != 0
            || actual.results().len() != expected.results.len()
            || !types_match
        {
            bail!("initializer signature differs from the current host contract");
        }
        let mut results = expected
            .results
            .iter()
            .map(|value| match value {
                Ty::I32 => Val::I32(0),
                Ty::I64 => Val::I64(0),
            })
            .collect::<Vec<_>>();
        function.call(&mut *store, &[], &mut results)?;
    } else {
        instance
            .get_typed_func::<(i32, i32), i32>(&mut *store, exports::ALLOC)?
            .call(&mut *store, (1, 8))?;
    }
    Ok(())
}

fn require_done(status: PollStatus) -> Result<u32> {
    match status {
        PollStatus::Done { response } if response != 0 => Ok(response),
        other => bail!("the HTTP fixture returned {other:?}, expected a completed response"),
    }
}

fn response_count(log: &Shared) -> Result<usize> {
    Ok(log
        .lock()
        .map_err(|_| anyhow::anyhow!("host observation lock poisoned"))?
        .responses
        .len())
}

enum ExpectedResponse<'a> {
    Echo(&'a [u8]),
    Compatibility,
}

fn check_response(
    log: &Shared,
    index: usize,
    handle: u32,
    expected: ExpectedResponse<'_>,
) -> Result<()> {
    let observations = log
        .lock()
        .map_err(|_| anyhow::anyhow!("host observation lock poisoned"))?;
    let head = observations
        .responses
        .get(index)
        .context("guest returned no response")?;
    if usize::try_from(handle).ok() != index.checked_add(1) {
        bail!("the returned response handle does not name the emitted response");
    }
    match expected {
        ExpectedResponse::Echo(body) => {
            if head.status != 200 || head.body != body {
                bail!(
                    "unexpected HTTP echo response {} {:?}",
                    head.status,
                    head.body
                );
            }
        }
        ExpectedResponse::Compatibility => {
            if head.status != 404 || head.body.is_empty() {
                bail!(
                    "unexpected compatibility response {} {:?}",
                    head.status,
                    head.body
                );
            }
        }
    }
    Ok(())
}

/// Resident-set size of this process, in KB, from /proc/self/status.
fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Measure process RSS while activating the requested number of sample instances.
fn density(engine: &Engine, module: &Module, linker: &Linker<()>, n: usize) -> Result<()> {
    let base = rss_kb();
    let mut stores = Vec::with_capacity(n);
    let t = Instant::now();
    for _ in 0..n {
        let mut store = Store::new(engine, ());
        let instance = linker.instantiate(&mut store, module)?;
        instance
            .get_typed_func::<(), ()>(&mut store, "_initialize")?
            .call(&mut store, ())?;
        // Force the runtime up, which is what grows the heap.
        instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "wisp_alloc")?
            .call(&mut store, (1, 8))?;
        stores.push((store, instance));
    }
    let elapsed = t.elapsed().as_secs_f64() * 1e3;
    let after = rss_kb();
    println!();
    println!("  ACTIVATION RSS");
    println!("    instances            {n}");
    println!(
        "    activation total     {elapsed:.0} ms ({:.2} ms each)",
        elapsed / n as f64
    );
    println!("    host RSS             {base} KB -> {after} KB");
    println!(
        "    marginal per instance {:.0} KB",
        (after.saturating_sub(base)) as f64 / n as f64
    );
    drop(stores);
    Ok(())
}

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "guests/dotnet/out/wisp_guest_dotnet.wasm".to_string());
    let bytes = std::fs::read(&path).with_context(|| format!("reading {path}"))?;

    let config = Config::new();
    let engine = Engine::new(&config)?;

    let t_compile = Instant::now();
    let module = Module::new(&engine, &bytes)?;
    let compile_ms = t_compile.elapsed().as_secs_f64() * 1e3;

    check_abi_surface(&module)?;

    let log: Shared = Arc::new(Mutex::new(HostLog::default()));
    let linker = build_linker(&engine, &module, log.clone())?;

    let mut store = Store::new(&engine, ());
    let t_inst = Instant::now();
    let instance = linker.instantiate(&mut store, &module)?;
    let instantiate_ms = t_inst.elapsed().as_secs_f64() * 1e3;

    let init = instance.get_typed_func::<(), ()>(&mut store, "_initialize")?;
    let t_init = Instant::now();
    init.call(&mut store, ())?;
    let initialize_ms = t_init.elapsed().as_secs_f64() * 1e3;

    let t_first_managed = Instant::now();
    initialize_sample(&mut store, &instance)?;
    let first_managed_call_ms = t_first_managed.elapsed().as_secs_f64() * 1e3;

    let mem_after_init = instance
        .get_memory(&mut store, "memory")
        .context("no memory export")?
        .data_size(&store);

    let sp = instance
        .get_global(&mut store, "__stack_pointer")
        .context("guest must export __stack_pointer for the invariant-3 check")?;
    let sp_at_rest = read_sp(&mut store, &sp)?;

    let dispatch = DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/quote".into(),
            path: "/quote".into(),
            query: vec![],
            headers: vec![("accept".into(), "text/plain".into())],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0xC0FFEE,
        cpu_budget_ms: 50,
        now_ms: 1_724_000_000_000,
    };
    let encoded = wisp_abi::encode(&dispatch)?;
    let compatibility = std::env::args().any(|argument| argument == "--compat");
    let first_response = response_count(&log)?;
    let t_first = Instant::now();
    let response = require_done(call_handle(&mut store, &instance, &encoded)?)?;
    let first_dispatch_ms = t_first.elapsed().as_secs_f64() * 1000.0;
    check_response(
        &log,
        first_response,
        response,
        if compatibility {
            ExpectedResponse::Compatibility
        } else {
            ExpectedResponse::Echo(b"/quote")
        },
    )?;
    if read_sp(&mut store, &sp)? != sp_at_rest {
        bail!("the sample shadow stack pointer changed after completion");
    }

    let memory = instance
        .get_memory(&mut store, "memory")
        .context("no memory export")?;
    let image = memory.data(&store).to_vec();
    drop(store);
    let mut store2 = Store::new(&engine, ());
    let restored = linker.instantiate(&mut store2, &module)?;
    let memory2 = restored
        .get_memory(&mut store2, "memory")
        .context("no restored memory")?;
    if image.len() > memory2.data_size(&store2) {
        let pages = (image.len() - memory2.data_size(&store2)).div_ceil(64 * 1024) as u64;
        memory2.grow(&mut store2, pages)?;
    }
    memory2.write(&mut store2, 0, &image)?;
    let sp2 = restored
        .get_global(&mut store2, "__stack_pointer")
        .context("no restored stack pointer")?;
    sp2.set(&mut store2, Val::I32(sp_at_rest))?;

    let dispatch2 = DispatchRecord {
        dispatch_id: 2,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/quote2".into(),
            path: "/quote2".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        ..dispatch.clone()
    };
    let next_response = response_count(&log)?;
    let response = require_done(call_handle(
        &mut store2,
        &restored,
        &wisp_abi::encode(&dispatch2)?,
    )?)?;
    check_response(
        &log,
        next_response,
        response,
        if compatibility {
            ExpectedResponse::Compatibility
        } else {
            ExpectedResponse::Echo(b"/quote2")
        },
    )?;
    let reused_response = response_count(&log)?;
    let response = require_done(call_handle(&mut store2, &restored, &encoded)?)?;
    check_response(
        &log,
        reused_response,
        response,
        if compatibility {
            ExpectedResponse::Compatibility
        } else {
            ExpectedResponse::Echo(b"/quote")
        },
    )?;
    if read_sp(&mut store2, &sp2)? != sp_at_rest {
        bail!("the restored sample shadow stack pointer changed after completion");
    }
    println!("PASS HTTP response, completed-state memory round trip and instance reuse");
    println!("module: {path} ({} bytes)", bytes.len());
    println!("compile: {compile_ms:.2} ms; instantiate: {instantiate_ms:.2} ms");
    println!(
        "initialize: {initialize_ms:.2} ms; first managed call: {first_managed_call_ms:.2} ms"
    );
    println!(
        "HTTP dispatch: {first_dispatch_ms:.2} ms; initialized memory: {mem_after_init} bytes"
    );
    if let Some(n) = std::env::args()
        .skip_while(|a| a != "--density")
        .nth(1)
        .and_then(|v| v.parse::<usize>().ok())
    {
        density(&engine, &module, &linker, n)?;
    }
    Ok(())
}

/// §5.2 plus `wisp-abi::imports::exports::REQUIRED`.
fn check_abi_surface(module: &Module) -> Result<()> {
    let exports: Vec<&str> = module.exports().map(|e| e.name()).collect();
    for required in wisp_abi::imports::exports::REQUIRED {
        if !exports.contains(required) {
            bail!("module is missing required export `{required}` (§5.2)");
        }
    }

    // §4.3 V3: exactly one linear memory.
    let memories = module
        .exports()
        .filter(|e| matches!(e.ty(), wasmtime::ExternType::Memory(_)))
        .count();
    if memories != 1 {
        bail!("module exports {memories} memories; V3 permits exactly one");
    }

    Ok(())
}

fn build_linker(engine: &Engine, module: &Module, log: Shared) -> Result<Linker<()>> {
    let mut linker: Linker<()> = Linker::new(engine);

    // ---- the declared capability surface ---------------------------------
    let l = log.clone();
    linker.func_wrap(
        "wisp",
        "log_emit",
        move |mut caller: Caller<'_, ()>, level: i32, ptr: i32, len: i32| {
            let s = read_guest_string(&mut caller, ptr, len);
            l.lock().unwrap().logs.push(format!("[{level}] {s}"));
        },
    )?;

    let l = log.clone();
    linker.func_wrap(
        "wisp",
        "resp_new",
        move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| -> i32 {
            let bytes = read_guest_bytes(&mut caller, ptr, len);
            match postcard::from_bytes::<ResponseHead>(&bytes) {
                Ok(head) => {
                    let mut g = l.lock().unwrap();
                    g.responses.push(head);
                    g.responses.len() as i32
                }
                Err(_) => 0,
            }
        },
    )?;

    linker.func_wrap("wisp", "resp_finish", move |_handle: i32| {})?;

    // ---- everything else -------------------------------------------------
    // NativeAOT-LLVM leaves ~32 residual `wasi:*` imports in the module. Rule
    // V1 would reject all of them, so the interesting question is not whether
    // they are DECLARED but whether they are REACHABLE. Rather than trapping
    // (which only tells us the first one) each gets a recording stub, so one
    // run enumerates the whole set the host would have to stub for real.
    for import in module.imports() {
        if import.module() == wisp_abi::imports::HOST_MODULE {
            continue;
        }
        let ExternType::Func(ty) = import.ty() else {
            bail!(
                "guest imports a non-function `{}::{}`",
                import.module(),
                import.name()
            );
        };
        let full = format!("{}::{}", import.module(), import.name());
        let fixup = RETURN_AREA_FIXUPS
            .iter()
            .find(|(name, _)| *name == full)
            .map(|(_, bytes)| *bytes);
        let l = log.clone();
        // Precomputed outside the closure: a wasm import stub cannot report an
        // anyhow error, and validating the shape once is cheaper anyway.
        let zeros: Vec<Val> = ty
            .results()
            .map(|t| match t {
                ValType::I32 => Ok(Val::I32(0)),
                ValType::I64 => Ok(Val::I64(0)),
                ValType::F32 => Ok(Val::F32(0)),
                ValType::F64 => Ok(Val::F64(0)),
                other => bail!("guest import has unsupported result type {other:?}"),
            })
            .collect::<Result<_>>()?;
        linker.func_new(
            import.module(),
            import.name(),
            ty.clone(),
            move |mut caller, params, results| {
                l.lock().unwrap().non_wisp_calls.push(full.clone());

                // The canonical-ABI lowering of a function with no wasm result
                // writes its return value into a caller-supplied area. Leaving
                // that area as whatever garbage was on the shadow stack would
                // make the guest read a fabricated list; zeroing it is the
                // smallest honest answer ("empty").
                if let (Some(n), Some(Val::I32(retptr))) = (fixup, params.last()) {
                    if let Some(Extern::Memory(mem)) = caller.get_export("memory") {
                        let zeros = vec![0u8; n];
                        let _ = mem.write(&mut caller, *retptr as usize, &zeros);
                    }
                }

                results.clone_from_slice(&zeros);
                Ok(())
            },
        )?;
    }

    Ok(linker)
}

/// Imports whose canonical-ABI return area must be zeroed for the stub to be
/// a coherent "empty" answer rather than whatever was on the shadow stack.
/// Discovered by running the guest, not guessed: the list is exactly the set
/// `hostcheck` observed the .NET startup path reach.
const RETURN_AREA_FIXUPS: &[(&str, usize)] = &[
    // list<tuple<string, string>> -> (ptr, len)
    ("wasi:cli/environment@0.2.0::get-environment", 8),
    // list<string> -> (ptr, len)
    ("wasi:cli/environment@0.2.0::get-arguments", 8),
    // list<tuple<descriptor, string>> -> (ptr, len)
    ("wasi:filesystem/preopens@0.2.0::get-directories", 8),
];

fn read_sp(store: &mut Store<()>, sp: &Global) -> Result<i32> {
    match sp.get(store) {
        Val::I32(v) => Ok(v),
        other => bail!("__stack_pointer is {other:?}, expected i32"),
    }
}

fn read_guest_bytes(caller: &mut Caller<'_, ()>, ptr: i32, len: i32) -> Vec<u8> {
    let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
        return Vec::new();
    };
    let data = mem.data(&caller);
    let (start, end) = (ptr as usize, ptr as usize + len as usize);
    if end > data.len() {
        return Vec::new();
    }
    data[start..end].to_vec()
}

fn read_guest_string(caller: &mut Caller<'_, ()>, ptr: i32, len: i32) -> String {
    String::from_utf8_lossy(&read_guest_bytes(caller, ptr, len)).into_owned()
}

/// §5.5: host→guest data enters via `wisp_alloc` + copy.
fn handoff(store: &mut Store<()>, instance: &Instance, bytes: &[u8]) -> Result<i32> {
    let alloc = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "wisp_alloc")?;
    let ptr = alloc.call(&mut *store, (bytes.len() as i32, 8))?;
    if ptr == 0 {
        bail!("wisp_alloc returned 0 for {} bytes", bytes.len());
    }
    let mem = instance
        .get_memory(&mut *store, "memory")
        .context("no memory export")?;
    mem.write(&mut *store, ptr as usize, bytes)?;
    Ok(ptr)
}

fn call_handle(store: &mut Store<()>, instance: &Instance, bytes: &[u8]) -> Result<PollStatus> {
    let ptr = handoff(store, instance, bytes)?;
    let f = instance.get_typed_func::<(i32, i32), i64>(&mut *store, "wisp_handle")?;
    let raw = f.call(&mut *store, (ptr, bytes.len() as i32))?;
    Ok(PollStatus::decode(raw as u64))
}
