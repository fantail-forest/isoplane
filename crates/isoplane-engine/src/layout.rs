//! Lowering an import's core-wasm parameters onto [`crate::ImportCall`].
//! Where the buffer is comes from `BufSpec` on the import definition; this
//! module only packs the remaining parameters into the scalar array.

use isoplane_abi::imports::{BufSpec, ImportDef};

/// Non-buffer parameters, in declaration order, widened to `u64`.
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

/// Index of the `ptr` parameter, if this import takes a buffer. `len` (or
/// `cap`) is always the next parameter.
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
    use isoplane_abi::imports::{lookup, Ty, IMPORTS};

    fn def(name: &str) -> &'static ImportDef {
        lookup(name).expect("the import is in the table")
    }

    #[test]
    fn scalars_skip_the_buffer_pair() {
        assert_eq!(
            scalars_of(def("log_emit"), &[3, 1024, 16]),
            [3, 0, 0],
            "log_emit(level, ptr, len): the level is the only scalar"
        );
        assert_eq!(
            scalars_of(def("di_alarm_set"), &[1024, 4, 999]),
            [999, 0, 0],
            "di_alarm_set(ptr, len, at_ms): the deadline is the only scalar"
        );
        assert_eq!(
            scalars_of(def("http_body_read"), &[7, 65536]),
            [7, 65536, 0],
            "http_body_read(handle, max) carries no buffer, so both are scalars"
        );
    }

    #[test]
    fn every_declared_buffer_is_a_readable_i32_pair() {
        for def in IMPORTS {
            if let Some(i) = ptr_index(def) {
                let p = def.sig.params;
                assert!(
                    p.len() >= i + 2 && p[i] == Ty::I32 && p[i + 1] == Ty::I32,
                    "{}: BufSpec points at param {i} but the signature is {:?}, so the engine \
                     would read a scalar as a pointer",
                    def.name,
                    p
                );
            }
        }
    }
}
