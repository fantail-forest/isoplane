//! How each import's core-wasm parameters lower onto [`crate::ImportCall`].
//!
//! `wisp_abi::imports::Sig` says what *types* an import takes, but not which
//! parameters are the `(ptr, len)` guest buffer or which way the bytes flow,
//! and the signatures alone are genuinely ambiguous:
//!
//! * `BUF_TO_VOID` is an **in** buffer for `abort`/`di_alarm_cancel` and an
//!   **out** buffer for `entropy_fill`/`init_hash_seed`.
//! * `log_emit(i32,i32,i32)`, `resp_write` and `socket_accept` put a scalar
//!   *before* the pair; `di_alarm_set(i32,i32,i64)` puts one *after* it.
//! * `http_body_read(i32,i32)->i64` has `BUF_TO_FUTURE`'s exact shape but is
//!   `(handle, max_len)` and carries no buffer at all.
//!
//! So the mapping has to be written down somewhere. It **ought** to be a bit on
//! `wisp_abi::ImportDef` next to `sensitive`/`init_visible`, because the
//! validator, the linker and the guest SDK all need to agree on it and
//! `docs/plan.md` ground rule 2 is explicit that duplicating contract data is a
//! real bug. Until it moves there, this table is the definition of record and
//! the engine is its only consumer. (Reported to the integrator.)

use wisp_abi::imports::{ImportDef, IMPORTS};

/// Where an import's guest buffer lives, and which way the bytes travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Buf {
    /// No `(ptr, len)` pair: every parameter is a scalar.
    None,
    /// Guest -> host. The engine copies the bytes **out** of linear memory
    /// before calling the host, so the host never holds a pointer into a memory
    /// the guest could grow underneath it (§5.5).
    In(usize),
    /// Host -> guest. The host fills a host-side scratch buffer and the engine
    /// copies it **in**. Same rule, opposite direction: still no host pointer
    /// in guest memory.
    Out(usize),
}

impl Buf {
    pub(crate) fn ptr_index(self) -> Option<usize> {
        match self {
            Buf::None => None,
            Buf::In(i) | Buf::Out(i) => Some(i),
        }
    }
    pub(crate) fn is_out(self) -> bool {
        matches!(self, Buf::Out(_))
    }
}

/// The lowering for one import name.
pub(crate) fn buf_of(name: &str) -> Buf {
    use Buf::{In, None as NoBuf, Out};
    match name {
        // ---- core ----------------------------------------------------------
        "abort" => In(0),
        "init_unit_begin" | "init_unit_deferred" => NoBuf,
        // The one externally sourced value the sealed init sandbox supplies
        // (§4.4): the host writes the escrowed per-deploy Marvin seed in.
        "init_hash_seed" => Out(0),

        // ---- observability -------------------------------------------------
        "log_emit" => In(1), // (level, ptr, len)
        "trace_span_begin" => In(0),
        "trace_span_end" => NoBuf,

        // ---- clock & entropy -----------------------------------------------
        // `clock_now_coarse` never reaches a binding at all: the frozen clock is
        // an engine-owned security control (§8.4/§8.5), not a host service.
        "clock_now_coarse" | "clock_deadline_in" => NoBuf,
        "entropy_fill" => Out(0),

        // ---- outbound http --------------------------------------------------
        "http_fetch" => In(0),
        "http_body_read" => NoBuf, // (body handle, max bytes)

        // ---- inbound response ------------------------------------------------
        "resp_new" => In(0),
        "resp_write" => In(1), // (handle, ptr, len)
        "resp_finish" => NoBuf,

        // ---- storage bindings -------------------------------------------------
        "kv_get" | "kv_put" | "kv_delete" | "kv_list" | "blob_get" | "blob_put"
        | "queue_send" | "sql_query" => In(0),

        // ---- durable instances -------------------------------------------------
        "di_call" | "di_persist" | "di_storage_get" | "di_storage_put"
        | "di_outbox_enqueue" => In(0),
        "di_alarm_set" => In(0), // (ptr, len, at_ms)
        "di_alarm_cancel" => In(0),
        "di_fencing_epoch" => NoBuf,

        // ---- sockets -----------------------------------------------------------
        "socket_accept" => In(1), // (handle, tag_ptr, tag_len)
        // The socket handle rides inside the postcard `SocketSend` payload.
        "socket_send" | "socket_broadcast" => In(0),
        "socket_close" => NoBuf,

        // ---- secrets, services, ctx ---------------------------------------------
        "secret_get" | "service_call" => In(0),
        "wait_until" => NoBuf,

        // A name reaching here is a new entry in `IMPORTS` with no lowering. The
        // test below turns that into a build failure rather than a runtime
        // surprise, because guessing would silently corrupt an argument list.
        other => unreachable!("import `{other}` has no parameter lowering; add one to layout.rs"),
    }
}

/// Every import in the table has a lowering, and every declared `(ptr, len)`
/// pair really is a pair of `i32`s at the stated index.
pub(crate) fn validate_table() -> Result<(), String> {
    use wisp_abi::imports::Ty;
    for def in IMPORTS {
        let buf = buf_of(def.name);
        if let Some(i) = buf.ptr_index() {
            let p = def.sig.params;
            if p.len() < i + 2 || p[i] != Ty::I32 || p[i + 1] != Ty::I32 {
                return Err(format!(
                    "{}: lowering claims a (ptr,len) pair at param {i} but the signature is {:?}",
                    def.name, p
                ));
            }
        }
    }
    Ok(())
}

/// Non-buffer parameters, in declaration order, widened to `u64`. This is what
/// lands in [`crate::ImportCall::scalars`].
pub(crate) fn scalars_of(def: &ImportDef, raw: &[u64]) -> [u64; 3] {
    let skip = buf_of(def.name).ptr_index();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground rule 2: the import table is consumed by three components at once,
    /// so a new import with no lowering must fail loudly here rather than
    /// quietly mis-parse an argument list in production.
    #[test]
    fn every_import_has_a_consistent_lowering() {
        validate_table().unwrap();
    }

    #[test]
    fn scalars_skip_the_buffer_pair() {
        // log_emit(level, ptr, len) -> the level is the only scalar.
        let def = wisp_abi::imports::lookup("log_emit").unwrap();
        assert_eq!(scalars_of(def, &[3, 1024, 16]), [3, 0, 0]);
        // di_alarm_set(ptr, len, at_ms) -> the deadline is the only scalar.
        let def = wisp_abi::imports::lookup("di_alarm_set").unwrap();
        assert_eq!(scalars_of(def, &[1024, 4, 999]), [999, 0, 0]);
        // http_body_read(handle, max) -> both are scalars.
        let def = wisp_abi::imports::lookup("http_body_read").unwrap();
        assert_eq!(scalars_of(def, &[7, 65536]), [7, 65536, 0]);
    }
}
