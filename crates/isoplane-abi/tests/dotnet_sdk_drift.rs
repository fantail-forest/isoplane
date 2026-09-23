//! The C# SDK's hand-written mirrors of this crate, held to it: every extern
//! in `Abi/Imports.cs` against `IMPORTS`, and every enum in `Abi/Records.cs`
//! and `Abi/PollStatus.cs` against the discriminants postcard writes for the
//! Rust type. Runs on the stable toolchain with no .NET installed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use isoplane_abi::dispatch::{DispatchKind, HttpRequest, };
use isoplane_abi::imports::{self, Ty};
use isoplane_abi::result::{FutureError, };
use isoplane_abi::status::TrapCode;

fn sdk_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../guest-sdks/dotnet/Abi")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// `(entry point, params, results)` for every `[DllImport("isoplane", ...)]`
/// extern in `Imports.cs`.
fn csharp_externs(src: &str) -> Vec<(String, Vec<Ty>, Vec<Ty>)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line
            .trim()
            .strip_prefix("[DllImport(\"isoplane\", EntryPoint = \"")
        else {
            continue;
        };
        let name = rest
            .split('"')
            .next()
            .expect("an entry point name")
            .to_string();
        let decl = lines[i + 1..]
            .iter()
            .find(|l| l.contains("extern"))
            .unwrap_or_else(|| panic!("no extern declaration follows the import of `{name}`"))
            .trim();
        let after = decl
            .split("extern ")
            .nth(1)
            .unwrap_or_else(|| panic!("`{name}`: not an extern declaration: {decl}"));
        let (ret, sig) = after
            .split_once(' ')
            .expect("a return type before the name");
        let params_src = sig
            .split_once('(')
            .and_then(|(_, p)| p.split_once(')'))
            .map(|(p, _)| p)
            .unwrap_or_else(|| panic!("`{name}`: no parameter list: {decl}"));
        let params = params_src
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| {
                ty(
                    p.split_whitespace().next().expect("a parameter type"),
                    &name,
                )
            })
            .collect();
        let results = match ret {
            "void" => Vec::new(),
            other => vec![ty(other, &name)],
        };
        out.push((name, params, results));
    }
    out
}

fn ty(csharp: &str, name: &str) -> Ty {
    match csharp {
        "byte*" | "int" | "uint" => Ty::I32,
        "long" | "ulong" => Ty::I64,
        other => panic!("`{name}` uses a C# type with no core-wasm lowering here: {other}"),
    }
}

#[test]
fn every_extern_in_imports_cs_is_an_import_with_the_same_signature() {
    let externs = csharp_externs(&sdk_file("Imports.cs"));
    assert!(
        !externs.is_empty(),
        "Imports.cs declares no externs, so nothing was checked"
    );
    for (name, params, results) in externs {
        let def = imports::lookup(&name)
            .unwrap_or_else(|| panic!("Imports.cs declares `{name}`, which is not in IMPORTS"));
        assert_eq!(
            (params.as_slice(), results.as_slice()),
            (def.sig.params, def.sig.results),
            "`{name}`: the C# signature differs from the table's"
        );
    }
}

/// `(name, value)` pairs of a `public enum NAME : uint { ... }` block.
fn csharp_enum(src: &str, name: &str) -> BTreeMap<String, u32> {
    let start = src
        .find(&format!("enum {name} : uint"))
        .unwrap_or_else(|| panic!("no `enum {name} : uint` in the SDK"));
    let body = &src[start..];
    let body = &body[body.find('{').expect("an enum body") + 1..];
    let body = &body[..body.find('}').expect("the end of the enum body")];
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("///"))
        .map(|l| {
            let (n, v) = l
                .trim_end_matches(',')
                .split_once('=')
                .unwrap_or_else(|| panic!("`{name}`: a member without an explicit value: {l}"));
            (
                n.trim().to_string(),
                v.trim()
                    .parse()
                    .unwrap_or_else(|e| panic!("`{name}`: {l}: {e}")),
            )
        })
        .collect()
}

/// The discriminant postcard writes first for a value of an enum type.
fn discriminant<T: serde::Serialize>(v: &T) -> u32 {
    let bytes = isoplane_abi::encode(v).expect("encodes");
    let mut value = 0u32;
    let mut shift = 0;
    for b in bytes {
        value |= u32::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
    panic!("an unterminated varint")
}

fn assert_mirror<T: serde::Serialize>(csharp_file: &str, name: &str, rust: &[(&str, T)]) {
    let cs = csharp_enum(&sdk_file(csharp_file), name);
    let rust: BTreeMap<String, u32> = rust
        .iter()
        .map(|(n, v)| (n.to_string(), discriminant(v)))
        .collect();
    assert_eq!(
        cs, rust,
        "C# `{name}` in {csharp_file} does not mirror the Rust enum's variants and discriminants"
    );
}

#[test]
fn dispatch_kind_in_records_cs_mirrors_the_rust_enum() {
    let state = Some(vec![1u8]);
    assert_mirror(
        "Records.cs",
        "DispatchKind",
        &[
            (
                "Http",
                DispatchKind::Http(HttpRequest {
                    method: "GET".into(),
                    url: String::new(),
                    path: "/".into(),
                    query: vec![],
                    headers: vec![],
                    body: vec![],
                }),
            ),
        ],
    );
}


#[test]
fn future_error_in_records_cs_mirrors_the_rust_enum() {
    assert_mirror(
        "Records.cs",
        "FutureError",
        &[
            ("Cancelled", FutureError::Cancelled),
            ("Denied", FutureError::Denied(String::new())),
            ("NotFound", FutureError::NotFound),
            ("Io", FutureError::Io(String::new())),
        ],
    );
}

#[test]
fn trap_code_in_poll_status_cs_mirrors_the_rust_enum() {
    let cs = csharp_enum(&sdk_file("PollStatus.cs"), "TrapCode");
    let rust: BTreeMap<String, u32> = [
        ("Unknown", TrapCode::Unknown),
        ("Panic", TrapCode::Panic),
        ("OutOfMemory", TrapCode::OutOfMemory),
        ("CapabilityViolation", TrapCode::CapabilityViolation),
        ("Protocol", TrapCode::Protocol),
        ("TooManyFutures", TrapCode::TooManyFutures),
    ]
    .into_iter()
    .map(|(n, v)| (n.to_string(), v as u32))
    .collect();
    assert_eq!(
        cs, rust,
        "C# `TrapCode` in PollStatus.cs does not mirror the Rust enum's values"
    );
}

#[test]
fn generated_aspnet_entrypoints_match_the_host_export_contract() {
    let adapter = sdk_file("../AspNetCore/buildTransitive/Isoplane.AspNetCore.targets");
    assert!(
        adapter.contains("Isoplane.Exports.targets"),
        "the ASP.NET adapter must consume the shared export generator"
    );
    let source = sdk_file("../Build/Isoplane.Exports.targets");
    let mut found = BTreeMap::new();
    // The attribute is conditional on the runtime profile, so the export it
    // names is the next declaration after it, not on its own line.
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if !line.contains("[UnmanagedCallersOnly(") {
            continue;
        }
        let name = line
            .split("EntryPoint=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("an export name");
        let declaration = lines[index + 1..]
            .iter()
            .find_map(|candidate| candidate.split("public static ").nth(1))
            .expect("a static export after its attribute");
        let (result, method) = declaration.split_once(' ').expect("an export return type");
        let parameters = method
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .expect("export parameters")
            .0;
        let params: Vec<_> = parameters
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                ty(
                    value.split_whitespace().next().expect("a parameter type"),
                    name,
                )
            })
            .collect();
        let results = if result == "void" {
            vec![]
        } else {
            vec![ty(result, name)]
        };
        found.insert(name, (params, results));
    }
    // The native PAL supplies its allocator exports, independently of the managed router.
    let managed_signatures: Vec<_> = isoplane_abi::exports::SIGNATURES
        .iter()
        .filter(|(name, _)| {
            !matches!(
                *name,
                isoplane_abi::exports::FS_ALLOC | isoplane_abi::exports::FS_FREE
            )
        })
        .collect();
    assert_eq!(
        found.len(),
        managed_signatures.len(),
        "the generated adapter must supply every declared host entrypoint"
    );
    for (name, signature) in managed_signatures {
        let (params, results) = found
            .get(name)
            .unwrap_or_else(|| panic!("missing generated export {name}"));
        assert_eq!(
            (params.as_slice(), results.as_slice()),
            (signature.params, signature.results),
            "generated export {name} must match the ABI"
        );
    }
}

#[test]
fn managed_entropy_chunks_match_the_host_out_buffer_limit() {
    let source = sdk_file("Randomness.cs");
    let value = source
        .split("MaximumDrawBytes = ")
        .nth(1)
        .and_then(|value| value.split(';').next())
        .expect("the managed entropy wrapper must declare its maximum draw size")
        .parse::<usize>()
        .expect("the managed entropy draw cap must be an integer");
    assert_eq!(
        value,
        imports::MAX_OUT_BUFFER_BYTES,
        "managed entropy must chunk at the canonical host buffer limit"
    );
}

#[test]
fn local_debug_artifact_marker_matches_the_sdk_emitter() {
    let source = sdk_file("../Build/DebugManifest.cs");
    let prefix = "private const string LocalDebugSection = \"";
    let marker = source
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|value| value.strip_suffix("\";"))
        .expect("the SDK emitter must declare its local debug section name");
    assert_eq!(
        marker,
        isoplane_abi::LOCAL_DEBUG_SECTION,
        "the SDK debug artifact marker must match the host contract"
    );
}

#[test]
fn mono_memory_accounting_uses_the_contracts_wasm_page_size() {
    let patch = include_str!("../../../guest-runtime/patches/runtime/035-wasi-memory-limit.patch");
    let expression = format!(
        "(guint64)__builtin_wasm_memory_size (0) * {};",
        isoplane_abi::WASM_PAGE_SIZE
    );
    assert!(
        patch.contains(&expression),
        "the Mono memory query must convert wasm pages using the host contract's page size"
    );
}

#[test]
fn native_memory_queries_match_the_host_import_signature() {
    let definition = imports::lookup("memory_limit_bytes")
        .expect("the native collectors must have a canonical memory query");
    assert_eq!(
        (definition.sig.params, definition.sig.results),
        (&[][..], &[Ty::I64][..]),
        "the native collectors declare a parameterless 64-bit memory query"
    );
    let attribute = format!(
        "import_module(\"{}\"), import_name(\"{}\")",
        imports::HOST_MODULE,
        definition.name
    );
    for (patch, declaration) in [
        (
            include_str!("../../../guest-runtime/patches/runtime/035-wasi-memory-limit.patch"),
            "extern guint64 isoplane_memory_limit_bytes (void);",
        ),
        (
            include_str!("../../../guest-runtime/patches/runtimelab/008-wasi-memory-limit.patch"),
            "uint64_t isoplane_memory_limit_bytes();",
        ),
    ] {
        assert!(
            patch.contains(&attribute) && patch.contains(declaration),
            "each native collector must bind the canonical import as an unsigned 64-bit byte count"
        );
    }
}
