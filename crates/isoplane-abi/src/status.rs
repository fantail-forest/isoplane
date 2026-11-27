/// What a guest entrypoint returns, packed into one `u64` by
/// [`PollStatus::encode`]: the low 32 bits hold the variant's tag and the high
/// 32 bits its payload, which is the response handle for `Done`, the trap code
/// for `Trapped` and zero otherwise.
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

/// Why a guest trapped, as the payload of [`PollStatus::Trapped`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    strum::EnumMessage,
    strum::FromRepr,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
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
}

impl TrapCode {
    /// The code a payload names. One no variant has reads as `Unknown`.
    pub fn from_u32(v: u32) -> Self {
        Self::from_repr(v).unwrap_or(Self::Unknown)
    }
}

/// The bit of a packed [`PollStatus`] at which its payload starts. The bits
/// below it hold the [`PollTag`].
pub const PAYLOAD_SHIFT: u32 = 32;

/// The bits of a packed [`PollStatus`] below [`PAYLOAD_SHIFT`], naming its
/// variant.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    strum::EnumMessage,
    strum::FromRepr,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[repr(u32)]
pub enum PollTag {
    Done = 0,
    Pending = 1,
    Trapped = 2,
    Cancelled = 3,
    Failed = 4,
}

impl PollStatus {
    pub fn encode(self) -> u64 {
        let (tag, payload) = match self {
            PollStatus::Done { response } => (PollTag::Done, response),
            PollStatus::Pending => (PollTag::Pending, 0),
            PollStatus::Cancelled => (PollTag::Cancelled, 0),
            PollStatus::Failed => (PollTag::Failed, 0),
            PollStatus::Trapped { code } => (PollTag::Trapped, code as u32),
        };
        (tag as u64) | ((payload as u64) << PAYLOAD_SHIFT)
    }

    /// Reads the word [`PollStatus::encode`] writes. An unknown tag decodes as
    /// `Trapped`.
    pub fn decode(v: u64) -> Self {
        let tag = (v & ((1 << PAYLOAD_SHIFT) - 1)) as u32;
        let payload = (v >> PAYLOAD_SHIFT) as u32;
        match PollTag::from_repr(tag) {
            Some(PollTag::Done) => PollStatus::Done { response: payload },
            Some(PollTag::Pending) => PollStatus::Pending,
            Some(PollTag::Cancelled) => PollStatus::Cancelled,
            Some(PollTag::Failed) => PollStatus::Failed,
            Some(PollTag::Trapped) | None => PollStatus::Trapped {
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
