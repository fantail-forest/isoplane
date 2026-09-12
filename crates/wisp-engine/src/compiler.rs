//! Native compiler workers, separate from guest execution workers.

use std::sync::Arc;

use parking_lot::Mutex;

/// Native stack reserved for each Cranelift worker, including pre-initialisation.
/// This is compiler workspace; it does not enlarge a guest's execution stack.
pub const COMPILER_STACK_BYTES: usize = 8 * 1024 * 1024;

static POOL: Mutex<Option<Arc<rayon::ThreadPool>>> = Mutex::new(None);

#[derive(Debug, thiserror::Error)]
pub(crate) enum CompileError {
    #[error("creating native compiler workers: {0}")]
    Workers(#[source] rayon::ThreadPoolBuildError),
    #[error("compiling WebAssembly: {0}")]
    Module(#[source] wasmtime::Error),
}

pub(crate) fn compile(
    engine: &wasmtime::Engine,
    wasm: &[u8],
) -> Result<wasmtime::Module, CompileError> {
    // Compiling the .NET TLS acceptance module in parallel exhausted default
    // 2 MiB compiler stacks in debug builds; 8 MiB stacks completed that workload.
    // install also scopes Wasmtime's internal parallel iterator to this pool.
    let pool = workers(&POOL, || {
        rayon::ThreadPoolBuilder::new()
            .stack_size(COMPILER_STACK_BYTES)
            .thread_name(|index| format!("wisp-compiler-{index}"))
            .build()
    })
    .map_err(CompileError::Workers)?;
    pool.install(|| wasmtime::Module::new(engine, wasm))
        .map_err(CompileError::Module)
}

fn workers(
    slot: &Mutex<Option<Arc<rayon::ThreadPool>>>,
    build: impl FnOnce() -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>,
) -> Result<Arc<rayon::ThreadPool>, rayon::ThreadPoolBuildError> {
    let mut cached = slot.lock();
    if let Some(pool) = cached.as_ref() {
        return Ok(Arc::clone(pool));
    }
    let pool = Arc::new(build()?);
    *cached = Some(Arc::clone(&pool));
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_compiler_pool_does_not_need_another_worker_start() {
        let slot = Mutex::new(None);
        workers(&slot, || {
            rayon::ThreadPoolBuilder::new().num_threads(1).build()
        })
        .expect("the first compiler pool must start");
        let reused = workers(&slot, || {
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .spawn_handler(|_| Err(std::io::Error::other("no additional workers available")))
                .build()
        });
        assert!(
            reused.is_ok(),
            "an existing compiler pool must remain available without creating more workers"
        );
    }

    #[test]
    fn a_failed_compiler_worker_start_can_be_retried() {
        let slot = Mutex::new(None);
        let first = workers(&slot, || {
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .spawn_handler(|_| Err(std::io::Error::other("temporary worker start failure")))
                .build()
        });
        assert!(
            first.is_err(),
            "a failed worker start must report its error"
        );
        let retry = workers(&slot, || {
            rayon::ThreadPoolBuilder::new().num_threads(1).build()
        });
        assert!(
            retry.is_ok(),
            "a temporary worker start failure must not disable later compilation"
        );
        let pool = retry.expect("the successful retry must supply workers");
        let module = pool.install(|| {
            let wasm = wat::parse_str("(module)").expect("the empty test module is valid");
            wasmtime::Module::new(&wasmtime::Engine::default(), wasm)
        });
        assert!(
            module.is_ok(),
            "the recovered compiler pool must compile a module"
        );
    }
}
