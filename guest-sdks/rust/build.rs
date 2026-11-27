//! Declares the host imports from `isoplane_abi::imports::IMPORTS`, as one
//! `isoplane_imports!` invocation that `abi.rs` includes.

use std::error::Error;
use std::fmt::Write;

use isoplane_abi::imports::{BufSpec, Ty, HOST_MODULE, IMPORTS};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    let mut text = format!("isoplane_imports! {{\n    module = {HOST_MODULE:?};\n");
    for import in IMPORTS {
        let buffer = match import.buf {
            BufSpec::None => None,
            BufSpec::In(index) | BufSpec::Out(index) => Some(usize::from(index)),
        };
        let parameters = import
            .sig
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| match buffer {
                Some(start) if index == start => "ptr: i32".to_owned(),
                Some(start) if index == start + 1 => "len: i32".to_owned(),
                _ => format!("a{index}: {}", scalar(*ty)),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let result = match import.sig.results {
            [] => String::new(),
            [ty] => format!(" -> {}", scalar(*ty)),
            _ => return Err(format!("{} returns more than one value", import.name).into()),
        };
        writeln!(
            text,
            "    #[doc = {:?}]\n    fn {}({parameters}){result};",
            import.why, import.name
        )?;
    }
    text.push_str("}\n");
    let output = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("imports.rs");
    std::fs::write(output, text)?;
    Ok(())
}

fn scalar(ty: Ty) -> &'static str {
    match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    }
}
