//! Admitted WASI imports and their host capability dispositions.

pub use wisp_abi::pal::*;


// The adapter *bodies* moved to `wisp_engine::pal` so that the data plane can
// compose the same surface it validates. `wisp-control` depends on
// `wisp-engine`, so re-exporting is the direction that works — and it keeps
// this module the single name every caller already imports.
//
// The alternative was a second implementation of four host functions, which
// is exactly the shape of drift this table exists to prevent: a validator
// that admits a `wasi:*` import and a linker that satisfies it differently is
// a hole with a whitelist in front of it.
pub use wisp_engine::pal::{link, PalSource};

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests below reach for the WISP table; the module itself is
    // re-exports, so importing this at module scope would be an unused import
    // in every non-test build.
    use wisp_abi::imports;

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for p in PAL_IMPORTS {
            assert!(seen.insert((p.module, p.func)), "duplicate PAL entry {}", p.full_name());
        }
    }

    #[test]
    fn every_entry_is_a_wasi_name() {
        for p in PAL_IMPORTS {
            assert!(is_wasi(p.module), "{} is not a wasi: module", p.full_name());
            assert_ne!(p.module, imports::HOST_MODULE);
        }
    }

    /// **No-new-channels, structural half.** You cannot delegate to a
    /// capability the platform does not have, so every delegation target must
    /// be a real entry in the one capability table.
    #[test]
    fn every_delegation_targets_a_real_platform_capability() {
        for p in delegated() {
            if let Some(cap) = p.disposition.capability() {
                assert!(
                    imports::lookup(cap).is_some(),
                    "{} delegates to `{cap}`, which is not in wisp_abi::imports::IMPORTS",
                    p.full_name()
                );
            }
        }
    }

    /// **No-new-channels, the part that would hurt.** A WASI import must never
    /// become a door to egress, storage, secrets or sockets. Those groups are
    /// capabilities a tenant declares and a manifest displays; reaching one
    /// through `wasi:*` would be precisely the "capability absent from
    /// `wisp:platform`" the rule forbids — and worse, it would arrive
    /// unlabelled.
    #[test]
    fn no_delegation_reaches_an_io_or_secret_capability() {
        use wisp_abi::imports::Group::*;
        let forbidden =
            [Http, Kv, Blob, Queue, Sql, Durable, Socket, Secret, Service, Response, Ctx];
        for p in delegated() {
            let Some(cap) = p.disposition.capability() else { continue };
            let def = imports::lookup(cap).expect("checked above");
            assert!(
                !forbidden.contains(&def.group),
                "{} delegates to `{cap}` in group {:?} — a WASI import must not be a door \
                 to I/O or secrets (§4.3 no-new-channels)",
                p.full_name(),
                def.group
            );
            assert!(
                !def.sensitive,
                "{} delegates to `{cap}`, which is marked sensitive (§8.6)",
                p.full_name()
            );
        }
    }

    /// The adapter draws on exactly two `wisp:platform` capabilities. A third
    /// is a new information channel into the sandbox and must not arrive as a
    /// patch — the same argument `wisp_abi::imports`' own
    /// `host_writes_into_guest_memory_through_exactly_three_imports` makes.
    #[test]
    fn the_adapter_draws_on_exactly_two_platform_capabilities() {
        assert_eq!(delegation_targets(), vec!["clock_now_coarse", "entropy_fill"]);
    }

    /// The groups §2.5.4/§8.4 deny outright must be trap-on-call to the last
    /// entry. A single delegated filesystem or socket import would be an
    /// ambient-authority regression that no other test would notice.
    #[test]
    fn the_denied_interfaces_are_trap_on_call_without_exception() {
        for p in PAL_IMPORTS {
            let denied = p.module.starts_with("wasi:filesystem/")
                || p.module.starts_with("wasi:sockets/")
                || p.module == IO_STREAMS
                || p.module == IO_POLL
                || p.module == CLI_STDIN
                || p.module == CLI_STDOUT
                || p.module == CLI_STDERR
                || p.module == WALL_CLOCK;
            if denied {
                assert_eq!(
                    p.disposition,
                    Disposition::TrapOnCall,
                    "{} must be trap-on-call",
                    p.full_name()
                );
            }
        }
    }

    /// Delegated entries carry a checked signature; trap entries deliberately
    /// do not. Getting this backwards would either skip a type check on a body
    /// we actually run, or pin a shape on a stub we never implement.
    #[test]
    fn signatures_are_declared_exactly_for_delegated_entries() {
        for p in PAL_IMPORTS {
            assert_eq!(
                p.sig.is_some(),
                p.disposition.is_delegated(),
                "{}: sig presence must match delegation",
                p.full_name()
            );
        }
    }

    /// Every entry earns its place with a sentence. An entry with no reason is
    /// an entry nobody can audit.
    #[test]
    fn every_entry_states_why() {
        for p in PAL_IMPORTS {
            assert!(p.why.len() > 20, "{} has no usable rationale", p.full_name());
        }
    }
}
