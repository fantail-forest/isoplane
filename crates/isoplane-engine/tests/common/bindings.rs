use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_engine::{HostBindings, ImportCall};

/// Records what the guest asked for, and hands back a coarse clock that
/// advances on every read, so a frozen-clock test is testing the engine's
/// freeze rather than a host that returned the same number twice.
#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Arc<parking_lot::Mutex<Vec<String>>>,
    pub runtime_configuration: Arc<parking_lot::Mutex<Option<Vec<u8>>>>,
    pub responses: Arc<parking_lot::Mutex<Vec<isoplane_abi::dispatch::ResponseHead>>>,
    pub response_writes: Arc<parking_lot::Mutex<std::collections::VecDeque<(u64, u32)>>>,
    pub ready_slices: Arc<parking_lot::Mutex<std::collections::VecDeque<(u64, u64)>>>,
    pub reject_next_response: Arc<AtomicBool>,
    pub deny_entropy: Arc<AtomicBool>,
    pub entropy_draws: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
    pub clock: Arc<AtomicU64>,
    pub clock_reads: Arc<AtomicU64>,
    pub upload: Arc<parking_lot::Mutex<Upload>>,
}

/// The current request's body and the guest's unanswered reads of it.
#[derive(Default)]
pub struct Upload {
    /// `None` when the request's framing permits no body.
    pub remaining: Option<std::collections::VecDeque<u8>>,
    /// The future and size limit of each unanswered `req_body_read`.
    pub reads: std::collections::VecDeque<(u64, usize)>,
}

pub struct TestBindings {
    pub rec: Recorder,
    pub memory_limit_bytes: Option<u64>,
    pub next_future: u64,
    pub(crate) next_response: i64,
    pub fill_byte: u8,
    /// When false, `call_fill` denies, which is the default `HostBindings`
    /// behaviour.
    pub allow_fill: bool,
    /// What `call_fill` reports in place of the length of the buffer it filled.
    pub full_length: Option<i64>,
}

impl TestBindings {
    pub fn new(rec: Recorder) -> Self {
        Self {
            rec,
            memory_limit_bytes: None,
            next_future: 1,
            next_response: 1,
            fill_byte: 0xAB,
            allow_fill: true,
            full_length: None,
        }
    }
    fn note(&self, call: &ImportCall<'_>) {
        self.rec
            .calls
            .lock()
            .push(format!("{}({} bytes)", call.import, call.payload.len()));
    }
}

impl HostBindings for TestBindings {
    fn begin_slice(&mut self) {}
    fn end_slice(&mut self) {}
    fn memory_limit_bytes(&self) -> Option<u64> {
        self.memory_limit_bytes
    }

    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        self.note(&call);
        if call.import == "dispatch_ready" && call.scalars[0] == u64::MAX {
            return 0;
        }
        let id = self.next_future;
        self.next_future += 1;
        if call.import == "dispatch_ready" {
            self.rec
                .ready_slices
                .lock()
                .push_back((call.scalars[0], id));
        }
        if call.import == "req_body_read" {
            self.rec
                .upload
                .lock()
                .reads
                .push_back((id, call.scalars[0] as usize));
        }
        if call.import == "resp_write" {
            self.rec
                .responses
                .lock()
                .last_mut()
                .expect("a response write must follow response creation")
                .body
                .extend_from_slice(call.payload);
            self.rec
                .response_writes
                .lock()
                .push_back((id, call.payload.len() as u32));
        }
        id
    }

    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        self.note(&call);
        if call.import == "resp_new" {
            if self.rec.reject_next_response.swap(false, Ordering::Relaxed) {
                return 0;
            }
            self.rec
                .responses
                .lock()
                .push(postcard::from_bytes(call.payload).expect("the guest response must decode"));
            let handle = self.next_response;
            self.next_response += 1;
            return handle;
        }
        if call.import == "req_body_can_have_body" {
            return i64::from(self.rec.upload.lock().remaining.is_some());
        }
        0
    }

    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        out: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        self.note(&call);
        if !self.allow_fill
            || (call.import == "entropy_fill" && self.rec.deny_entropy.load(Ordering::Relaxed))
        {
            return Err(isoplane_engine::bindings::BindingError::Unsupported(
                call.import,
            ));
        }
        if call.import == "runtime_config_get" {
            let settings = self.rec.runtime_configuration.lock();
            let bytes = settings.as_deref().unwrap_or(b"{}");
            let length = bytes.len().min(out.len());
            out[..length].copy_from_slice(&bytes[..length]);
            return Ok(bytes.len() as i64);
        }
        // A fake CSPRNG that answered the same bytes every time could not tell
        // a per-call draw from a cached one.
        out.fill(self.fill_byte);
        self.fill_byte = self.fill_byte.wrapping_add(1);
        if call.import == "entropy_fill" {
            self.rec.entropy_draws.lock().push(out.to_vec());
        }
        Ok(self.full_length.unwrap_or(out.len() as i64))
    }

    fn slice_clock_ms(&self) -> u64 {
        self.rec.clock_reads.fetch_add(1, Ordering::Relaxed);
        self.rec.clock.fetch_add(1, Ordering::Relaxed) + 1
    }
}
