//! Canonical native readiness entrypoints observed in the pinned runtime.
use crate::imports::{Sig, Ty};
use crate::pal::{Delegation, Disposition, PalDef};

/// Readiness and coarse deadline operations; cannot create connections.
pub const POLL_IMPORT: &str = "poll_call";

/// Canonical adapter operation identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationId(pub u32);

/// The host operation or explicit refusal implemented for a canonical entry.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// A relative or absolute coarse deadline.
    Timer {
        /// Whether the supplied timestamp is absolute.
        absolute: bool,
    },
    /// The frozen coarse clock in canonical nanoseconds.
    ClockNow,
    /// The coarse clock resolution.
    ClockResolution,
    /// Inspect a list of pollables without blocking.
    Poll,
    /// Inspect one pollable without blocking.
    Ready,
    /// Release a pollable and cancel its pending work.
    Drop,
    /// Explicitly refuse synchronous waiting.
    Trap,
}

/// The single definition consumed by the validator, native wrappers and engine adapter.
pub struct Definition {
    /// Stable operation id in the scalar packet.
    pub id: OperationId,
    /// Native wasi-libc symbol suffix.
    pub symbol: &'static str,
    /// WISP capability used for this operation.
    pub capability: &'static str,
    /// Canonical imported signature and name.
    pub pal: PalDef,
    /// Implemented action.
    pub action: Action,
}

const fn definition(
    id: u32,
    symbol: &'static str,
    module: &'static str,
    name: &'static str,
    capability: &'static str,
    sig: Sig,
    action: Action,
) -> Definition {
    Definition { id: OperationId(id), symbol, capability, action, pal: PalDef {
        module, func: name, sig: Some(sig), disposition: Disposition::Delegate(Delegation::Cap(capability)),
        why: "host-owned coarse deadlines and nonblocking readiness; synchronous waits remain denied",
    }}
}

/// Every admitted canonical entry, with no ambient network fallback.
pub const DEFINITIONS: &[Definition] = &[
    definition(
        0,
        "monotonic_clock_subscribe_duration",
        "wasi:clocks/monotonic-clock@0.2.0",
        "subscribe-duration",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I64],
            results: &[Ty::I32],
        },
        Action::Timer { absolute: false },
    ),
    definition(
        1,
        "poll_pollable_drop",
        "wasi:io/poll@0.2.0",
        "[resource-drop]pollable",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop,
    ),
    definition(
        2,
        "poll_poll",
        "wasi:io/poll@0.2.0",
        "poll",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Poll,
    ),
    definition(
        55,
        "monotonic_clock_now",
        "wasi:clocks/monotonic-clock@0.2.0",
        "now",
        POLL_IMPORT,
        Sig {
            params: &[],
            results: &[Ty::I64],
        },
        Action::ClockNow,
    ),
    definition(
        56,
        "monotonic_clock_resolution",
        "wasi:clocks/monotonic-clock@0.2.0",
        "resolution",
        POLL_IMPORT,
        Sig {
            params: &[],
            results: &[Ty::I64],
        },
        Action::ClockResolution,
    ),
    definition(
        57,
        "monotonic_clock_subscribe_instant",
        "wasi:clocks/monotonic-clock@0.2.0",
        "subscribe-instant",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I64],
            results: &[Ty::I32],
        },
        Action::Timer { absolute: true },
    ),
    definition(
        58,
        "poll_method_pollable_ready",
        "wasi:io/poll@0.2.0",
        "[method]pollable.ready",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Ready,
    ),
    definition(
        59,
        "poll_method_pollable_block",
        "wasi:io/poll@0.2.0",
        "[method]pollable.block",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
];

/// Maximum number of scalar arguments accepted by a canonical packet.
pub const MAX_ARGUMENTS: usize = {
    let mut maximum = 0;
    let mut index = 0;
    while index < DEFINITIONS.len() {
        if let Some(signature) = DEFINITIONS[index].pal.sig {
            if signature.params.len() > maximum {
                maximum = signature.params.len();
            }
        }
        index += 1;
    }
    maximum
};

/// Generate native wrappers from the same signatures the validator admits.
pub fn native_wrappers() -> String {
    let mut output = String::from("/* Generated from wisp_abi::native_io::canonical. */\n");
    let capability = POLL_IMPORT;
    output.push_str(&format!("__attribute__((import_module(\"wisp\"), import_name(\"{capability}\")))\nextern int64_t wisp_{capability}(int32_t operation, const uint64_t *arguments, int32_t count);\n"));
    for definition in DEFINITIONS {
        if matches!(
            definition.action,
            Action::ClockNow | Action::ClockResolution
        ) {
            continue;
        }
        let Some(signature) = definition.pal.sig else {
            continue;
        };
        let parameters: Vec<_> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| format!("{} a{index}", c_type(*ty)))
            .collect();
        let arguments: Vec<_> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                format!(
                    "({})a{index}",
                    match ty {
                        Ty::I32 => "uint32_t",
                        Ty::I64 => "uint64_t",
                    }
                )
            })
            .collect();
        let result = signature
            .results
            .first()
            .copied()
            .map(c_type)
            .unwrap_or("void");
        let returned = if signature.results.is_empty() {
            "".to_string()
        } else {
            format!("return ({result})")
        };
        output.push_str(&format!("{result} __wasm_import_{}({}) {{ const uint64_t args[] = {{ {} }}; {returned}wisp_{}({}, args, {}); }}\n",
            definition.symbol, if parameters.is_empty() { "void".to_string() } else { parameters.join(", ") },
            if arguments.is_empty() { "0".to_string() } else { arguments.join(", ") }, definition.capability, definition.id.0, parameters.len()));
    }
    output
}

fn c_type(ty: Ty) -> &'static str {
    match ty {
        Ty::I32 => "int32_t",
        Ty::I64 => "int64_t",
    }
}

/// Generate the managed P/Invoke adaptation map as tab-separated records.
pub fn managed_imports() -> String {
    let mut output = String::new();
    for definition in DEFINITIONS {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            definition.pal.module, definition.pal.func, definition.capability, definition.id.0
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_wrappers_match_the_canonical_definition() {
        assert_eq!(
            super::native_wrappers(),
            include_str!("../../../../guest-sdks/dotnet/native/wisp_native_io.h"),
            "regenerate the native I/O header from its ABI example"
        );
    }
    #[test]
    fn managed_imports_match_the_canonical_definition() {
        assert_eq!(
            super::managed_imports(),
            include_str!("../../../../guest-sdks/dotnet/Build/RuntimeAdapter/NativeImports.tsv"),
            "regenerate the managed native I/O map from its ABI example"
        );
    }
}
