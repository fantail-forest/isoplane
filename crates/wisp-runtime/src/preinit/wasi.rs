//! The WASI PAL adapter inside the init sandbox: the same table and
//! dispositions the validator admitted and the engine composes, with the
//! sandbox's scope rule applied to the delegated entries, so a static
//! constructor that reads the clock through `wasi:clocks` defers exactly as
//! one that reads `clock_now_coarse` does.

use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::imports;
use wisp_abi::pal::{self, Disposition};
use wizer::wasmtime as wt;

use super::sandbox::{host_entropy, refuse_capability, write_guest, Sandbox};

/// A non-`wisp` function import as the module declares it.
pub(super) struct WasiImport {
    module: String,
    name: String,
    params: Vec<wasmparser::ValType>,
    results: Vec<wasmparser::ValType>,
}

pub(super) fn wasi_imports(wasm: &[u8]) -> Vec<WasiImport> {
    let mut types: Vec<(Vec<wasmparser::ValType>, Vec<wasmparser::ValType>)> = Vec::new();
    let mut out = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm).flatten() {
        match payload {
            wasmparser::Payload::TypeSection(r) => {
                for group in r.into_iter().flatten() {
                    for sub in group.into_types() {
                        match sub.composite_type.inner {
                            wasmparser::CompositeInnerType::Func(f) => {
                                types.push((f.params().to_vec(), f.results().to_vec()))
                            }
                            _ => types.push((Vec::new(), Vec::new())),
                        }
                    }
                }
            }
            wasmparser::Payload::ImportSection(r) => {
                for imp in r.into_imports().flatten() {
                    if imp.module == imports::HOST_MODULE {
                        continue;
                    }
                    let idx = match imp.ty {
                        wasmparser::TypeRef::Func(i) | wasmparser::TypeRef::FuncExact(i) => i,
                        _ => continue,
                    };
                    let Some((params, results)) = types.get(idx as usize).cloned() else {
                        continue;
                    };
                    out.push(WasiImport {
                        module: imp.module.to_string(),
                        name: imp.name.to_string(),
                        params,
                        results,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

pub(super) fn install_pal_adapter(
    linker: &mut wizer::Linker,
    engine: &wt::Engine,
    wasi: &[WasiImport],
    state: &Arc<Mutex<Sandbox>>,
) -> anyhow::Result<()> {
    for imp in wasi {
        let cvt = |t: &wasmparser::ValType| match t {
            wasmparser::ValType::I64 => wt::ValType::I64,
            wasmparser::ValType::F32 => wt::ValType::F32,
            wasmparser::ValType::F64 => wt::ValType::F64,
            wasmparser::ValType::V128 => wt::ValType::V128,
            _ => wt::ValType::I32,
        };
        let ty = wt::FuncType::new(
            engine,
            imp.params.iter().map(cvt),
            imp.results.iter().map(cvt),
        );
        let st = state.clone();
        let Some(def) = pal::lookup(&imp.module, &imp.name) else {
            // Unreachable behind the validator, and defined anyway so that
            // wizer's shim never answers it with zeros.
            let full = format!("{}::{}", imp.module, imp.name);
            linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, _r| {
                anyhow::bail!("`{full}` is outside the WASI PAL whitelist (V1)")
            })?;
            continue;
        };
        if let Some(fs) = wisp_abi::filesystem::IMPORTS
            .iter()
            .find(|fs| fs.pal.module == def.module && fs.pal.func == def.func)
        {
            linker.func_new(
                &imp.module,
                &imp.name,
                ty,
                move |mut caller, args, _results| {
                    let mut scalars = [0u64; wisp_abi::filesystem::MAX_ARGUMENTS];
                    anyhow::ensure!(
                        args.len() <= scalars.len(),
                        "filesystem argument count exceeds adapter capacity"
                    );
                    for (index, arg) in args.iter().enumerate() {
                        scalars[index] = match arg {
                            wt::Val::I32(value) => *value as u32 as u64,
                            wt::Val::I64(value) => *value as u64,
                            _ => anyhow::bail!("filesystem argument is not an integer"),
                        };
                    }
                    let files = st.lock().platform_files.clone();
                    wisp_engine::pal::filesystem::call(
                        &files,
                        &mut super::filesystem::CallerMemory(&mut caller),
                        fs,
                        &scalars[..args.len()],
                    )
                },
            )?;
            continue;
        }
        match (def.disposition, def.module, def.func) {
            (Disposition::Delegate(_), m, "now") if m == pal::MONOTONIC_CLOCK => {
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, rets| {
                    let mut s = st.lock();
                    if !s.serves("clock_now_coarse") {
                        drop(s);
                        return Err(refuse_capability(&st, "clock_now_coarse"));
                    }
                    s.count_pal_call("clock_now_coarse");
                    s.count_pal_call(pal::MONOTONIC_CLOCK);
                    rets[0] = wt::Val::I64(0);
                    Ok(())
                })?;
            }
            (Disposition::Delegate(_), m, "resolution") if m == pal::MONOTONIC_CLOCK => {
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, rets| {
                    rets[0] = wt::Val::I64(pal::CLOCK_QUANTUM_NS);
                    Ok(())
                })?;
            }
            (Disposition::Delegate(_), m, "get-environment" | "get-arguments")
                if m == pal::CLI_ENVIRONMENT =>
            {
                linker.func_new(&imp.module, &imp.name, ty, move |mut c, args, _r| {
                    st.lock().count_pal_call(pal::CLI_ENVIRONMENT);
                    let retptr = args
                        .last()
                        .and_then(wt::Val::i32)
                        .ok_or_else(|| anyhow::anyhow!("{} has no return area", def.func))?;
                    write_guest(&mut c, retptr, &[0u8; 8])
                })?;
            }
            (Disposition::Delegate(_), m, "get-random-bytes") if m == pal::RANDOM_RANDOM => {
                linker.func_new(&imp.module, &imp.name, ty, move |mut c, args, _r| {
                    let mut s = st.lock();
                    if !s.serves("entropy_fill") {
                        drop(s);
                        return Err(refuse_capability(&st, "entropy_fill"));
                    }
                    s.count_pal_call("entropy_fill");
                    s.count_pal_call(pal::RANDOM_RANDOM);
                    drop(s);
                    let len = match args.first().and_then(wt::Val::i64) {
                        Some(n) if n >= 0 => n as usize,
                        _ => 0,
                    };
                    anyhow::ensure!(
                        len <= (1 << 20),
                        "entropy draw of {len} bytes is out of range"
                    );
                    let retptr = args
                        .get(1)
                        .and_then(wt::Val::i32)
                        .ok_or_else(|| anyhow::anyhow!("get-random-bytes has no return area"))?;
                    let ptr = if len == 0 {
                        0
                    } else {
                        let realloc = c
                            .get_export("cabi_realloc")
                            .and_then(|e| e.into_func())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "guest declares `wasi:random` but exports no `cabi_realloc`"
                                )
                            })?
                            .typed::<(i32, i32, i32, i32), i32>(&c)?;
                        realloc.call(&mut c, (0, 0, 1, len as i32))?
                    };
                    let bytes = host_entropy(&st, len);
                    if len > 0 {
                        write_guest(&mut c, ptr, &bytes)?;
                    }
                    let mut area = [0u8; 8];
                    area[..4].copy_from_slice(&(ptr as u32).to_le_bytes());
                    area[4..].copy_from_slice(&(len as u32).to_le_bytes());
                    write_guest(&mut c, retptr, &area)
                })?;
            }
            _ => {
                let full = format!("{}::{}", def.module, def.func);
                let why = def.why;
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, _r| {
                    anyhow::bail!(
                        "guest called `{full}` during pre-init; the WISP PAL adapter satisfies \
                         it with a trap-on-call stub: {why}"
                    )
                })?;
            }
        }
    }
    Ok(())
}
