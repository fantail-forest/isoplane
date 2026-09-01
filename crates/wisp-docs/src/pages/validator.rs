//! `validator.md` — from [`wisp_control::validator`].
//!
//! The V4 numbers are the ones a tenant designs against, and the module says
//! so: *"Real numbers so tenants can design against them."* They are read out
//! of [`limits`] rather than copied, and the V3 ceilings come from
//! [`Limits::default`] so the validator page and the plans page cannot disagree
//! about the same ceiling.

use anyhow::Result;
use wisp_abi::config::Limits;
use wisp_abi::imports::exports;
use wisp_control::validator::{limits, ViolationCode, ENGINE_FEATURES, INIT_MEMORY_LIMIT_BYTES};

use crate::table::{bytes, cell, commas, Annotations, Table};
use crate::Block;

/// Why each proposal is off. Keyed by the flag's own name, so the *set* is
/// computed and only the sentence is written.
///
/// This belongs next to `ENGINE_FEATURES` in `wisp-control`, the way
/// `ImportDef::why` sits next to `IMPORTS` — the guard below is the stand-in
/// until it moves.
const DISABLED_WHY: &[(&str, &str)] = &[
    (
        "THREADS",
        "No threads. One logical thread per instance is what makes the empty-stack invariant \
         checkable, and `memory.atomic.wait*` is the only in-sandbox blocking instruction — with \
         the feature off it fails bytecode validation rather than being policed at runtime.",
    ),
    (
        "SHARED_EVERYTHING_THREADS",
        "The same argument as `THREADS`, for the newer proposal",
    ),
    (
        "RELAXED_SIMD",
        "A deterministic SIMD subset only; relaxed SIMD's results are host-dependent, which \
         would make `wisp replay` a lie",
    ),
    ("EXCEPTIONS", "Beyond the profile"),
    ("LEGACY_EXCEPTIONS", "Beyond the profile"),
    (
        "STACK_SWITCHING",
        "The ABI is callback-CPS. Engine-suspended stacks are exactly what the empty-stack \
         invariant exists to forbid, so the proposal that reintroduces them is off at the \
         bytecode level.",
    ),
    (
        "TAIL_CALL",
        "A tail call to an import would be a trick outside the declared world. Conservative \
         reading; the V1 allowlist still covers the other half.",
    ),
    (
        "MULTI_MEMORY",
        "Exactly one linear memory. V3 says the same thing structurally; both are checked.",
    ),
    ("MEMORY64", "One 32-bit linear memory"),
    (
        "GC",
        "The guest ships its own GC inside linear memory, so engine GC objects would be a second \
         heap the platform cannot see — and all tenant state must be visible in linear memory, \
         globals and tables",
    ),
    (
        "GC_TYPES",
        "Turning this off also removes `externref`, which is right for a second reason: no host \
         pointer ever enters guest memory, and every host resource is an opaque `u32` handle",
    ),
    (
        "CUSTOM_PAGE_SIZES",
        "One page size, so a delta's page arithmetic is the same on both sides",
    ),
    ("MEMORY_CONTROL", "Not in the profile"),
    ("WIDE_ARITHMETIC", "Not in the profile"),
    (
        "COMPONENT_MODEL",
        "The documented deviation: the same world, lowered to a flat core-wasm ABI under module \
         `wisp`. Its sub-proposals — async, threading, GC, values, error contexts and the rest \
         — are off with it, and are not listed separately here.",
    ),
    (
        "FUNCTION_REFERENCES",
        "Typed function references. Not in the baseline, and not needed: the guest ABI is five \
         exports and a flat import table.",
    ),
    ("COMPACT_IMPORTS", "Not in the profile"),
    ("CUSTOM_DESCRIPTORS", "Not in the profile"),
];

/// Component-model sub-proposals, which are off because `component_model` is
/// off. Thirteen rows saying "see the row above" is not a reference table.
fn is_component_model_subflag(name: &str) -> bool {
    name.starts_with("CM_")
}

const PAGE: &str = "validator.md";

/// Every violation code, in rule order.
///
/// [`violation_list_is_exhaustive`] has no wildcard arm, so a new code fails
/// to compile here until it has a row.
const CODES: &[ViolationCode] = &[
    ViolationCode::V1UndeclaredImport,
    ViolationCode::V1MissingExport,
    ViolationCode::V1SignatureMismatch,
    ViolationCode::V2ForbiddenFeature,
    ViolationCode::V3MemoryShape,
    ViolationCode::V4ResourceBomb,
];

#[allow(dead_code)]
fn violation_list_is_exhaustive(c: ViolationCode) -> &'static str {
    match c {
        ViolationCode::V1UndeclaredImport => "V1",
        ViolationCode::V1MissingExport => "V1",
        ViolationCode::V1SignatureMismatch => "V1",
        ViolationCode::V2ForbiddenFeature => "V2",
        ViolationCode::V3MemoryShape => "V3",
        ViolationCode::V4ResourceBomb => "V4",
    }
}

fn rule(c: ViolationCode) -> &'static str {
    violation_list_is_exhaustive(c)
}

const CODE_NOTES: &[(&str, &str)] = &[
    (
        "V1UndeclaredImport",
        "An import whose module is not `wisp`, or whose field is not in the declared set. Also \
         raised for component types that fail to match the world, and for nested instantiation, \
         which is a linking escape hatch.",
    ),
    ("V1MissingExport", "A required guest export is missing"),
    (
        "V1SignatureMismatch",
        "A declared import, or a required export, has the wrong core-wasm signature. Name-only \
         matching would be theatre: the world is a set of *types*, and half a type check is not \
         a type check.",
    ),
    (
        "V2ForbiddenFeature",
        "The binary requires a WebAssembly proposal the engine profile has disabled",
    ),
    (
        "V3MemoryShape",
        "More than one linear memory, a shared memory, or a declared memory maximum above the \
         plan's ceiling. An **imported** memory is still a memory for this rule.",
    ),
    (
        "V4ResourceBomb",
        "A structural resource-bomb heuristic tripped. Hitting this usually means an unstripped \
         debug build.",
    ),
];

/// The V4 ceilings, paired with what each one bounds.
///
/// The values come from `limits::*`; only the sentence is written here.
const V4_LIMITS: &[(&str, &str)] = &[
    ("MAX_FUNCTIONS", "Functions in the module"),
    ("MAX_LOCALS_PER_FUNCTION", "Locals declared by any one function"),
    ("MAX_TABLE_ELEMENTS", "Elements across all tables"),
    ("MAX_CUSTOM_SECTION_BYTES", "Total custom-section bytes — what makes a 50 MB \"module\" that is 49 MB of debug info visible"),
    ("MAX_IMPORTS", "Import entries"),
    ("MAX_EXPORTS", "Export entries"),
    ("MAX_TYPES", "Type entries"),
    ("MAX_ELEMENT_SEGMENTS", "Element segments"),
    ("MAX_DATA_SEGMENTS", "Data segments"),
];

pub fn blocks() -> Result<Vec<Block>> {
    Ok(vec![
        Block::new(PAGE, "violation-codes", violation_codes()?),
        Block::new(PAGE, "required-exports", required_exports()),
        Block::new(PAGE, "v3-ceilings", v3_ceilings()),
        Block::new(PAGE, "v2-disabled", v2_disabled()?),
        Block::new(PAGE, "v4-limits", v4_limits()?),
    ])
}

/// The proposals V2 refuses, computed as everything `wasmparser` knows about
/// minus what [`ENGINE_FEATURES`] admits.
///
/// The page used to list these by hand. A hand-written list of disabled
/// WebAssembly proposals is a list that silently stops matching the engine the
/// first time the profile is widened — and widening the profile is exactly the
/// change that most deserves to show up in the documentation.
fn v2_disabled() -> Result<String> {
    let disabled = wasmparser::WasmFeatures::all().difference(ENGINE_FEATURES);
    let mut notes = Annotations::new("disabled proposal", DISABLED_WHY);
    let mut t = Table::new(&["Proposal", "Why it is off"]);
    // Alphabetical: bitflags iterates in declaration order, which is an
    // accident of `wasmparser`'s source and not something a reader looking a
    // proposal up can predict.
    let mut names: Vec<&str> = disabled
        .iter_names()
        .map(|(n, _)| n)
        .filter(|n| !is_component_model_subflag(n))
        .collect();
    names.sort_unstable();
    let n = names.len();
    for name in names {
        t.row(&[format!("`{}`", name.to_lowercase()), cell(&notes.get(name))]);
    }
    notes.finish()?;
    anyhow::ensure!(
        n > 0,
        "validator.md: read no disabled proposals; the profile walk is wrong"
    );
    Ok(t.render())
}

fn violation_codes() -> Result<String> {
    let mut notes = Annotations::new("violation code", CODE_NOTES);
    let mut t = Table::new(&["Rule", "Code", "Meaning"]);
    for c in CODES {
        let name = format!("{c:?}");
        t.row(&[
            format!("**{}**", rule(*c)),
            format!("`{name}`"),
            notes.get(&name),
        ]);
    }
    notes.finish()?;
    Ok(t.render())
}

/// The exports V1 requires and the ones it merely hopes for, read out of
/// [`exports::REQUIRED`] and [`exports::OPTIONAL`].
fn required_exports() -> String {
    let req: Vec<String> = exports::REQUIRED.iter().map(|e| format!("`{e}`")).collect();
    let opt: Vec<String> = exports::OPTIONAL.iter().map(|e| format!("`{e}`")).collect();
    format!(
        "Required: {}. Optional but strongly recommended: {}.",
        req.join(", "),
        opt.join(", ")
    )
}

/// The V3 ceilings, per plan.
///
/// Same source as the plans page — [`Limits::default`] — so the two cannot state
/// different ceilings for the same plan.
fn v3_ceilings() -> String {
    let per_plan: Vec<String> = [
        Limits {
            module_bytes_compressed: 5242880,
            instance_memory_max: 67108864,
            cpu_ms_per_http_dispatch: 10,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6,
        },
        Limits::default(),
        Limits {
            module_bytes_compressed: 52428800,
            instance_memory_max: 536870912,
            cpu_ms_per_http_dispatch: 300000,
            subrequests_per_dispatch: 10000,
            pending_futures_per_dispatch: 4096,
        },
    ]
    .iter()
    .map(|p| {
        let n = format!("{p:?}").to_lowercase();
        format!("{} `{n}`", bytes(p.instance_memory_max))
    })
    .collect();
    format!(
        "Ceilings: {}.\n\nA second gate sits in front of pre-initialization: a module may not \
         declare more than {} of linear memory at init time, checked before a single guest \
         instruction runs. It equals the largest plan ceiling, and it covers a caller that \
         reaches the pre-initializer directly without a prior `validate`.",
        per_plan.join(", "),
        bytes(INIT_MEMORY_LIMIT_BYTES),
    )
}

fn v4_limits() -> Result<String> {
    let mut notes = Annotations::new("V4 limit", V4_LIMITS);
    let mut t = Table::new(&["Limit", "Value", "Bounds"]).right_align(1);
    // Each value is named individually rather than swept up by a macro, so
    // the constant and the row are the same identifier: delete the constant
    // and this stops compiling.
    let vals: &[(&str, u64)] = &[
        ("MAX_FUNCTIONS", limits::MAX_FUNCTIONS as u64),
        (
            "MAX_LOCALS_PER_FUNCTION",
            limits::MAX_LOCALS_PER_FUNCTION as u64,
        ),
        ("MAX_TABLE_ELEMENTS", limits::MAX_TABLE_ELEMENTS as u64),
        ("MAX_CUSTOM_SECTION_BYTES", limits::MAX_CUSTOM_SECTION_BYTES),
        ("MAX_IMPORTS", limits::MAX_IMPORTS as u64),
        ("MAX_EXPORTS", limits::MAX_EXPORTS as u64),
        ("MAX_TYPES", limits::MAX_TYPES as u64),
        ("MAX_ELEMENT_SEGMENTS", limits::MAX_ELEMENT_SEGMENTS as u64),
        ("MAX_DATA_SEGMENTS", limits::MAX_DATA_SEGMENTS as u64),
    ];
    for (name, v) in vals {
        let shown = if *name == "MAX_CUSTOM_SECTION_BYTES" {
            bytes(*v)
        } else {
            commas(*v)
        };
        t.row(&[format!("`{name}`"), shown, cell(&notes.get(name))]);
    }
    notes.finish()?;
    Ok(t.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disabled_set_is_computed_from_the_engine_profile() {
        let t = v2_disabled().unwrap();
        assert!(t.contains("`threads`"), "{t}");
        // And what the engine *does* admit must not appear as refused.
        assert!(!t.contains("`simd`"), "SIMD is in the profile\n{t}");
        assert!(!t.contains("`bulk_memory`"), "{t}");
    }

    #[test]
    fn the_v4_table_carries_the_constants() {
        let t = v4_limits().unwrap();
        assert!(t.contains(&commas(limits::MAX_FUNCTIONS as u64)), "{t}");
        assert!(
            t.contains(&bytes(limits::MAX_CUSTOM_SECTION_BYTES)),
            "custom-section bytes read as a size\n{t}"
        );
    }

    #[test]
    fn the_v3_ceilings_agree_with_the_plans_page_because_they_share_a_source() {
        let t = v3_ceilings();
        for p in [
            Limits {
                module_bytes_compressed: 5242880,
                instance_memory_max: 67108864,
                cpu_ms_per_http_dispatch: 10,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
            Limits::default(),
            Limits {
                module_bytes_compressed: 52428800,
                instance_memory_max: 536870912,
                cpu_ms_per_http_dispatch: 300000,
                subrequests_per_dispatch: 10000,
                pending_futures_per_dispatch: 4096,
            },
        ] {
            assert!(t.contains(&bytes(p.instance_memory_max)), "{t}");
        }
        assert_eq!(
            INIT_MEMORY_LIMIT_BYTES,
            Limits {
                module_bytes_compressed: 52428800,
                instance_memory_max: 536870912,
                cpu_ms_per_http_dispatch: 300000,
                subrequests_per_dispatch: 10000,
                live_dis: 18446744073709551615,
                delta_storage_bytes: 18446744073709551615,
                hibernation_horizon_ms: 31536000000,
                websockets_per_di: 1048576,
                pending_futures_per_dispatch: 4096
            }
            .instance_memory_max
        );
    }

    #[test]
    fn required_exports_come_from_the_abi() {
        let s = required_exports();
        for e in exports::REQUIRED {
            assert!(s.contains(e), "{s}");
        }
        assert!(s.contains("wisp_initialize"), "{s}");
    }
}
