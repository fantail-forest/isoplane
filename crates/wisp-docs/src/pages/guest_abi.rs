//! Guest export signatures, trap codes and constants read from the ABI definitions.

use anyhow::Result;
use wisp_abi::imports::{exports, Sig, Ty};
use wisp_abi::status::TrapCode;
use wisp_abi::{ABI_REV, PAGE_SIZE, WASM_PAGE_SIZE};

use crate::table::{cell, commas, tick, Annotations, Table};
use crate::Block;

const PAGE: &str = "guest-abi.md";

/// The exports, in the order the page lists them: required first.
const EXPORTS: &[&str] = &[
    exports::ALLOC,
    exports::HANDLE,
    exports::WAKE,
    exports::MEMORY,
    exports::GC_HINT,
    exports::INITIALIZE,
];

/// Parameter names, so `(i32, i32) -> i64` reads as something.
///
/// Core wasm carries no parameter names; these are the only hand-written part
/// of the signature column, and the types beside them come from
/// [`exports::SIGNATURES`].
const PARAM_NAMES: &[(&str, &[&str])] = &[
    ("wisp_alloc", &["size", "align"]),
    ("wisp_handle", &["ptr", "len"]),
    ("wisp_wake", &["future", "ptr", "len"]),
    ("wisp_gc_hint", &["pressure"]),
    ("wisp_initialize", &[]),
];

const EXPORT_NOTES: &[(&str, &str)] = &[
    (
        "wisp_alloc",
        "Host→guest buffer handoff. The **guest** frees the buffer when the host hands it back \
         in `wisp_handle`/`wisp_wake`.",
    ),
    (
        "wisp_handle",
        "A new top-level work item. `(ptr,len)` is a postcard `DispatchRecord`. Returns a packed \
         `PollStatus`.",
    ),
    (
        "wisp_wake",
        "Completion of a previously returned future. `(ptr,len)` is a postcard `ResultRecord`. \
         Returns a packed `PollStatus`.",
    ),
    ("memory", "The guest linear memory"),
    ("wisp_gc_hint", "Pre-snapshot compaction hint"),
    (
        "wisp_initialize",
        "Optional application initialization entrypoint, called during preparation.",
    ),
];

const TRAP_NOTES: &[(&str, &str)] = &[
    ("Unknown", "No code was supplied, or one this build does not recognise"),
    ("Panic", "Guest panic"),
    ("OutOfMemory", "Allocation failed, or the memory ceiling was hit"),
    (
        "CapabilityViolation",
        "A capability was invoked that this module did not declare, or that the init sandbox \
         forbids",
    ),
    ("Protocol", "Malformed dispatch or result encoding"),
    ("TooManyFutures", "The simultaneous-pending-futures cap was exceeded"),
];

const TRAP_CODES: &[TrapCode] = &[
    TrapCode::Unknown,
    TrapCode::Panic,
    TrapCode::OutOfMemory,
    TrapCode::CapabilityViolation,
    TrapCode::Protocol,
    TrapCode::TooManyFutures,
];

#[allow(dead_code)]
fn trap_list_is_exhaustive(c: TrapCode) -> u32 {
    match c {
        TrapCode::Unknown => 0,
        TrapCode::Panic => 1,
        TrapCode::OutOfMemory => 2,
        TrapCode::CapabilityViolation => 3,
        TrapCode::Protocol => 4,
        TrapCode::TooManyFutures => 5,
    }
}

/// Render the ABI export, trap and constant tables.
pub fn blocks() -> Result<Vec<Block>> {
    Ok(vec![
        Block::new(PAGE, "guest-exports", guest_exports()?),
        Block::new(PAGE, "trap-codes", trap_codes()?),
        Block::new(PAGE, "abi-constants", abi_constants()),
    ])
}

fn ty(t: Ty) -> &'static str {
    match t {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    }
}

fn signature(name: &str) -> String {
    let Some(sig) = exports::signature(name) else {
        // `memory` is an export with no function signature.
        return "—".to_string();
    };
    let Sig { params, results } = sig;
    let names = PARAM_NAMES.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).unwrap_or(&[]);
    let ps: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(i, t)| match names.get(i) {
            Some(n) => format!("{} {n}", ty(*t)),
            None => ty(*t).to_string(),
        })
        .collect();
    let res = if results.is_empty() {
        "()".to_string()
    } else {
        results.iter().map(|t| ty(*t).to_string()).collect::<Vec<_>>().join(", ")
    };
    format!("`({}) -> {res}`", ps.join(", "))
}

fn guest_exports() -> Result<String> {
    let mut notes = Annotations::new("guest export", EXPORT_NOTES);
    let mut t = Table::new(&["Export", "Core-wasm signature", "Required", "Purpose"]);
    for name in EXPORTS {
        let required = exports::REQUIRED.contains(name);
        if !required && !exports::OPTIONAL.contains(name) {
            anyhow::bail!(
                "guest-abi.md: export `{name}` is in neither REQUIRED nor OPTIONAL in wisp-abi"
            );
        }
        t.row(&[
            format!("`{name}`"),
            signature(name),
            if required { tick(true).to_string() } else { "optional".into() },
            notes.get(name),
        ]);
    }
    // Neither list may hold an export the page omits.
    for e in exports::REQUIRED.iter().chain(exports::OPTIONAL) {
        if !EXPORTS.contains(e) {
            anyhow::bail!(
                "guest-abi.md: `{e}` is declared in wisp-abi but missing from the page's export \
                 list in crates/wisp-docs"
            );
        }
    }
    notes.finish()?;
    Ok(t.render())
}

fn trap_codes() -> Result<String> {
    let mut notes = Annotations::new("trap code", TRAP_NOTES);
    let mut t = Table::new(&["Code", "Value", "Meaning"]).right_align(1);
    for c in TRAP_CODES {
        let name = format!("{c:?}");
        // The discriminant, read off the type rather than counted by hand.
        let v = *c as u32;
        t.row(&[format!("`{name}`"), v.to_string(), cell(&notes.get(&name))]);
        // A code that does not survive the wire round-trip would be a code a
        // guest can never actually report.
        if TrapCode::from_u32(v) != *c {
            anyhow::bail!("guest-abi.md: TrapCode::{name} does not round-trip through {v}");
        }
    }
    notes.finish()?;
    Ok(t.render())
}

fn abi_constants() -> String {
    let mut t = Table::new(&["Constant", "Value", "What it fixes"]).right_align(1);
    t.row(&[
        "`ABI_REV`".into(),
        ABI_REV.to_string(),
        cell(
            "The version of the guest host-interface contract.",
        ),
    ]);
    t.row(&[
        "engine page".into(),
        format!("{} bytes", commas(PAGE_SIZE as u64)),
        cell("Engine memory-page size"),
    ]);
    t.row(&[
        "wasm page".into(),
        format!("{} bytes", commas(WASM_PAGE_SIZE as u64)),
        cell("The unit a module declares its memory minimum and maximum in"),
    ]);
    t.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_signatures_come_from_the_abi() {
        assert_eq!(signature("wisp_wake"), "`(i64 future, i32 ptr, i32 len) -> i64`");
        assert_eq!(signature("wisp_initialize"), "`() -> ()`");
        assert_eq!(signature("memory"), "—");
    }

    #[test]
    fn trap_values_are_the_discriminants() {
        let t = trap_codes().unwrap();
        for c in TRAP_CODES {
            assert!(t.contains(&format!("`{c:?}` | {}", *c as u32)), "{c:?} is misnumbered\n{t}");
        }
    }

    #[test]
    fn an_export_the_abi_declares_cannot_be_left_off_the_page() {
        // Proven by construction: every REQUIRED and OPTIONAL name is checked
        // against EXPORTS, and the two lists agree today.
        guest_exports().unwrap();
        let listed: Vec<&&str> = EXPORTS.iter().collect();
        assert_eq!(listed.len(), exports::REQUIRED.len() + exports::OPTIONAL.len());
    }
}
