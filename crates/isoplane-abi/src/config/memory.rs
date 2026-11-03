//! Validated per-application linear-memory ceilings.
use serde::{Deserialize, Serialize};

/// Largest addressable wasm32 linear memory.
pub const WASM32_MEMORY_MAX_BYTES: u64 =
    include!(concat!(env!("OUT_DIR"), "/wasm32_memory_max_bytes.rs"));

/// A positive, page-aligned wasm32 memory ceiling in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct GuestMemoryBytes(u64);

/// A memory ceiling outside the platform's addressable, page-aligned range.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("guest memory must be a positive multiple of {page_bytes} bytes, at most {maximum_bytes} bytes",
    page_bytes = crate::WASM_PAGE_SIZE,
    maximum_bytes = WASM32_MEMORY_MAX_BYTES)]
pub struct InvalidGuestMemory;

impl Default for GuestMemoryBytes {
    fn default() -> Self {
        Self(512 << 20)
    }
}

impl GuestMemoryBytes {
    /// The complete wasm32 address range, independent of an application's allowance.
    pub const WASM32_MAX: Self = Self(WASM32_MEMORY_MAX_BYTES);

    /// Validate an operator-selected linear-memory ceiling.
    pub fn new(bytes: u64) -> Result<Self, InvalidGuestMemory> {
        if bytes == 0
            || bytes > WASM32_MEMORY_MAX_BYTES
            || !bytes.is_multiple_of(crate::WASM_PAGE_SIZE as u64)
        {
            return Err(InvalidGuestMemory);
        }
        Ok(Self(bytes))
    }
    /// The validated ceiling in bytes.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The linear memory an instance whose memory limit is `limit_bytes` may grow
/// to before the host stops it: a quarter more, rounded up to a whole page.
/// Past the limit itself the instance takes no new dispatches and is retired
/// once those it has finish, so the margin lets them finish rather than fail.
pub fn memory_ceiling_bytes(limit_bytes: u64) -> u64 {
    let page = crate::WASM_PAGE_SIZE as u64;
    let ceiling = limit_bytes.saturating_add(limit_bytes / 4);
    ceiling
        .div_ceil(page)
        .saturating_mul(page)
        .min(WASM32_MEMORY_MAX_BYTES)
}

impl TryFrom<u64> for GuestMemoryBytes {
    type Error = InvalidGuestMemory;
    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}
impl From<GuestMemoryBytes> for u64 {
    fn from(value: GuestMemoryBytes) -> Self {
        value.get()
    }
}
impl std::str::FromStr for GuestMemoryBytes {
    type Err = InvalidGuestMemory;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.parse().map_err(|_| InvalidGuestMemory)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_budgets_reject_unaddressable_or_unaligned_values() {
        for bytes in [0, 1, 65537, WASM32_MEMORY_MAX_BYTES + 65536] {
            assert!(
                GuestMemoryBytes::new(bytes).is_err(),
                "invalid memory ceiling {bytes} must be refused"
            );
            let encoded = postcard::to_allocvec(&bytes).expect("serialize candidate budget");
            assert!(
                postcard::from_bytes::<GuestMemoryBytes>(&encoded).is_err(),
                "serialized input cannot bypass memory validation"
            );
        }
    }

    #[test]
    fn an_instance_may_grow_a_quarter_past_its_memory_limit() {
        for (limit_mib, ceiling_mib) in [(128, 160), (512, 640)] {
            assert_eq!(memory_ceiling_bytes(limit_mib << 20), ceiling_mib << 20);
        }
        assert_eq!(
            memory_ceiling_bytes(65_536 * 3),
            65_536 * 4,
            "the ceiling rounds up to a whole page"
        );
        assert_eq!(
            memory_ceiling_bytes(WASM32_MEMORY_MAX_BYTES),
            WASM32_MEMORY_MAX_BYTES,
            "the ceiling never passes the wasm32 address range"
        );
    }

    #[test]
    fn page_aligned_memory_budgets_round_trip_without_changing_the_ceiling() {
        for bytes in [65536, 536870912, WASM32_MEMORY_MAX_BYTES] {
            let budget = GuestMemoryBytes::new(bytes).expect("valid budget");
            let encoded = postcard::to_allocvec(&budget).expect("encode budget");
            assert_eq!(
                postcard::from_bytes::<GuestMemoryBytes>(&encoded)
                    .expect("decode budget")
                    .get(),
                bytes,
                "stored memory authorization must preserve its exact ceiling"
            );
        }
    }
}
