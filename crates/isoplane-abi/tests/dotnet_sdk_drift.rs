//! The C# SDK's hand-written mirrors of this crate, held to it: every extern
//! in `Abi/Imports.cs` against `IMPORTS`, and every enum in `Abi/Records.cs`
//! and `Abi/PollStatus.cs` against the discriminants postcard writes for the
//! Rust type. Runs on the stable toolchain with no .NET installed.

use std::collections::BTreeMap;

use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_abi::imports::{self, Ty};
use isoplane_abi::result::FutureError;
use isoplane_abi::status::{PollStatus, TrapCode};

fn sdk_file(name: &str) -> String {
    let path = isoplane_test_support::repository_root(env!("CARGO_MANIFEST_DIR"))
        .join("guest-sdks/dotnet/Abi")
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
            ("Background", DispatchKind::Background),
        ],
    );
}

/// A postcard reader with the operations `Records.cs` performs, replayed
/// in the order that file performs them.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn varint(&mut self) -> u64 {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let (byte, rest) = self.bytes.split_first().expect("an unterminated varint");
            self.bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    }

    fn apply(&mut self, operation: &str) -> Option<u64> {
        match operation {
            "U64" | "U32" | "Varint" => Some(self.varint()),
            "SkipBytes" => {
                let length = self.varint() as usize;
                self.bytes = &self.bytes[length..];
                None
            }
            "Bool" => {
                let (_, rest) = self.bytes.split_first().expect("a bool byte");
                self.bytes = rest;
                None
            }
            other => panic!("Records.cs reads a dispatch record with an operation this test does not replay: {other}"),
        }
    }
}

/// `(field, operation)` for every read `DispatchRecord.Decode` performs after
/// the dispatch kind; a read into nothing has an empty field.
fn csharp_dispatch_reads(src: &str) -> Vec<(String, String)> {
    let body = &src[src
        .find("static DispatchRecord Decode")
        .expect("the decoder")..];
    let body = &body[body
        .find("// TraceContext")
        .expect("the reads after the kind")..];
    let body = &body[..body.find("return d;").expect("the end of the decoder")];
    body.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//") && line.contains("r."))
        .map(|line| {
            let (target, call) = line.split_once("r.").expect("a reader call");
            let field = target
                .trim_end_matches(" = ")
                .trim_start_matches("d.")
                .to_string();
            let operation = call
                .split("()")
                .next()
                .expect("a reader operation")
                .to_string();
            (field, operation)
        })
        .collect()
}

#[test]
fn the_managed_dispatch_record_reads_the_rust_records_fields_in_order() {
    let record = DispatchRecord {
        dispatch_id: 9,
        kind: DispatchKind::Background,
        trace: TraceContext {
            trace_id: "trace".into(),
            span_id: "span".into(),
            sampled: true,
        },
        cancel_future: 1_000_001,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 1_700_000_000_000,
    };
    let bytes = isoplane_abi::encode(&record).expect("encodes");
    let mut reader = Reader { bytes: &bytes };
    assert_eq!(reader.varint(), record.dispatch_id);
    assert_eq!(
        reader.varint(),
        discriminant(&DispatchKind::Background).into()
    );
    let mut read = BTreeMap::new();
    for (field, operation) in csharp_dispatch_reads(&sdk_file("Records.cs")) {
        if let Some(value) = reader.apply(&operation) {
            read.insert(field, value);
        }
    }
    assert!(
        reader.bytes.is_empty(),
        "Records.cs leaves {} bytes of the dispatch record unread",
        reader.bytes.len()
    );
    let expected: BTreeMap<String, u64> = [
        ("CancelFuture", record.cancel_future),
        ("OccupancyBudgetMs", record.occupancy_budget_ms.into()),
        ("PendingFuturesLimit", record.pending_futures_limit.into()),
        ("NowMs", record.now_ms),
    ]
    .into_iter()
    .map(|(field, value)| (field.to_string(), value))
    .collect();
    assert_eq!(
        read, expected,
        "Records.cs reads the dispatch record's fields in a different order or shape than the Rust struct encodes"
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
            ("BufferLimit", FutureError::BufferLimit),
        ],
    );
}

#[test]
fn poll_status_tags_match_the_managed_boundary() {
    let source = sdk_file("PollStatus.cs");
    let managed: BTreeMap<String, u32> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("private const uint Tag"))
        .map(|line| {
            let (name, value) = line.split_once(" = ").expect("status tag declaration");
            (
                name.to_owned(),
                value
                    .trim_end_matches(';')
                    .parse()
                    .expect("numeric status tag"),
            )
        })
        .collect();
    let native = [
        ("Done", PollStatus::Done { response: 0 }),
        ("Pending", PollStatus::Pending),
        (
            "Trapped",
            PollStatus::Trapped {
                code: TrapCode::Panic,
            },
        ),
        ("Cancelled", PollStatus::Cancelled),
        ("Failed", PollStatus::Failed),
    ]
    .into_iter()
    .map(|(name, status)| (name.to_owned(), status.encode() as u32))
    .collect();
    assert_eq!(managed, native);
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
    ]
    .into_iter()
    .map(|(n, v)| (n.to_string(), v as u32))
    .collect();
    assert_eq!(
        cs, rust,
        "C# `TrapCode` in PollStatus.cs does not mirror the Rust enum's values"
    );
}

/// The managed signatures reached by the native Mono entrypoints.
fn managed_exports(source: &str) -> BTreeMap<String, (Vec<Ty>, Vec<Ty>)> {
    let native = sdk_file("../native/mono_entry.c");
    let mut found = BTreeMap::new();
    for function in native.split("\n}\n") {
        let Some(declaration) = function.lines().find_map(|line| {
            ["int32_t isoplane_", "int64_t isoplane_", "void isoplane_"]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        }) else {
            continue;
        };
        let export = declaration
            .split_once('(')
            .expect("a native entrypoint name")
            .0;
        let name = format!("isoplane_{export}");
        let method_name = function
            .split("invoke(\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a native entrypoint invokes its managed method");
        let declaration = source
            .lines()
            .filter_map(|line| line.split("public static ").nth(1))
            .find(|line| {
                line.split_once(' ')
                    .is_some_and(|(_, signature)| signature.starts_with(&format!("{method_name}(")))
            })
            .unwrap_or_else(|| panic!("{name} requires managed method {method_name}"));
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
                    &name,
                )
            })
            .collect();
        let results = if result == "void" {
            vec![]
        } else {
            vec![ty(result, &name)]
        };
        found.insert(name.to_string(), (params, results));
    }
    assert!(
        !found.is_empty(),
        "the Mono adapter must invoke managed entrypoints"
    );
    found
}

#[test]
fn generated_mono_runtime_entrypoints_match_the_host_export_contract() {
    let adapter = sdk_file("../MonoRuntime/src/Isoplane.MonoRuntime.csproj");
    assert!(
        adapter.contains("Isoplane.Exports.targets"),
        "the shared Mono runtime must consume the export generator"
    );
    let found = managed_exports(&sdk_file("../Build/Isoplane.Exports.targets"));
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
            .get(*name)
            .unwrap_or_else(|| panic!("missing generated export {name}"));
        assert_eq!(
            (params.as_slice(), results.as_slice()),
            (signature.params, signature.results),
            "generated export {name} must match the ABI"
        );
    }
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
fn the_mono_runtime_loader_reads_packages_where_the_host_serves_them() {
    let source = sdk_file("../MonoRuntime/src/PackageLoader.cs");
    let prefix = "public const string Root = \"";
    let root = source
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|value| value.strip_suffix("\";"))
        .expect("the package loader must declare its package root");
    assert_eq!(
        root,
        isoplane_abi::filesystem::APPLICATION_PACKAGE_ROOT,
        "the shared runtime must look for the package where the host serves it"
    );
}

#[test]
fn the_mono_debug_runtime_reads_its_debugger_where_the_host_serves_it() {
    let source = sdk_file("../native/mono_entry.c");
    let prefix = "#define ISOPLANE_DEBUG_AGENT_PATH \"";
    let path = source
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|value| value.strip_suffix('"'))
        .expect("the Mono entry must declare where it reads the debug session");
    assert_eq!(
        path,
        isoplane_abi::filesystem::DEBUG_AGENT_PATH,
        "a debug runtime must look for its debugger where a local debug session serves it"
    );
}

#[test]
fn mono_memory_accounting_uses_the_contracts_wasm_page_size() {
    let source = sdk_file("../../../vendor/dotnet-runtime/src/mono/mono/utils/memfuncs.c");
    let expression = format!(
        "(guint64)__builtin_wasm_memory_size (0) * {};",
        isoplane_abi::WASM_PAGE_SIZE
    );
    assert!(
        source.contains(&expression),
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
    let source = sdk_file("../../../vendor/dotnet-runtime/src/mono/mono/utils/memfuncs.c");
    assert!(
        source.contains(&attribute)
            && source.contains("extern guint64 isoplane_memory_limit_bytes (void);"),
        "Mono's collector must bind the canonical import as an unsigned 64-bit byte count"
    );
}
