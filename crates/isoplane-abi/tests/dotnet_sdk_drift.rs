//! The C# SDK's hand-written mirrors of this crate's wire values, held to it:
//! every enum in `Abi/Records.cs` and `Abi/PollStatus.cs` against the
//! discriminants postcard writes for the Rust type. Runs on the stable
//! toolchain with no .NET installed.

use std::collections::BTreeMap;

use isoplane_abi::dispatch::{DispatchKind, HttpRequest};
use isoplane_abi::status::{PollStatus, TrapCode};

fn sdk_file(name: &str) -> String {
    let path = isoplane_test_support::repository_root(env!("CARGO_MANIFEST_DIR"))
        .join("guest-sdks/dotnet/Abi")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
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
    let bytes = postcard::to_allocvec(v).expect("encodes");
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
                }),
            ),
            ("Background", DispatchKind::Background),
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
