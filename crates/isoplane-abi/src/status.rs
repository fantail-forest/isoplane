//! The poll status a guest entrypoint returns, packed into one `u64` so that
//! `isoplane_handle` and `isoplane_wake` need no out-parameter:
//!
//! ```text
//!   bits  0..32   tag      0 = done, 1 = pending, 2 = trapped, 3 = cancelled, 4 = failed
//!   bits 32..64   payload  done -> response handle (0 = no response)
//!                          trapped -> trap code
//! ```

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollStatus {
    /// Dispatch complete. Carries the host response handle, or zero for no response.
    Done { response: u32 },
    /// Guest returned with an empty invocation stack to await a host-owned wake.
    /// Host-owned I/O and other resources may remain live.
    Pending,
    /// The dispatch cooperatively cancelled. The instance remains reusable.
    Cancelled,
    /// An application exception failed this dispatch; the instance remains reusable.
    Failed,
    /// Guest fault. The instance is discarded.
    Trapped { code: TrapCode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TrapCode {
    Unknown = 0,
    Panic = 1,
    OutOfMemory = 2,
    /// A capability the module did not declare, or that the init sandbox
    /// forbids.
    CapabilityViolation = 3,
    /// Malformed dispatch or result encoding.
    Protocol = 4,
    /// The pending-futures cap was exceeded.
    TooManyFutures = 5,
}

impl TrapCode {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Panic,
            2 => Self::OutOfMemory,
            3 => Self::CapabilityViolation,
            4 => Self::Protocol,
            5 => Self::TooManyFutures,
            _ => Self::Unknown,
        }
    }
}

const TAG_DONE: u32 = 0;
const TAG_PENDING: u32 = 1;
const TAG_TRAPPED: u32 = 2;
const TAG_CANCELLED: u32 = 3;
const TAG_FAILED: u32 = 4;

impl PollStatus {
    pub fn encode(self) -> u64 {
        let (tag, payload) = match self {
            PollStatus::Done { response } => (TAG_DONE, response),
            PollStatus::Pending => (TAG_PENDING, 0),
            PollStatus::Cancelled => (TAG_CANCELLED, 0),
            PollStatus::Failed => (TAG_FAILED, 0),
            PollStatus::Trapped { code } => (TAG_TRAPPED, code as u32),
        };
        (tag as u64) | ((payload as u64) << 32)
    }

    pub fn decode(v: u64) -> Self {
        let tag = (v & 0xFFFF_FFFF) as u32;
        let payload = (v >> 32) as u32;
        match tag {
            TAG_DONE => PollStatus::Done { response: payload },
            TAG_PENDING => PollStatus::Pending,
            TAG_CANCELLED => PollStatus::Cancelled,
            TAG_FAILED => PollStatus::Failed,
            _ => PollStatus::Trapped {
                code: TrapCode::from_u32(payload),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_survives_a_round_trip() {
        for s in [
            PollStatus::Done { response: 0 },
            PollStatus::Done {
                response: 0xDEAD_BEEF,
            },
            PollStatus::Pending,
            PollStatus::Cancelled,
            PollStatus::Failed,
            PollStatus::Trapped {
                code: TrapCode::CapabilityViolation,
            },
        ] {
            assert_eq!(
                PollStatus::decode(s.encode()),
                s,
                "the packed status must decode to what was encoded"
            );
        }
    }
}
