//! Runs the C# guest through the platform's **own** deploy-time validator
//! (`isoplane_control::validate`) and prints the verdict verbatim.
//!
//! The point is not to describe what rule V1 would say about a module full of
//! `wasi:*` imports. It is to make the module meet the validator and print
//! whatever comes back, because a claim about our own validator that has not
//! been run against our own artifact is not evidence.
//!
//! Built only with `--features validator` so the default `check.sh` does not
//! drag the control plane (and wizer's second wasmtime) into the guest's
//! build graph.

use anyhow::{Context, Result};
use isoplane_abi::config::Limits;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "guests/dotnet/out/isoplane_guest_dotnet.wasm".to_string());
    let wasm = std::fs::read(&path).with_context(|| format!("reading {path}"))?;

    for limits in [
        Limits {
            module_bytes_compressed: 5242880,
            instance_memory_max: 67108864,
            cpu_ms_per_http_dispatch: 10,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6,
        },
        Limits::default(),
    ] {
        println!("=== isoplane_control::validate({path}, {limits:?}) ===");
        match isoplane_control::validate(&wasm, limits) {
            Ok(report) => {
                println!("ACCEPTED");
                println!("  capabilities       {:?}", report.capabilities);
                println!("  optional exports   {:?}", report.optional_exports);
                println!(
                    "  memory pages       initial {:?}, max {:?}",
                    report.memory_pages_initial, report.memory_pages_max
                );
                println!("  functions          {}", report.function_count);
                println!("  custom sections    {} bytes", report.custom_section_bytes);
            }
            Err(violations) => {
                println!("REJECTED — {} violation(s)", violations.len());
                for v in &violations {
                    println!("  {v}");
                }
            }
        }
        println!();
    }
    Ok(())
}
