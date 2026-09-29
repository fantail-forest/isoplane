//! Typed async wrappers over the raw capability imports.
//!
//! Every binding here is a thin shape over `abi::*`. The pattern is always the
//! same and is worth stating once: **encode, call, wrap the returned id, and
//! return.** No wrapper waits for anything; the awaiting happens in the app's
//! state machine and resolves to a `return Poll::Pending` out of the guest.


use isoplane_abi::calls::*;
use isoplane_abi::result::{BodyChunk, FetchResponse};

use crate::abi;
use crate::error::{IsoplaneError, Result};
use crate::future::{call, decode_postcard, decode_unit, HostFuture};



// ---------------------------------------------------------------------------
// outbound HTTP
// ---------------------------------------------------------------------------

pub struct HttpFetch;

impl HttpFetch {
    /// SSRF policy is enforced host-side; an RFC1918 target comes back as
    /// `IsoplaneError::Denied`.
    pub fn fetch(req: FetchRequest) -> HostFuture<FetchResponse> {
        call(abi::http_fetch, &req, decode_postcard::<FetchResponse>)
    }
    pub fn get(url: &str) -> HostFuture<FetchResponse> {
        HttpFetch::fetch(FetchRequest {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        })
    }
    pub fn post(url: &str, body: Vec<u8>, content_type: &str) -> HostFuture<FetchResponse> {
        HttpFetch::fetch(FetchRequest {
            method: "POST".into(),
            url: url.into(),
            headers: vec![("content-type".into(), content_type.into())],
            body,
        })
    }
}

/// Reads a response body through host-owned futures.
pub struct BodyReader {
    handle: u32,
}

impl BodyReader {
    pub fn new(handle: u32) -> BodyReader {
        BodyReader { handle }
    }
    pub fn read(&self, max: u32) -> HostFuture<BodyChunk> {
        let id = unsafe { abi::http_body_read(self.handle as i32, max as i32) } as u64;
        HostFuture::new(id, decode_postcard::<BodyChunk>)
    }
    /// Reads until EOF and propagates host I/O or cancellation errors.
    pub async fn read_to_end(&self, chunk: u32) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let c = self.read(chunk).await?;
            out.extend_from_slice(&c.data);
            if c.eof {
                return Ok(out);
            }
        }
    }
}

/// Fetches a body, using inline bytes when available and streaming otherwise.
pub async fn fetch_bytes(url: &str) -> Result<(u16, Vec<u8>)> {
    let r = HttpFetch::get(url).await?;
    if let Some(b) = r.body_inline {
        return Ok((r.status, b));
    }
    let body = BodyReader::new(r.body_handle)
        .read_to_end(64 * 1024)
        .await?;
    Ok((r.status, body))
}

// ---------------------------------------------------------------------------
// clock & entropy
// ---------------------------------------------------------------------------

pub struct Clock;

impl Clock {
    /// Millisecond granularity, and FROZEN for the dispatch slice. Do not
    /// build a timer out of this: it does not advance inside a slice, so a spin
    /// loop on it can never terminate -- it just burns the CPU budget and dies
    /// (see `guests/misbehaver`).
    pub fn now_ms() -> u64 {
        unsafe { abi::clock_now_coarse() as u64 }
    }
    /// Returns a host timer future; awaiting it returns control to the host.
    pub fn delay(ms: u64) -> HostFuture<()> {
        let id = unsafe { abi::clock_deadline_in(ms as i64) } as u64;
        HostFuture::new(id, decode_unit)
    }
}

pub struct Entropy;

impl Entropy {
    /// Requests bytes through the host entropy interface; captured values remain captured.
    pub fn fill(buf: &mut [u8]) {
        unsafe { abi::entropy_fill(buf.as_mut_ptr() as i32, buf.len() as i32) };
    }
    pub fn u64() -> u64 {
        let mut b = [0u8; 8];
        Entropy::fill(&mut b);
        u64::from_le_bytes(b)
    }
}

// ---------------------------------------------------------------------------
// observability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

/// CONTRACT GAP: `isoplane-abi` defines no log record type, so the payload here is
/// the raw UTF-8 message. See guests/README.md.
pub fn log(level: Level, msg: &str) {
    let b = msg.as_bytes();
    unsafe { abi::log_emit(level as i32, b.as_ptr() as i32, b.len() as i32) };
}

/// RAII span. `end` runs on drop -- which is fine for hygiene but is NOT a
/// correctness mechanism: a dispatch that runs past the hard deadline at B+G
/// is trapped mid-frame with no unwinding, so this drop may simply never
/// happen. The host closes spans on instance death.
pub struct Span(i32);

impl Span {
    pub fn begin(name: &str) -> Span {
        let b = name.as_bytes();
        Span(unsafe { abi::trace_span_begin(b.as_ptr() as i32, b.len() as i32) })
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        unsafe { abi::trace_span_end(self.0) };
    }
}

/// Turn a `Denied` into something an app can branch on without string
/// matching in three places.
pub fn is_denied(e: &IsoplaneError) -> bool {
    matches!(e, IsoplaneError::Denied(_))
}
