//! Local configuration inspection and WebAssembly compilation tools.

use clap::Parser;

use wispd::cli::Args;
use wispd::config::NodeConfig;

fn effective_config(args: &Args) -> anyhow::Result<NodeConfig> {
    let mut cfg = match &args.config {
        Some(p) => NodeConfig::from_toml(&std::fs::read_to_string(p)?)?,
        None => NodeConfig::dev_default(),
    };
    if let Some(d) = &args.data_dir {
        cfg.data_dir = d.clone();
    }
    Ok(cfg)
}

/// `--precompile`: build the artifact this node's cells will map, then exit.
///
/// The engine is built from the *node's* configuration, because a `.cwasm` is
/// only loadable by an engine whose fingerprint matches the one that produced
/// it — `(module hash, engine version, µarch)`. Compiling with a
/// differently configured engine writes a file every cell will refuse, which is
/// the check working rather than a bug, and is why this lives in `wispd` rather
/// than in a standalone tool.
#[cfg(feature = "real-backends")]
fn precompile(cfg: &NodeConfig, wasm_path: &std::path::Path) -> anyhow::Result<()> {
    let wasm = std::fs::read(wasm_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", wasm_path.display()))?;
    let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        max_instances: cfg.engine.max_instances,
        max_memory_bytes: cfg.engine.max_memory_mib << 20,
        guard_size_bytes: cfg.engine.guard_mib << 20,
        ..wisp_engine::EngineConfig::default()
    })?;
    let dir = wispd::real::RealPaths::under(&cfg.data_dir).artifacts;
    let t0 = std::time::Instant::now();
    let paths = engine.precompile_to_files(&wasm, &dir)?;
    println!(
        "{} -> {}  ({} B, {:.0} ms)",
        wasm_path.display(),
        paths.cwasm.display(),
        std::fs::metadata(&paths.cwasm)?.len(),
        t0.elapsed().as_secs_f64() * 1e3,
    );
    Ok(())
}

/// Report that this build has no compiler.
#[cfg(not(feature = "real-backends"))]
fn precompile(_cfg: &NodeConfig, _wasm_path: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!(
        "--precompile needs the wasm compiler, and this build has none: it was \
         built with `--no-default-features`, which drops `wisp-engine`. Rebuild \
         with the default features to precompile."
    )
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = effective_config(&args)?;

    if let Some(wasm) = args.precompile.clone() {
        return precompile(&cfg, &wasm);
    }
    if args.print_config {
        println!("{}", toml::to_string_pretty(&cfg)?);
        return Ok(());
    }
    if args.check {
        return match cfg.validate() {
            Ok(()) => {
                println!("ok: {} cells, {} apps", cfg.cells.len(), cfg.apps.len());
                Ok(())
            }
            Err(e) => {
                eprintln!("wispd: refusing this configuration: {e}");
                std::process::exit(1);
            }
        };
    }
    Ok(())
}
