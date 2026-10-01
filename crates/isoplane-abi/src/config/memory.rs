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
