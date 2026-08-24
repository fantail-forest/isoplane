//! Lowering an import's core-wasm parameters onto [`crate::ImportCall`].
//!
//! **Where the buffer is, and which way it flows, is not decided here.** It is
//! `wisp_abi::imports::BufSpec` on each `ImportDef`, alongside `sensitive` and
//! `init_visible`, because the validator, the linker and the guest SDK all key
//! off it and `docs/plan.md` ground rule 2 is explicit that duplicating
//! contract data is a real bug. This module only does the part that is genuinely
//! the engine's: turning the remaining parameters into the fixed-size scalar
//! array the binding vtable takes.
//!
//! (The engine did carry its own copy of that table briefly, before `BufSpec`
//! existed. It does not now, and the difference showed up immediately: adding
//! `init_config_get` to `IMPORTS` was a one-line contract change instead of a
//! contract change plus a silently-stale engine table.)

use wisp_abi::imports::{BufSpec, ImportDef};

/// Non-buffer parameters, in declaration order, widened to `u64`. This is what
/// lands in [`crate::ImportCall::scalars`].
pub(crate) fn scalars_of(def: &ImportDef, raw: &[u64]) -> [u64; 3] {
    let skip = ptr_index(def);
    let mut out = [0u64; 3];
    let mut n = 0;
    for (i, v) in raw.iter().enumerate() {
        if let Some(p) = skip {
            if i == p || i == p + 1 {
                continue;
            }
        }
        if n < out.len() {
            out[n] = *v;
            n += 1;
        }
    }
    out
}

/// Index of the `ptr` parameter, if this import takes a buffer at all. `len`
/// (or `cap`, for an out-buffer) is always the next parameter.
pub(crate) fn ptr_index(def: &ImportDef) -> Option<usize> {
    match def.buf {
        BufSpec::None => None,
        BufSpec::In(i) | BufSpec::Out(i) => Some(i as usize),
    }
}

pub(crate) fn is_out(def: &ImportDef) -> bool {
    matches!(def.buf, BufSpec::Out(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::imports::{lookup, Ty, IMPORTS};

    #[test]
    fn scalars_skip_the_buffer_pair() {
        // log_emit(level, ptr, len) -> the level is the only scalar.
        assert_eq!(scalars_of(lookup("log_emit").unwrap(), &[3, 1024, 16]), [3, 0, 0]);
        // di_alarm_set(ptr, len, at_ms) -> the deadline is the only scalar.
        assert_eq!(scalars_of(lookup("di_alarm_set").unwrap(), &[1024, 4, 999]), [999, 0, 0]);
        // http_body_read(handle, max) -> BUF_TO_FUTURE's shape, but no buffer;
        // both parameters are scalars.
        assert_eq!(scalars_of(lookup("http_body_read").unwrap(), &[7, 65536]), [7, 65536, 0]);
    }

    /// `wisp-abi` has its own test that every `BufSpec` agrees with its
    /// signature. This one checks the property the *engine* depends on: that
    /// the declared pair really is two `i32`s it can read as `(ptr, len)`, so
    /// the linker never has to guess and `IMPORTS` can grow without the engine
    /// silently mis-parsing an argument list.
    #[test]
    fn every_declared_buffer_is_a_readable_i32_pair() {
        for def in IMPORTS {
            if let Some(i) = ptr_index(def) {
                let p = def.sig.params;
                assert!(
                    p.len() >= i + 2 && p[i] == Ty::I32 && p[i + 1] == Ty::I32,
                    "{}: BufSpec points at param {i} but the signature is {:?}",
                    def.name,
                    p
                );
            }
        }
    }
}
