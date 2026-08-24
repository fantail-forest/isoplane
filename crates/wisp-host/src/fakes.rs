//! Scripted guests and outbound HTTP doubles for host tests.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use wisp_abi::calls::*;
use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::host::PinnedTarget;
use wisp_abi::result::*;
use wisp_abi::status::{PollStatus, TrapCode};
use wisp_abi::PAGE_SIZE;
use wisp_engine::budget::{TrapInfo, TrapKind};
use wisp_engine::{
    Completion, CpuBudget, DispatchOutcome, HostBindings, ImportCall, InstanceImage, InstanceOps,
    ModuleHash,
};

use crate::instances::Activator;
use crate::meters::CpuInterval;
use crate::ModuleId;

/// One step of a fake guest's program. Each step is what the guest does for
/// one call into it — `wisp_handle` or `wisp_wake`.
#[derive(Debug, Clone)]
pub enum Step {
    /// Return `done` with a response.
    Done { status: u16, body: Vec<u8> },
    /// Issue imports and return `pending`.
    Park { imports: Vec<(&'static str, u64)> },
    /// Return `trapped`.
    Trap(TrapCode),
    /// Burn CPU on the calling thread until the budget kills it. Real CPU, on
    /// a real pool thread, killed by a real deadline check.
    Spin,
    /// Touch `pages` pages of linear memory, so the dirty set is measurable.
    Dirty { pages: usize },
    /// Emit `times` log lines of `bytes` each, then return `done`. §10: log
    /// CPU counts against the dispatch budget, and this is what makes that
    /// measurable.
    Log { bytes: usize, times: usize },
    /// Issue `http_fetch` for `url` `times` over, then park. Exercises §8.6's
    /// egress policy and §11.1's subrequest cap through the real vtable.
    Fetch { url: String, times: usize },
    /// Record the bytes returned by `entropy_fill`.
    Entropy { bytes: usize },
}

/// The scripted behaviour of a fake module.
#[derive(Debug, Clone, Default)]
pub struct FakeBehaviour {
    /// The program for the first call into a fresh instance, then subsequent
    /// calls. Exhausting it means "return done with 200".
    pub program: Vec<Step>,
    /// Emulates a deferred lazy initializer (§4.4): the guest checks a flag in
    /// linear memory and, if unset, "initialises" and increments the counter.
    /// The observed pre-call value is written into the response body, so a
    /// test can see whether the instance carried residue.
    pub lazy_init_probe: bool,
    /// Bytes of linear memory. Kept tiny by default so tests are fast.
    pub memory_bytes: usize,
}

impl FakeBehaviour {
    pub fn done() -> Self {
        Self {
            program: vec![],
            lazy_init_probe: false,
            memory_bytes: 4 * PAGE_SIZE,
        }
    }
    pub fn program(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            program: steps.into_iter().collect(),
            lazy_init_probe: false,
            memory_bytes: 4 * PAGE_SIZE,
        }
    }
    pub fn with_lazy_init_probe(mut self) -> Self {
        self.lazy_init_probe = true;
        self
    }
    pub fn with_memory(mut self, bytes: usize) -> Self {
        self.memory_bytes = bytes;
        self
    }
}

/// Offset of the lazy-init flag in the fake guest's linear memory.
const LAZY_INIT_FLAG: usize = 0;
/// Offset of the "how many times did the lazy initializer run" counter.
const LAZY_INIT_RUNS: usize = 8;

pub struct FakeInstance {
    /// Every `(future, result)` this instance was woken with, shared with the
    /// [`FakeActivator`] so a test can assert what the *guest* observed rather
    /// than what the future table thinks it sent (§6.8's claim is about the
    /// guest's view).
    pub wakes_seen: Arc<Mutex<Vec<(u64, ResultRecord)>>>,
    bindings: Box<dyn HostBindings>,
    template: Arc<Vec<u8>>,
    memory: Vec<u8>,
    program: VecDeque<Step>,
    behaviour: FakeBehaviour,
    image: InstanceImage,
    /// True whenever no call into the guest is executing — which in a
    /// callback-CPS ABI is *always*, between calls. That is claim C4: there is
    /// no engine stack to be suspended, so a suspended instance is at a
    /// serializable point by construction.
    stack_empty: bool,
    dirty_pages: usize,
    gc_hints: u32,
    /// Every entropy draw this module's instances have made (§8.5).
    entropy: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeInstance {
    fn run(&mut self, budget: CpuBudget, rec: Option<&DispatchRecord>) -> DispatchOutcome {
        self.stack_empty = false;
        let cpu = CpuInterval::start();
        let wall = std::time::Instant::now();

        let observed_init = if self.behaviour.lazy_init_probe {
            let already = self.memory[LAZY_INIT_FLAG];
            if already == 0 {
                self.memory[LAZY_INIT_FLAG] = 1;
                let runs = u64::from_le_bytes(
                    self.memory[LAZY_INIT_RUNS..LAZY_INIT_RUNS + 8]
                        .try_into()
                        .unwrap(),
                );
                self.memory[LAZY_INIT_RUNS..LAZY_INIT_RUNS + 8]
                    .copy_from_slice(&(runs + 1).to_le_bytes());
                self.dirty_pages = self.dirty_pages.max(1);
            }
            already
        } else {
            0
        };

        let step = self.program.pop_front().unwrap_or(Step::Done {
            status: 200,
            // The probe value is the whole point: `0` means this dispatch saw
            // a pristine instance, `1` means it saw a previous request's
            // residue (§2.6).
            body: vec![observed_init],
        });

        let status = match step {
            Step::Done { status, body } => {
                let head = wisp_abi::dispatch::ResponseHead {
                    status,
                    headers: vec![],
                    body: if self.behaviour.lazy_init_probe {
                        vec![observed_init]
                    } else {
                        body
                    },
                    streaming: false,
                };
                let payload = postcard::to_allocvec(&head).unwrap();
                let h = self.bindings.call_sync(ImportCall {
                    import: "resp_new",
                    payload: &payload,
                    scalars: [0; 3],
                });
                self.bindings.call_sync(ImportCall {
                    import: "resp_finish",
                    payload: &[],
                    scalars: [h as u64, 0, 0],
                });
                PollStatus::Done { response: h as u32 }
            }
            Step::Park { imports } => {
                for (name, scalar) in imports {
                    let payload = fake_payload_for(name);
                    let fid = self.bindings.call_future(ImportCall {
                        import: name,
                        payload: &payload,
                        scalars: [scalar, 0, 0],
                    });
                    if fid == crate::futures::INVALID_FUTURE {
                        // The pending-futures cap refused us (§11.1). A real
                        // guest cannot proceed either; the cell converts the
                        // latched trap after we return.
                        break;
                    }
                }
                PollStatus::Pending
            }
            Step::Trap(code) => PollStatus::Trapped { code },
            Step::Dirty { pages } => {
                for p in 0..pages {
                    let off = p * PAGE_SIZE;
                    if off < self.memory.len() {
                        self.memory[off] = self.memory[off].wrapping_add(1);
                    }
                }
                self.dirty_pages = self.dirty_pages.max(pages);
                PollStatus::Done { response: 0 }
            }
            Step::Entropy { bytes } => {
                let mut buf = vec![0u8; bytes];
                let n = self.bindings.call_fill(
                    ImportCall {
                        import: "entropy_fill",
                        payload: &[],
                        scalars: [0; 3],
                    },
                    &mut buf,
                );
                debug_assert_eq!(n, bytes as i64);
                self.entropy.lock().push(buf);
                PollStatus::Done { response: 0 }
            }
            Step::Log { bytes, times } => {
                let line = vec![b'x'; bytes];
                for _ in 0..times {
                    self.bindings.call_sync(ImportCall {
                        import: "log_emit",
                        payload: &line,
                        scalars: [2, 0, 0],
                    });
                }
                PollStatus::Done { response: 0 }
            }
            Step::Fetch { url, times } => {
                let payload = postcard::to_allocvec(&FetchRequest {
                    method: "GET".into(),
                    url,
                    headers: vec![],
                    body: vec![],
                })
                .unwrap();
                for _ in 0..times {
                    self.bindings.call_future(ImportCall {
                        import: "http_fetch",
                        payload: &payload,
                        scalars: [0; 3],
                    });
                }
                PollStatus::Pending
            }
            Step::Spin => {
                // Real CPU on the real calling thread, killed by a real
                // deadline. §6.4's epoch counter exists to *deliver* the kill,
                // and this is the same shape: check, then terminate without
                // unwinding.
                let deadline_ns = budget.cpu_ms as u64 * 1_000_000;
                let wall_deadline =
                    std::time::Duration::from_millis(budget.wall_ms_cap.max(1) as u64);
                let mut x: u64 = 0;
                loop {
                    for i in 0..20_000u64 {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
                    }
                    std::hint::black_box(x);
                    if cpu.elapsed_ns() >= deadline_ns || wall.elapsed() >= wall_deadline {
                        break;
                    }
                }
                let cpu_ns = cpu.elapsed_ns();
                self.stack_empty = true;
                return DispatchOutcome {
                    status: PollStatus::Trapped {
                        code: TrapCode::Unknown,
                    },
                    completion: Completion::BudgetKill,
                    cpu_ns,
                    wall_ns: wall.elapsed().as_nanos() as u64,
                    trap: Some(TrapInfo {
                        kind: TrapKind::BudgetExceeded,
                        // §2.5.8: no unwinding, no finally, no cleanup quantum.
                        message: "cpu budget exceeded; terminated without unwinding".into(),
                    }),
                };
            }
        };

        let _ = rec;
        self.stack_empty = true;
        DispatchOutcome {
            status,
            completion: Completion::from_status(status),
            cpu_ns: cpu.elapsed_ns(),
            wall_ns: wall.elapsed().as_nanos() as u64,
            trap: match status {
                PollStatus::Trapped { .. } => Some(TrapInfo {
                    kind: TrapKind::GuestPanic,
                    message: "fake guest trapped".into(),
                }),
                _ => None,
            },
        }
    }

    /// How many times the deferred lazy initializer ran in this instance's
    /// memory. Under `fresh` this is 1 on every dispatch; under `reuse` it
    /// stops incrementing once the instance is warm (§2.6, §4.4).
    pub fn lazy_init_runs(&self) -> u64 {
        u64::from_le_bytes(
            self.memory[LAZY_INIT_RUNS..LAZY_INIT_RUNS + 8]
                .try_into()
                .unwrap(),
        )
    }
}

fn fake_payload_for(name: &'static str) -> Vec<u8> {
    match name {
        "http_fetch" => postcard::to_allocvec(&FetchRequest {
            method: "GET".into(),
            url: "http://example.com/".into(),
            headers: vec![],
            body: vec![],
        })
        .unwrap(),
        _ => Vec::new(),
    }
}

impl InstanceOps for FakeInstance {
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome {
        self.run(budget, Some(rec))
    }
    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome {
        self.wakes_seen.lock().push((future, res.clone()));
        self.run(budget, None)
    }
    fn gc_hint(&mut self, _pressure: u8) -> DispatchOutcome {
        self.gc_hints += 1;
        let half = self.memory.len() / 2;
        for b in &mut self.memory[half..] {
            *b = 0;
        }
        DispatchOutcome {
            status: PollStatus::Done { response: 0 },
            completion: Completion::Done,
            cpu_ns: 0,
            wall_ns: 0,
            trap: None,
        }
    }
    fn reset(&mut self) -> anyhow::Result<()> {
        // §6.2: `madvise(MADV_DONTNEED)` on the private mapping restores
        // template contents; globals/tables re-sealed.
        self.memory.clear();
        self.memory.extend_from_slice(&self.template);
        self.memory.resize(self.behaviour.memory_bytes, 0);
        self.program = self.behaviour.program.clone().into();
        self.image = InstanceImage::default();
        self.dirty_pages = 0;
        Ok(())
    }
    fn is_parked(&self) -> bool {
        self.stack_empty
    }
    fn memory(&mut self) -> &mut [u8] {
        &mut self.memory
    }
    fn memory_len(&self) -> usize {
        self.memory.len()
    }
    fn capture_mutable_state(&mut self) -> anyhow::Result<InstanceImage> {
        Ok(InstanceImage {
            globals: self.image.globals.clone(),
            table_mutations: self.image.table_mutations.clone(),
            memory_pages: (self.memory.len() / wisp_abi::WASM_PAGE_SIZE) as u32,
        })
    }
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> anyhow::Result<()> {
        self.image = img.clone();
        Ok(())
    }
}

/// A fake template. Its "pre-initialized memory image" is a deterministic
/// pattern, which is enough for a page diff to be meaningful.
pub struct FakeActivator {
    module: ModuleId,
    hash: ModuleHash,
    template: Arc<Vec<u8>>,
    behaviour: FakeBehaviour,
    pub activations: AtomicU64,
    pub wakes_seen: Arc<Mutex<Vec<(u64, ResultRecord)>>>,
    pub entropy: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeActivator {
    pub fn new(behaviour: FakeBehaviour) -> Self {
        Self::named(ModuleId::new("fake", "v1"), behaviour)
    }
    pub fn named(module: ModuleId, behaviour: FakeBehaviour) -> Self {
        let bytes = if behaviour.memory_bytes == 0 {
            4 * PAGE_SIZE
        } else {
            behaviour.memory_bytes
        };
        // A non-trivial template image, so "differs from the template" means
        // something in the page diff.
        let image: Vec<u8> = (0..bytes)
            .map(|i| ((i / PAGE_SIZE) as u8).wrapping_mul(31))
            .collect();
        let hash = ModuleHash::of(&image);
        Self {
            module,
            hash,
            template: Arc::new(image),
            behaviour: FakeBehaviour {
                memory_bytes: bytes,
                ..behaviour
            },
            activations: AtomicU64::new(0),
            wakes_seen: Arc::new(Mutex::new(Vec::new())),
            entropy: Arc::new(Mutex::new(Vec::new())),
        }
    }
    /// What every instance of this module has been woken with, in order.
    pub fn wakes(&self) -> Vec<(u64, ResultRecord)> {
        self.wakes_seen.lock().clone()
    }
    pub fn activation_count(&self) -> u64 {
        self.activations.load(Ordering::Relaxed)
    }
}

impl Activator for FakeActivator {
    fn module(&self) -> &ModuleId {
        &self.module
    }
    fn module_hash(&self) -> ModuleHash {
        self.hash
    }
    fn abi_rev(&self) -> u32 {
        wisp_abi::ABI_REV
    }
    fn template_image(&self) -> Arc<Vec<u8>> {
        self.template.clone()
    }
    fn activate(
        &self,
        bindings: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        self.activations.fetch_add(1, Ordering::Relaxed);
        let mut memory = (*self.template).clone();
        memory.resize(self.behaviour.memory_bytes, 0);
        Ok(Box::new(FakeInstance {
            entropy: self.entropy.clone(),
            wakes_seen: self.wakes_seen.clone(),
            bindings,
            template: self.template.clone(),
            memory,
            program: self.behaviour.program.clone().into(),
            behaviour: self.behaviour.clone(),
            image: InstanceImage::default(),
            stack_empty: true,
            dirty_pages: 0,
            gc_hints: 0,
        }))
    }
}

// ---------------------------------------------------------------------------
// Hibernation
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bindings targets
// ---------------------------------------------------------------------------

/// Records the address it was told to connect to, so a test can prove the
/// **pinned** address was used and the name was never re-resolved (§8.6).
#[derive(Default)]
pub struct FakeFetcher {
    pub connected_to: Mutex<Vec<std::net::SocketAddr>>,
    pub bodies: Mutex<HashMap<u32, VecDeque<Vec<u8>>>>,
    /// When set, `read_body` never resolves — an honest model of a streaming
    /// transfer that is still in flight. §6.8's Unbounded case only means
    /// anything while a transfer is outstanding, so a test that wants to
    /// observe it has to be able to keep one outstanding.
    stall: std::sync::atomic::AtomicBool,
}

impl FakeFetcher {
    pub fn stall_bodies(&self, on: bool) {
        self.stall.store(on, Ordering::Relaxed);
    }
}

#[async_trait]
impl crate::bindings::Fetcher for FakeFetcher {
    async fn fetch(
        &self,
        target: PinnedTarget,
        _req: FetchRequest,
    ) -> Result<FetchResponse, FutureError> {
        self.connected_to.lock().push(target.addr);
        Ok(FetchResponse {
            status: 200,
            headers: vec![],
            body_handle: 1,
            body_inline: Some(b"ok".to_vec()),
        })
    }
    async fn read_body(&self, handle: u32, _max: usize) -> Result<BodyChunk, FutureError> {
        if self.stall.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        let mut g = self.bodies.lock();
        let q = g.entry(handle).or_default();
        match q.pop_front() {
            Some(data) => Ok(BodyChunk {
                data,
                eof: q.is_empty(),
            }),
            None => Ok(BodyChunk {
                data: vec![],
                eof: true,
            }),
        }
    }
}

/// A [`crate::bindings::WakeSink`] that just records, for tests that do not
/// want a whole cell.
#[derive(Default)]
pub struct RecordingWake {
    pub signalled: Mutex<Vec<crate::InstanceId>>,
}

impl crate::bindings::WakeSink for RecordingWake {
    fn signal(&self, owner: crate::InstanceId) {
        self.signalled.lock().push(owner);
    }
}
