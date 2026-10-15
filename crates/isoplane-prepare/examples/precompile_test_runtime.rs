//! Compiles the shared runtime, instrumented as preparation does, for the
//! engine tests load it under, and writes the instrumented module beside it.
//!
//! Usage: precompile_test_runtime <Isoplane.MonoRuntime.wasm> <directory>

fn main() -> anyhow::Result<()> {
    let [runtime, directory] = std::env::args_os()
        .skip(1)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| {
            anyhow::anyhow!("usage: precompile_test_runtime <runtime.wasm> <directory>")
        })?;
    let directory = std::path::PathBuf::from(directory);
    let wasm = isoplane_prepare::compiled_module(&std::fs::read(&runtime)?)?;
    let engine = isoplane_engine::IsoplaneEngine::new(isoplane_fixtures::engine_config(1))?;
    engine.precompile_to_files(&wasm, &directory)?;
    std::fs::write(directory.join("runtime.wasm"), wasm)?;
    Ok(())
}
