#![forbid(unsafe_code)]

use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/BuildProtocol/WasmMemory.props");
    println!("cargo::rerun-if-changed={}", source.display());
    let text = std::fs::read_to_string(&source)?;
    let (page_bytes, maximum_bytes) =
        memory_limits(&text).map_err(|error| format!("{}: {error}", source.display()))?;
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").ok_or("Cargo did not supply OUT_DIR")?,
    );
    std::fs::write(output.join("wasm_page_bytes.rs"), page_bytes.to_string())?;
    std::fs::write(
        output.join("wasm32_memory_max_bytes.rs"),
        maximum_bytes.to_string(),
    )?;
    Ok(())
}

fn memory_limits(text: &str) -> Result<(u64, u64), Box<dyn Error>> {
    let document = roxmltree::Document::parse(text)?;
    let project = document.root_element();
    if !project.has_tag_name("Project")
        || project.attributes().len() != 1
        || project.attribute("TreatAsLocalProperty") != Some("WasmPageBytes;Wasm32MemoryMaxBytes")
    {
        return Err("expected a Project with fixed local memory properties".into());
    }
    let mut groups = project.children().filter(roxmltree::Node::is_element);
    let group = groups.next().ok_or("missing PropertyGroup")?;
    if groups.next().is_some()
        || !group.has_tag_name("PropertyGroup")
        || group.attributes().len() != 0
    {
        return Err("expected one unconditional PropertyGroup".into());
    }
    if group.children().filter(roxmltree::Node::is_element).count() != 2 {
        return Err("expected only WasmPageBytes and Wasm32MemoryMaxBytes".into());
    }
    let page_bytes = literal(group, "WasmPageBytes")?;
    let maximum_bytes = literal(group, "Wasm32MemoryMaxBytes")?;
    if page_bytes == 0 || maximum_bytes == 0 || !maximum_bytes.is_multiple_of(page_bytes) {
        return Err("memory sizes must be positive and the maximum must be page-aligned".into());
    }
    Ok((page_bytes, maximum_bytes))
}

fn literal(group: roxmltree::Node<'_, '_>, name: &str) -> Result<u64, Box<dyn Error>> {
    let mut properties = group.children().filter(|node| node.has_tag_name(name));
    let property = properties.next().ok_or_else(|| format!("missing {name}"))?;
    if properties.next().is_some()
        || property.attributes().len() != 0
        || property.children().any(|node| !node.is_text())
    {
        return Err(format!("{name} must be one unconditional literal integer").into());
    }
    property
        .text()
        .ok_or_else(|| format!("{name} has no value"))?
        .trim()
        .parse()
        .map_err(|error| format!("{name} must be a literal integer: {error}").into())
}
