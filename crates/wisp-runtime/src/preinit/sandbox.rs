//! The initialization linker defines declared imports and applies the capability policy.
//! An undefined import would be answered with zeros by wizer's shim, and a
//! static constructor that read the clock would bake a zero instead of
//! deferring.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::imports::{self, ImportDef};
use wizer::wasmtime as wt;

use super::wasi::{install_pal_adapter, WasiImport};
use wisp_abi::config::InitSandbox;

use super::{func_type, is_no_unit};

pub(super) type LinkerFactory = Rc<dyn Fn(&wt::Engine) -> anyhow::Result<wizer::Linker>>;

pub(super) struct Sandbox {
    seed: [u8; 16],
    config: Vec<u8>,
    deferred: BTreeSet<u32>,
    /// Set by `init_unit_begin`, cleared when a unit is skipped.
    current_unit: Option<u32>,
    /// The unit most recently told to skip, cleared at the next
    /// `init_unit_begin`. Tells a guest that ran a unit it was told to skip
    /// apart from a trap in glue code between units.
    last_deferred: Option<u32>,
    /// `(unit in scope, capability reached for)`, recorded at the refusal;
    /// by the time the trap surfaces the stack is gone.
    pub(super) violation: Option<(Option<u32>, &'static str)>,
    pub(super) refusals: u32,
    pub(super) logs: Vec<String>,
    pub(super) seed_reads: u32,
    pal: InitSandbox,
    pub(super) pal_calls: BTreeMap<&'static str, u32>,
    entropy_draws: u64,
}

impl Sandbox {
    pub(super) fn new(config: Vec<u8>, deferred: BTreeSet<u32>, pal: InitSandbox) -> Self {
        Sandbox {
            seed: rand::random(),
            config,
            deferred,
            current_unit: None,
            last_deferred: None,
            violation: None,
            refusals: 0,
            logs: Vec::new(),
            seed_reads: 0,
            pal,
            pal_calls: BTreeMap::new(),
            entropy_draws: 0,
        }
    }

    /// Admits application entropy in every unit; other PAL access keeps its mode/scope policy.
    pub(super) fn serves(&self, name: &str) -> bool {
        name == "entropy_fill" || (self.current_unit.is_none() && super::serves(self.pal, name))
    }

    pub(super) fn count_pal_call(&mut self, name: &'static str) {
        *self.pal_calls.entry(name).or_default() += 1;
    }
}

pub(super) fn sealed_linker(state: Arc<Mutex<Sandbox>>, wasi: Vec<WasiImport>) -> LinkerFactory {
    Rc::new(move |engine: &wt::Engine| {
        let mut linker: wizer::Linker = wt::Linker::new(engine);
        install_pal_adapter(&mut linker, engine, &wasi, &state)?;
        for def in imports::IMPORTS {
            let st = state.clone();
            let ty = func_type(engine, def);
            let name = def.name;
            if def.init_visible || def.name == "entropy_fill" {
                install_init_visible(&mut linker, def, ty, st)?;
            } else {
                linker.func_new(
                    imports::HOST_MODULE,
                    name,
                    ty,
                    move |mut caller, args, rets| {
                        if st.lock().serves(name) {
                            return serve_pal(name, &st, &mut caller, args, rets);
                        }
                        Err(refuse_capability(&st, name))
                    },
                )?;
            }
        }
        Ok(linker)
    })
}

fn install_init_visible(
    linker: &mut wizer::Linker,
    def: &'static ImportDef,
    ty: wt::FuncType,
    st: Arc<Mutex<Sandbox>>,
) -> anyhow::Result<()> {
    let name = def.name;
    match name {
        "entropy_fill" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, rets| serve_pal(name, &st, &mut caller, args, rets),
            )?;
        }

        "init_hash_seed" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (ptr, len) = (arg_i32(args, 0)?, arg_i32(args, 1)?);
                    let mut s = st.lock();
                    s.seed_reads += 1;
                    let seed = s.seed;
                    drop(s);
                    if len as usize != seed.len() {
                        anyhow::bail!(
                            "init_hash_seed expects a {}-byte buffer, guest passed {len}",
                            seed.len()
                        );
                    }
                    write_guest(&mut caller, ptr, &seed)
                },
            )?;
        }
        "init_config_get" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, rets| {
                    let (ptr, cap) = (arg_i32(args, 0)?, arg_i32(args, 1)?);
                    let blob = st.lock().config.clone();
                    let n = (cap as usize).min(blob.len());
                    if n > 0 {
                        write_guest(&mut caller, ptr, &blob[..n])?;
                    }
                    rets[0] = wt::Val::I32(blob.len() as i32);
                    Ok(())
                },
            )?;
        }
        "init_unit_begin" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |_caller, args, _rets| {
                    let id = arg_i32(args, 0)? as u32;
                    let mut s = st.lock();
                    s.current_unit = if is_no_unit(id) { None } else { Some(id) };
                    s.last_deferred = None;
                    Ok(())
                },
            )?;
        }
        "init_unit_deferred" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |_caller, args, rets| {
                    let id = arg_i32(args, 0)? as u32;
                    let mut s = st.lock();
                    let skip = s.deferred.contains(&id);
                    if skip {
                        s.current_unit = None;
                        s.last_deferred = Some(id);
                    }
                    rets[0] = wt::Val::I32(skip as i32);
                    Ok(())
                },
            )?;
        }
        "log_emit" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (level, ptr, len) =
                        (arg_i32(args, 0)?, arg_i32(args, 1)?, arg_i32(args, 2)?);
                    let bytes = read_guest(&mut caller, ptr, len)?;
                    let mut s = st.lock();
                    if s.logs.len() < 1024 {
                        s.logs
                            .push(format!("[{level}] {}", String::from_utf8_lossy(&bytes)));
                    }
                    Ok(())
                },
            )?;
        }
        "abort" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (ptr, len) = (arg_i32(args, 0)?, arg_i32(args, 1)?);
                    let msg = read_guest(&mut caller, ptr, len)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_else(|_| "<unreadable>".into());
                    let unit = st.lock().current_unit;
                    anyhow::bail!(
                        "guest aborted during pre-init{}: {msg}",
                        unit.map(|u| format!(" (init unit {u})"))
                            .unwrap_or_default()
                    )
                },
            )?;
        }
        other => {
            linker.func_new(imports::HOST_MODULE, other, ty, move |_c, _a, _r| {
                anyhow::bail!(
                    "import `{other}` is marked init_visible in wisp-abi but the init sandbox \
                     has no implementation for it; refusing to fake one"
                )
            })?;
        }
    }
    Ok(())
}

/// Initialization clock and entropy operations.
fn serve_pal(
    name: &'static str,
    st: &Arc<Mutex<Sandbox>>,
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    args: &[wt::Val],
    rets: &mut [wt::Val],
) -> anyhow::Result<()> {
    st.lock().count_pal_call(name);
    match name {
        "clock_now_coarse" => {
            rets[0] = wt::Val::I64(0);
            Ok(())
        }
        "entropy_fill" => {
            let (ptr, len) = (arg_i32(args, 0)?, arg_i32(args, 1)?);
            anyhow::ensure!(
                (0..=(1 << 20)).contains(&len),
                "entropy_fill length {len} out of range"
            );
            let bytes = host_entropy(st, len as usize);
            write_guest(caller, ptr, &bytes)
        }
        _ => anyhow::bail!("`{name}` is listed in RUNTIME_PAL but has no implementation"),
    }
}

/// Draws bytes from the host randomness source.
pub(super) fn host_entropy(st: &Arc<Mutex<Sandbox>>, len: usize) -> Vec<u8> {
    st.lock().entropy_draws += 1;
    let mut out = vec![0u8; len];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut out);
    out
}

/// Record a refusal of the `wisp:platform` capability `name` and produce the
/// trap that defers the unit in scope.
pub(super) fn refuse_capability(st: &Arc<Mutex<Sandbox>>, name: &'static str) -> anyhow::Error {
    let mut s = st.lock();
    s.refusals += 1;
    let unit = s.current_unit;
    let told_to_skip = s.last_deferred;
    if s.violation.is_none() {
        s.violation = Some((unit.or(told_to_skip), name));
    }
    anyhow::anyhow!(
        "WISP init sandbox: capability `{name}` is sealed during pre-init; the init \
         unit that reached for it is deferred to lazy per-instance execution"
    )
}

pub(super) fn arg_i32(args: &[wt::Val], i: usize) -> anyhow::Result<i32> {
    args.get(i)
        .and_then(wt::Val::i32)
        .ok_or_else(|| anyhow::anyhow!("argument {i} is not an i32"))
}

fn guest_memory(caller: &mut wt::Caller<'_, wizer::StoreData>) -> anyhow::Result<wt::Memory> {
    caller
        .get_export(imports::exports::MEMORY)
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("guest exports no `memory`"))
}

pub(super) fn write_guest(
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    ptr: i32,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mem = guest_memory(caller)?;
    mem.write(&mut *caller, ptr as usize, bytes)?;
    Ok(())
}

fn read_guest(
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    ptr: i32,
    len: i32,
) -> anyhow::Result<Vec<u8>> {
    const MAX: i32 = 1 << 20;
    anyhow::ensure!((0..=MAX).contains(&len), "buffer length {len} out of range");
    let mem = guest_memory(caller)?;
    let mut buf = vec![0u8; len as usize];
    mem.read(&*caller, ptr as usize, &mut buf)?;
    Ok(buf)
}
