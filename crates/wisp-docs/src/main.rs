//! ```text
//! cargo run -p wisp-docs            # rewrite the generated regions
//! cargo run -p wisp-docs -- --check # fail if they are stale, change nothing
//! ```
//!
//! `--check` is what `cargo test -p wisp-docs` runs; it is here as well so the
//! same answer is available without a test harness.

fn main() -> anyhow::Result<()> {
    let check = std::env::args().any(|a| a == "--check");
    if check {
        wisp_docs::check()?;
        println!("reference documentation is up to date with the code");
        return Ok(());
    }
    let changed = wisp_docs::write()?;
    if changed.is_empty() {
        println!("reference documentation is up to date with the code");
    } else {
        for page in &changed {
            println!("regenerated docs/userdocs/docs/reference/{page}");
        }
    }
    Ok(())
}
