//! Ephemeral host entropy shared by preparation and live guest draws.

use std::cell::RefCell;
use std::io;

use rand::{RngCore, SeedableRng, TryRngCore};
use rand_chacha::ChaCha12Rng;

const RESEED_STREAM_BYTES: usize = 64 * 1024;

/// A secure entropy draw failed before its output could be returned to the guest.
#[derive(Debug, thiserror::Error)]
pub enum EntropyError {
    /// The operating system refused initial seeding or reseeding.
    #[error("seeding host entropy from the operating system: {0}")]
    OperatingSystem(#[source] io::Error),
    /// The current thread cannot acquire its generator.
    #[error("the current thread's host entropy generator is unavailable")]
    Unavailable,
}

#[derive(Default)]
struct Generator {
    rng: Option<ChaCha12Rng>,
    process_id: u32,
    consumed_bytes: usize,
}

thread_local! {
    static GENERATOR: RefCell<Generator> = RefCell::new(Generator::default());
}

/// Fills `output` from a thread-owned ChaCha12 generator, seeded by the OS initially,
/// after consuming 64 KiB of its stream, and before any output in a forked process.
/// Stream consumption includes bytes discarded by partial-word draws.
/// Generator state is never serialized or included in guest captures. On error,
/// the caller must discard the buffer; an earlier portion may have been filled.
pub fn fill(output: &mut [u8]) -> Result<(), EntropyError> {
    GENERATOR
        .try_with(|generator| {
            let mut generator = generator
                .try_borrow_mut()
                .map_err(|_| EntropyError::Unavailable)?;
            generator
                .fill(std::process::id(), output, &mut os_seed)
                .map_err(EntropyError::OperatingSystem)
        })
        .map_err(|_| EntropyError::Unavailable)?
}

fn os_seed() -> io::Result<[u8; 32]> {
    let mut seed = [0; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut seed)
        .map_err(io::Error::other)?;
    Ok(seed)
}

impl Generator {
    fn fill(
        &mut self,
        process_id: u32,
        mut output: &mut [u8],
        seed: &mut impl FnMut() -> io::Result<[u8; 32]>,
    ) -> io::Result<()> {
        if self.process_id != process_id {
            self.rng = None;
            self.consumed_bytes = 0;
            self.process_id = process_id;
        }
        while !output.is_empty() {
            if self.consumed_bytes == RESEED_STREAM_BYTES {
                self.rng = None;
            }
            let rng = match &mut self.rng {
                Some(rng) => rng,
                absent @ None => {
                    let rng = absent.insert(ChaCha12Rng::from_seed(seed()?));
                    self.consumed_bytes = 0;
                    rng
                }
            };
            let count = output.len().min(RESEED_STREAM_BYTES - self.consumed_bytes);
            let (chunk, remaining) = output.split_at_mut(count);
            rng.fill_bytes(chunk);
            self.consumed_bytes =
                usize::try_from(rng.get_word_pos() * 4).map_err(io::Error::other)?;
            output = remaining;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_output_uses_chacha12_seeded_by_the_selected_os_source() {
        let mut generator = Generator::default();
        let mut output = [0; 32];
        generator.fill(1, &mut output, &mut || Ok([7; 32])).unwrap();
        let mut expected = [0; 32];
        ChaCha12Rng::from_seed([7; 32]).fill_bytes(&mut expected);
        assert_eq!(output, expected);
    }

    #[test]
    fn reseeding_happens_before_stream_consumption_exceeds_each_64_kib_interval() {
        let mut generator = Generator::default();
        let mut calls = 0;
        let mut seed = || {
            calls += 1;
            Ok([calls; 32])
        };
        let mut output = vec![0; 2 * RESEED_STREAM_BYTES + 1];
        generator.fill(1, &mut output, &mut seed).unwrap();
        assert_eq!(calls, 3);
        for (index, chunk) in output.chunks(RESEED_STREAM_BYTES).enumerate() {
            let mut expected = vec![0; chunk.len()];
            ChaCha12Rng::from_seed([index as u8 + 1; 32]).fill_bytes(&mut expected);
            assert_eq!(chunk, expected);
        }
    }

    #[test]
    fn partial_word_draws_charge_stream_consumption_and_an_empty_draw_does_not_reseed() {
        let mut generator = Generator::default();
        let mut calls = 0;
        for _ in 0..RESEED_STREAM_BYTES / 4 {
            generator
                .fill(1, &mut [0], &mut || {
                    calls += 1;
                    Ok([calls; 32])
                })
                .unwrap();
        }
        assert_eq!(calls, 1);
        generator
            .fill(1, &mut [], &mut || panic!("an empty draw needs no seed"))
            .unwrap();
        generator
            .fill(1, &mut [0], &mut || {
                calls += 1;
                Ok([calls; 32])
            })
            .unwrap();
        assert_eq!(calls, 2);
    }

    #[test]
    fn initial_seeding_failure_is_returned_without_a_fallback_stream() {
        let mut generator = Generator::default();
        let mut output = [9; 32];
        let error = generator
            .fill(1, &mut output, &mut || {
                Err(io::Error::other("entropy refused"))
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(output, [9; 32]);
        assert!(generator.rng.is_none());
    }

    #[test]
    fn failed_reseeding_refuses_further_output_until_the_os_supplies_a_new_seed() {
        let mut generator = Generator::default();
        generator
            .fill(1, &mut vec![0; RESEED_STREAM_BYTES], &mut || Ok([1; 32]))
            .unwrap();
        let mut output = [9; 32];
        assert!(generator
            .fill(1, &mut output, &mut || Err(io::Error::other(
                "entropy refused"
            )))
            .is_err());
        assert_eq!(output, [9; 32]);
        generator.fill(1, &mut output, &mut || Ok([2; 32])).unwrap();
        let mut expected = [0; 32];
        ChaCha12Rng::from_seed([2; 32]).fill_bytes(&mut expected);
        assert_eq!(output, expected);
    }

    #[test]
    fn a_changed_process_discards_inherited_state_before_any_output() {
        let mut generator = Generator::default();
        generator
            .fill(1, &mut [0; 16], &mut || Ok([1; 32]))
            .unwrap();
        let mut output = [9; 32];
        assert!(generator
            .fill(2, &mut output, &mut || Err(io::Error::other(
                "entropy refused"
            )))
            .is_err());
        assert_eq!(output, [9; 32]);
        generator.fill(2, &mut output, &mut || Ok([2; 32])).unwrap();
        let mut expected = [0; 32];
        ChaCha12Rng::from_seed([2; 32]).fill_bytes(&mut expected);
        assert_eq!(output, expected);
    }
}
