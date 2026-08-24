//! Shared guest-execution contracts, module identities and mutable-state data.

#![forbid(unsafe_code)]

use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;

pub mod bindings;
pub mod budget;
pub mod snapshot;
pub mod template;

pub use bindings::{HostBindings, ImportCall};
pub use budget::{CpuBudget, DispatchOutcome, TrapInfo};
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template};

pub type Result<T> = anyhow::Result<T>;

/// Synchronous guest execution and mutable-state capture contract.
pub trait InstanceOps {
    /// Runs one dispatch under the supplied execution budget.
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Delivers one completed host future under the supplied execution budget.
    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Requests guest garbage collection with the supplied pressure hint.
    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome;
    /// Restores the reusable template state, or returns an error.
    fn reset(&mut self) -> Result<()>;
    /// Reports whether the instance is parked with no active guest call.
    fn is_parked(&self) -> bool;
    /// Mutable linear-memory bytes for the instance.
    fn memory(&mut self) -> &mut [u8];
    /// Current linear-memory size in bytes.
    fn memory_len(&self) -> usize;
    /// Captures globals and table metadata; memory bytes are captured separately.
    fn capture_mutable_state(&mut self) -> Result<InstanceImage>;
    /// Restores captured mutable-state metadata, or returns an error.
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()>;
}

/// Dispatch completion category used for instance lifecycle decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Guest returned done.
    Done,
    /// Guest returned pending.
    Parked,
    /// Dispatch ended in a trap.
    Trapped,
    /// Dispatch exceeded its CPU budget.
    BudgetKill,
}

impl Completion {
    pub fn from_status(s: PollStatus) -> Self {
        match s {
            PollStatus::Done { .. } => Completion::Done,
            PollStatus::Pending => Completion::Parked,
            PollStatus::Trapped { .. } => Completion::Trapped,
        }
    }
    /// Whether this completion category permits instance reuse.
    pub fn instance_survives(self) -> bool {
        matches!(self, Completion::Done | Completion::Parked)
    }
}
