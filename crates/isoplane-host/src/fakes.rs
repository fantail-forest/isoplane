//! Scripted guests and outbound HTTP doubles for host tests.

use std::collections::{VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use isoplane_abi::calls::*;
use isoplane_abi::dispatch::DispatchRecord;
use isoplane_abi::host::{PinnedTarget};
use isoplane_abi::result::*;
use isoplane_abi::status::{PollStatus, TrapCode};
use isoplane_abi::PAGE_SIZE;
use isoplane_engine::budget::{TrapInfo, TrapKind};
use isoplane_engine::{
    Completion, DispatchOutcome, HostBindings, ImportCall, InstanceImage, InstanceOps, ModuleHash,
    OccupancyBudget,
};

use crate::meters::OccupancyInterval;
use crate::pool::Activator;
use crate::ModuleId;

/// What the scripted guest does for one call into it.
#[derive(Debug, Clone)]
pub enum Step {
    Done {
        status: u16,
        body: Vec<u8>,
    },
    /// Issue imports and return `pending`.
    Park {
        imports: Vec<(&'static str, u64)>,
    },
    /// Enter cancellation grace and return to receive its wake.
    EnterGrace,
    Trap(TrapCode),
    /// Hold the worker until the test drops the release sender.
    Hold {
        entered: std::sync::mpsc::Sender<()>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    },
    /// Report a hard budget kill without running an actual busy loop.
    HardKill,
    /// Report a cooperative budget kill with prescribed overage.
    CleanKill {
        overage_ns: u64,
    },
    /// Touch `pages` pages of linear memory.
    Dirty {
        pages: usize,
    },
    /// Emit `times` log lines of `bytes` each, then return `done`.
    Log {
        bytes: usize,
        times: usize,
    },
    /// Issue `http_fetch` for `url` `times` over, then park.
    Fetch {
        url: String,
        times: usize,
    },
    /// Record the bytes returned by `entropy_fill`.
    Entropy {
        bytes: usize,
    },
    /// Return the received dispatch record as the response body.
    EchoRecord,
}

#[derive(Debug, Clone, Default)]
pub struct FakeBehaviour {
    /// One step per call into the instance; exhausted means `done` with 200.
    pub program: Vec<Step>,
    /// A deferred lazy initializer: the guest checks a flag in linear memory,
    /// sets it if unset, and reports the value it observed in the body, so a
    /// test can see whether the instance carried residue.
    pub lazy_init_probe: bool,
    /// Initial scripted memory bytes; zero selects one WebAssembly page.
    pub memory_bytes: usize,
    /// Prescribed occupancy for each ordinary slice in accounting tests.
    pub slice_occupancy: Option<std::time::Duration>,
    /// Emit the scripted final response through the streaming transport.
    pub response_streaming: bool,
}

impl FakeBehaviour {
    pub fn done() -> Self {
        Self {
            program: vec![],
            lazy_init_probe: false,
            memory_bytes: isoplane_abi::WASM_PAGE_SIZE,
            slice_occupancy: None,
            response_streaming: false,
        }
    }
    pub fn program(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            program: steps.into_iter().collect(),
            lazy_init_probe: false,
            memory_bytes: isoplane_abi::WASM_PAGE_SIZE,
            slice_occupancy: None,
            response_streaming: false,
        }
    }
    pub fn with_lazy_init_probe(mut self) -> Self {
        self.lazy_init_probe = true;
        self
    }
}

const LAZY_INIT_FLAG: usize = 0;
const LAZY_INIT_RUNS: usize = 8;

pub struct FakeInstance {
    /// Every `(future, result)` this instance was woken with, shared with the
    /// [`FakeActivator`] so a test asserts what the guest observed.
    pub wakes_seen: Arc<Mutex<Vec<(u64, ResultRecord)>>>,
    bindings: Box<dyn HostBindings>,
    template: Arc<Vec<u8>>,
    memory: Vec<u8>,
    program: VecDeque<Step>,
    behaviour: FakeBehaviour,
    image: InstanceImage,
    stack_empty: bool,
    last_parked: bool,
    /// A trap or a kill leaves arbitrary memory, never capturable.
    survived_last_call: bool,
    dirty_pages: usize,
    gc_hints: u32,
    entropy: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeInstance {
    fn run(&mut self, budget: OccupancyBudget, rec: Option<&DispatchRecord>) -> DispatchOutcome {
        self.stack_empty = false;
        let interval = OccupancyInterval::start();
        let wall = std::time::Instant::now();

        let observed_init = if self.behaviour.lazy_init_probe {
            let already = self.memory[LAZY_INIT_FLAG];
            if already == 0 {
                self.memory[LAZY_INIT_FLAG] = 1;
                let runs = u64::from_le_bytes(
                    self.memory[LAZY_INIT_RUNS..LAZY_INIT_RUNS + 8]
                        .try_into()
                        .expect("an 8-byte slice"),
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
            body: vec![observed_init],
        });

        let status = match step {
            Step::Done { status, body } => {
                let head = isoplane_abi::dispatch::ResponseHead {
                    status,
                    headers: vec![],
                    body: if self.behaviour.lazy_init_probe {
                        vec![observed_init]
                    } else {
                        body
                    },
                    streaming: self.behaviour.response_streaming,
                };
                let h = self.respond(head);
                PollStatus::Done { response: h }
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
                        break;
                    }
                }
                PollStatus::Pending
            }
            Step::EnterGrace => {
                self.bindings.enter_grace();
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
                let payload = enc(&FetchRequest {
                    method: "GET".into(),
                    url,
                    headers: vec![],
                    body: vec![],
                });
                for _ in 0..times {
                    self.bindings.call_future(ImportCall {
                        import: "http_fetch",
                        payload: &payload,
                        scalars: [0; 3],
                    });
                }
                PollStatus::Pending
            }
            Step::EchoRecord => {
                let body = rec
                    .map(|r| postcard::to_allocvec(r).unwrap_or_default())
                    .unwrap_or_default();
                let h = self.respond(isoplane_abi::dispatch::ResponseHead {
                    status: 200,
                    headers: vec![],
                    body,
                    streaming: false,
                });
                PollStatus::Done { response: h }
            }
            Step::Hold { entered, release } => {
                entered
                    .send(())
                    .expect("the hold observer remains available");
                match release
                    .lock()
                    .recv_timeout(std::time::Duration::from_secs(30))
                {
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) | Ok(()) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        panic!("test did not release held worker")
                    }
                }
                let response = self.respond(isoplane_abi::dispatch::ResponseHead {
                    status: 200,
                    headers: vec![],
                    body: vec![],
                    streaming: false,
                });
                PollStatus::Done { response }
            }
            Step::HardKill | Step::CleanKill { .. } => {
                let (cooperative, overage_ns) = if let Step::CleanKill { overage_ns } = step {
                    (true, overage_ns)
                } else {
                    (false, budget.grace_ns())
                };
                let occupancy_ns = budget
                    .budget_ns()
                    .saturating_add(overage_ns)
                    .saturating_sub(budget.spent_ns);
                if cooperative {
                    self.bindings.enter_grace();
                }
                self.stack_empty = true;
                self.last_parked = false;
                if cooperative {
                    self.survived_last_call = true;
                    return DispatchOutcome {
                        status: PollStatus::Done { response: 0 },
                        completion: Completion::CleanKill,
                        occupancy_ns,
                        occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
                        execution_limit: None,
                        wall_ns: wall.elapsed().as_nanos() as u64,
                        trap: None,
                        kill: Some(isoplane_engine::KillPhase::Clean),
                        overage_ns,
                        grace_deferrals: 0,
                    };
                }
                self.survived_last_call = false;
                return DispatchOutcome {
                    status: PollStatus::Trapped {
                        code: TrapCode::Unknown,
                    },
                    completion: Completion::BudgetKill,
                    occupancy_ns,
                    occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
                    execution_limit: None,
                    wall_ns: wall.elapsed().as_nanos() as u64,
                    trap: Some(TrapInfo {
                        kind: TrapKind::BudgetExceeded,
                        message: "occupancy budget exceeded at the B+G hard deadline; terminated \
                                  without unwinding"
                            .into(),
                    }),
                    kill: Some(isoplane_engine::KillPhase::Hard),
                    overage_ns,
                    grace_deferrals: 0,
                };
            }
        };

        self.stack_empty = true;
        let completion = Completion::from_status(status);
        self.last_parked = completion == Completion::Parked;
        self.survived_last_call = completion.instance_survives();
        DispatchOutcome {
            status,
            completion,
            occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
            execution_limit: None,
            occupancy_ns: self
                .behaviour
                .slice_occupancy
                .map_or_else(|| interval.elapsed_ns(), |value| value.as_nanos() as u64),
            wall_ns: wall.elapsed().as_nanos() as u64,
            trap: match status {
                PollStatus::Trapped { .. } => Some(TrapInfo {
                    kind: TrapKind::GuestPanic,
                    message: "fake guest trapped".into(),
                }),
                _ => None,
            },
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    /// `resp_new` then `resp_finish` through the vtable, returning the handle.
    fn respond(&mut self, head: isoplane_abi::dispatch::ResponseHead) -> u32 {
        let payload = enc(&head);
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
        h as u32
    }
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("a fixed-shape payload built here encodes")
}

/// Encode the supported request payload for a scripted import.
fn fake_payload_for(name: &'static str) -> Vec<u8> {
    match name {
        "http_fetch" => enc(&FetchRequest {
            method: "GET".into(),
            url: "http://example.com/".into(),
            headers: vec![],
            body: vec![],
        }),
        _ => Vec::new(),
    }
}

impl InstanceOps for FakeInstance {
    fn dispatch(&mut self, rec: &DispatchRecord, budget: OccupancyBudget) -> DispatchOutcome {
        self.run(budget, Some(rec))
    }
    fn wake(
        &mut self,
        future: u64,
        res: &ResultRecord,
        budget: OccupancyBudget,
    ) -> DispatchOutcome {
        self.wakes_seen.lock().push((future, res.clone()));
        self.run(budget, None)
    }
    fn gc_hint(&mut self, _pressure: u8) -> DispatchOutcome {
        // Zeroes the upper half, enough for the delta size to respond.
        self.gc_hints += 1;
        let half = self.memory.len() / 2;
        for b in &mut self.memory[half..] {
            *b = 0;
        }
        DispatchOutcome {
            status: PollStatus::Done { response: 0 },
            completion: Completion::Done,
            occupancy_ns: 0,
            occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
            execution_limit: None,
            wall_ns: 0,
            trap: None,
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }
    fn reset(&mut self) -> anyhow::Result<()> {
        self.memory.clear();
        self.memory.extend_from_slice(&self.template);
        self.memory.resize(self.behaviour.memory_bytes, 0);
        self.program = self.behaviour.program.clone().into();
        self.image = InstanceImage::default();
        self.dirty_pages = 0;
        Ok(())
    }
    fn is_parked(&self) -> bool {
        self.stack_empty && self.last_parked
    }
    fn is_capture_eligible(&self) -> bool {
        self.stack_empty && self.survived_last_call
    }
    fn memory(&mut self) -> &mut [u8] {
        &mut self.memory
    }
    fn memory_len(&self) -> usize {
        self.memory.len()
    }
    fn storage_failed(&self) -> bool {
        false
    }
    fn capture_mutable_state(&mut self) -> anyhow::Result<InstanceImage> {
        Ok(InstanceImage {
            globals: self.image.globals.clone(),
            table_mutations: self.image.table_mutations.clone(),
            memory_pages: (self.memory.len() / isoplane_abi::WASM_PAGE_SIZE) as u32,
        })
    }
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> anyhow::Result<()> {
        let memory_bytes = (img.memory_pages as usize)
            .checked_mul(isoplane_abi::WASM_PAGE_SIZE)
            .ok_or_else(|| anyhow::anyhow!("scripted snapshot memory size overflow"))?;
        anyhow::ensure!(
            memory_bytes >= self.template.len(),
            "snapshot memory is smaller than the template"
        );
        if let Some(limit_bytes) = self.bindings.memory_limit_bytes() {
            anyhow::ensure!(
                memory_bytes as u64 <= limit_bytes,
                "snapshot memory exceeds the instance limit"
            );
        }
        self.memory.resize(memory_bytes, 0);
        self.image = img.clone();
        Ok(())
    }
}

/// A fake template whose memory image is a deterministic pattern, so a page
/// diff means something.
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
            isoplane_abi::WASM_PAGE_SIZE
        } else {
            behaviour.memory_bytes
        };
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
        isoplane_abi::ABI_REV
    }
    fn template_image(&self) -> Arc<isoplane_engine::MemoryImage> {
        Arc::new(isoplane_engine::MemoryImage::from_dense(&self.template))
    }
    fn snapshot_size_bound(&self, _max_memory_bytes: u64) -> anyhow::Result<u64> {
        Ok(crate::hibernate_capture::snapshot_size_bound(
            crate::hibernate_capture::SnapshotShape {
                max_memory_bytes: self.behaviour.memory_bytes as u64,
                mutable_globals: 0,
            },
        )?)
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
            last_parked: false,
            survived_last_call: true,
            dirty_pages: 0,
            gc_hints: 0,
        }))
    }
}





/// Records the address it was told to connect to.
#[derive(Default)]
pub struct FakeFetcher {
    pub connected_to: Mutex<Vec<std::net::SocketAddr>>,
}

#[async_trait]
impl crate::bindings::Fetcher for FakeFetcher {
    async fn fetch(
        &self,
        _attempt: crate::egress::ConnectAttempt,
        target: PinnedTarget,
        _req: FetchRequest,
    ) -> Result<crate::bindings::OutboundResponse, FutureError> {
        self.connected_to.lock().push(target.addr);
        Ok(crate::bindings::OutboundResponse {
            status: 200,
            headers: vec![],
            body: crate::bindings::OutboundBody::Inline(b"ok".to_vec()),
        })
    }
}

/// A [`crate::bindings::WakeSink`] that records, for tests without a cell.
#[derive(Default)]
pub struct RecordingWake {
    pub signalled: Mutex<Vec<crate::InstanceId>>,
}

impl crate::bindings::WakeSink for RecordingWake {
    fn signal(&self, owner: crate::InstanceId) {
        self.signalled.lock().push(owner);
    }
}
