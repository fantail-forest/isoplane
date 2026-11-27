use crate::imports::Sig;
use crate::native_io::canonical::{
    Definition, MONOTONIC_CLOCK_SUBSCRIBE_DURATION, POLL_POLL, POLL_POLLABLE_DROP,
};

pub const WASI_PREFIX: &str = "wasi:";

#[derive(Debug)]
pub struct PalDef {
    /// Core-wasm module name, `wasi:io/poll@0.2.0`.
    pub module: &'static str,
    /// Core-wasm field name in canonical-ABI spelling, `poll` or
    /// `[resource-drop]pollable`.
    pub func: &'static str,
    /// The native operation that answers the import.
    pub operation: &'static Definition,
}

impl PalDef {
    /// `module::func`, the spelling used in violation messages.
    pub fn full_name(&self) -> String {
        format!("{}::{}", self.module, self.func)
    }

    /// The signature the import is declared with.
    pub fn sig(&self) -> Sig {
        self.operation.sig
    }
}

const MONOTONIC_CLOCK: &str = "wasi:clocks/monotonic-clock@0.2.0";
const IO_POLL: &str = "wasi:io/poll@0.2.0";

/// Every admitted WASI import; one not listed is rejected. Each entry is
/// answered by the native operation its `isoplane` capability already offers,
/// so an entry that is not would give the guest a channel, time or entropy
/// outside the capability table.
pub const PAL_IMPORTS: &[PalDef] = &[
    PalDef {
        module: MONOTONIC_CLOCK,
        func: "subscribe-duration",
        operation: &MONOTONIC_CLOCK_SUBSCRIBE_DURATION,
    },
    PalDef {
        module: IO_POLL,
        func: "poll",
        operation: &POLL_POLL,
    },
    PalDef {
        module: IO_POLL,
        func: "[resource-drop]pollable",
        operation: &POLL_POLLABLE_DROP,
    },
];

/// Whether `module` is a WASI interface at all, so a rejection can say "not in
/// the PAL whitelist" rather than "not in the capability table".
pub fn is_wasi(module: &str) -> bool {
    module.starts_with(WASI_PREFIX)
}

pub fn lookup(module: &str, func: &str) -> Option<&'static PalDef> {
    PAL_IMPORTS
        .iter()
        .find(|p| p.module == module && p.func == func)
}

/// The quantum of the coarse clock, which the native clock reports as its
/// resolution and to which the host rounds every timer.
pub const CLOCK_QUANTUM_NS: i64 = 1_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_native_clock_reports_the_host_clock_quantum() {
        let native = include_str!("../../../guest-sdks/dotnet/native/isoplane_clock.c");
        let definition = format!("#define ISOPLANE_CLOCK_QUANTUM_NS {CLOCK_QUANTUM_NS}LL");
        assert!(
            native.lines().any(|line| line == definition),
            "the native clock resolution must match the host PAL clock quantum"
        );
    }
}
