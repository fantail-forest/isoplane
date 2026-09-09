use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use wisp_engine::{HostBindings, ImportCall};

/// Records what the guest asked for, and hands back a coarse clock that
/// advances on every read, so a frozen-clock test is testing the engine's
/// freeze rather than a host that returned the same number twice.
#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Arc<parking_lot::Mutex<Vec<String>>>,
    pub responses: Arc<parking_lot::Mutex<Vec<wisp_abi::dispatch::ResponseHead>>>,
    pub deny_entropy: Arc<AtomicBool>,
    pub entropy_draws: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
    pub clock: Arc<AtomicU64>,
    pub clock_reads: Arc<AtomicU64>,
}

pub struct TestBindings {
    pub rec: Recorder,
    pub init_sandbox: bool,
    pub next_future: u64,
    pub fill_byte: u8,
    /// When false, `call_fill` denies, which is the default `HostBindings`
    /// behaviour.
    pub allow_fill: bool,
    /// What `call_fill` reports as the value's full length, which for
    /// `init_config_get` may exceed the capacity the guest offered.
    pub full_length: Option<i64>,
}

impl TestBindings {
    pub fn new(rec: Recorder) -> Self {
        Self {
            rec,
            init_sandbox: false,
            next_future: 1,
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
    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        self.note(&call);
        let id = self.next_future;
        self.next_future += 1;
        id
    }

    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        self.note(&call);
        if call.import == "resp_new" {
            self.rec
                .responses
                .lock()
                .push(wisp_abi::decode(call.payload).expect("the guest response must decode"));
        }
        0
    }

    fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> i64 {
        self.note(&call);
        if !self.allow_fill
            || (call.import == "entropy_fill" && self.rec.deny_entropy.load(Ordering::Relaxed))
        {
            return -1;
        }
        // A fake CSPRNG that answered the same bytes every time could not tell
        // a per-call draw from a cached one.
        out.fill(self.fill_byte);
        self.fill_byte = self.fill_byte.wrapping_add(1);
        if call.import == "entropy_fill" {
            self.rec.entropy_draws.lock().push(out.to_vec());
        }
        self.full_length.unwrap_or(out.len() as i64)
    }

    fn slice_clock_ms(&self) -> u64 {
        self.rec.clock_reads.fetch_add(1, Ordering::Relaxed);
        self.rec.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_init_sandbox(&self) -> bool {
        self.init_sandbox
    }
}
