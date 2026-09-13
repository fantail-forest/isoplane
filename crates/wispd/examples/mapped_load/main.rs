//! Measures mapped loads of portable WASM compiled by this invocation.
//! JSON input supplies the module path, optional engine configuration and iteration count.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
use wisp_engine::prepared::{PreparedIdentity, PreparedPayload, ReadAhead};
#[cfg(target_os = "linux")]
use serde::Deserialize;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    engine: wisp_engine::EngineConfig,
    module: std::path::PathBuf,
    iterations: usize,
}

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--worker") {
        return measure();
    }
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg("--worker")
        .stdin(std::process::Stdio::inherit())
        .status()?;
    anyhow::ensure!(status.success(), "mapped-load worker failed: {status}");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn measure() -> anyhow::Result<()> {
    anyhow::bail!("verified file-cache eviction measurement requires Linux")
}

#[cfg(target_os = "linux")]
fn measure() -> anyhow::Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    anyhow::ensure!(
        input.len() <= 1024 * 1024,
        "measurement input exceeds bound"
    );
    let input: Input = serde_json::from_slice(&input)?;
    anyhow::ensure!(input.iterations > 0, "iterations must be positive");
    let compiler = wisp_engine::WispEngine::new(input.engine.clone())?;
    let wasm = std::fs::read(&input.module)?;
    let input_hash = *blake3::hash(&wasm).as_bytes();
    // No initializer runs: the portable module contains the complete initial state measured here.
    let identity = PreparedIdentity {
        input_hash,
        runtime_profile: wisp_prepare::RuntimeProfile::raw_wasm().digest()?,
        initialization_hash: input_hash,
    };
    let product = compiler.prepare_template(&wasm, identity)?;
    drop(compiler);
    let engine = wisp_engine::WispEngine::new(input.engine)?;
    let storage = tempfile::tempdir()?;
    let directory = storage.path().join("prepared");
    product.publish(&directory)?;
    let files = [
        PreparedPayload::Native,
        PreparedPayload::Baseline,
        PreparedPayload::State,
    ]
    .map(|payload| {
        let path = directory.join(payload.file_name());
        std::fs::metadata(&path).map(|metadata| (path, payload, metadata.len()))
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let ahead = files
        .iter()
        .filter(|(_, _, bytes)| *bytes > 0)
        .map(|(_, payload, bytes)| ReadAhead {
            payload: *payload,
            offset_bytes: 0,
            length_bytes: (*bytes).min(1024 * 1024),
        })
        .collect::<Vec<_>>();
    for mode in ["warm", "evicted", "evicted-read-ahead"] {
        for iteration in 0..input.iterations {
            let mut resident_pages_before = 0;
            let mut payload_pages = 0;
            for (path, _, _) in &files {
                if mode == "warm" {
                    std::io::copy(&mut std::fs::File::open(path)?, &mut std::io::sink())?;
                } else {
                    linux::evict(path)?;
                }
                let (resident, pages) = linux::resident_pages(path)?;
                resident_pages_before += resident;
                payload_pages += pages;
            }
            anyhow::ensure!(
                mode == "warm" || resident_pages_before == 0,
                "eviction not established: {resident_pages_before} payload pages remain resident"
            );
            anyhow::ensure!(
                mode != "warm" || resident_pages_before == payload_pages,
                "warm cache not established: {resident_pages_before}/{payload_pages} payload pages resident"
            );
            let started = Instant::now();
            // SAFETY: the local preparation engine compiled the native payload above, and this run owns
            // its private directory. Cache eviction does not modify the published contents.
            // The supervising parent reports mapped-storage faults from this worker.
            let template = unsafe {
                engine.load_prepared_mapped(
                    &directory,
                    identity,
                    if mode == "evicted-read-ahead" {
                        &ahead
                    } else {
                        &[]
                    },
                )
            }?;
            let load_ns = started.elapsed().as_nanos();
            println!(
                "{}",
                serde_json::json!({
                    "mode":mode, "iteration":iteration, "load_ns":load_ns,
                    "resident_pages_before":resident_pages_before,
                    "payload_pages":payload_pages,
                    "payload_bytes":files.iter().map(|(_, _, bytes)| *bytes).sum::<u64>(),
                    "memory_pages":template.memory_pages(),
                    "read_ahead_bytes":if mode == "evicted-read-ahead" {ahead.iter().map(|r| r.length_bytes).sum::<u64>()} else {0},
                    "scope":"existing-engine load; no activation/request; read-ahead included",
                })
            );
            drop(template);
        }
    }
    Ok(())
}
