pub(crate) mod filesystem;
pub(crate) mod native_io;
pub mod platform_files;

use isoplane_abi::imports::Ty;
use isoplane_abi::native_io::canonical::MAX_ARGUMENTS;
use isoplane_abi::pal::{PalDef, PAL_IMPORTS};

use crate::state::StoreState;

impl StoreState {
    /// Refuses, before its side effect, a native I/O call that initialization
    /// may not make, and counts the ones it may.
    fn check_native_capability(&mut self, import: &'static str) -> wasmtime::Result<()> {
        if self.is_init_sandbox() && !self.init_runtime_capability(import) {
            self.violation = Some(import);
            return Err(crate::state::CapabilityViolation {
                import,
                init_sandbox: true,
            }
            .into());
        }
        if self.is_init_sandbox() {
            *self.init_pal_calls.entry(import).or_default() += 1;
        }
        Ok(())
    }

    /// Performs nonblocking native I/O through the host bindings.
    fn native_io(
        &mut self,
        request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        self.bindings
            .as_mut()
            .ok_or(isoplane_abi::native_io::Error::Unsupported(
                "native I/O without host bindings",
            ))?
            .native_io(request)
    }
}

/// Defines every import in the table on `linker`.
/// Every adapter entry completes synchronously before control returns to the
/// guest. An entry that produces a result delivers it as its import's
/// canonical signature says: as the Wasm result, or written into guest memory
/// at the return pointer the guest passed.
/// During grace the bindings behind [`StoreState::native_io`] decide which
/// native I/O requests a cancelled dispatch may still make.
pub(crate) fn define_imports(linker: &mut wasmtime::Linker<StoreState>) -> anyhow::Result<()> {
    for def in PAL_IMPORTS {
        define(linker, def)?;
    }
    Ok(())
}

fn define(linker: &mut wasmtime::Linker<StoreState>, def: &'static PalDef) -> anyhow::Result<()> {
    let signature = def.sig();
    let value_type = |ty: &Ty| match ty {
        Ty::I32 => wasmtime::ValType::I32,
        Ty::I64 => wasmtime::ValType::I64,
    };
    let ty = wasmtime::FuncType::new(
        linker.engine(),
        signature.params.iter().map(value_type),
        signature.results.iter().map(value_type),
    );
    linker.func_new(
        def.module,
        def.func,
        ty,
        move |mut caller, args, results| {
            let mut scalars = [0u64; MAX_ARGUMENTS];
            for ((slot, value), param) in scalars.iter_mut().zip(args).zip(signature.params) {
                *slot = match param {
                    Ty::I32 => value.i32().expect(
                        "wasmtime type-checks each argument against `ty`, built from `param`",
                    ) as u32 as u64,
                    Ty::I64 => value.i64().expect(
                        "wasmtime type-checks each argument against `ty`, built from `param`",
                    ) as u64,
                };
            }
            let value = native_io::call(&mut caller, def.operation, &scalars[..args.len()])
                .map_err(wasmtime::Error::from_anyhow)?;
            if let Some(result) = results.first_mut() {
                *result = match signature.results[0] {
                    Ty::I32 => wasmtime::Val::I32(value as i32),
                    Ty::I64 => wasmtime::Val::I64(value as i64),
                };
            }
            Ok(())
        },
    )?;
    Ok(())
}
