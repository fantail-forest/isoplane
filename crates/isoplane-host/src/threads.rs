//! CPU-based starting allocations; these are thread counts, not CPU reservations.

/// Allocation across guest execution and the two asynchronous runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadAllocation {
    /// Synchronous guest execution workers.
    pub execution: usize,
    /// Guest I/O and wake-source workers.
    pub reactor: usize,
    /// Incoming HTTP and proxy workers.
    pub outer: usize,
}

impl ThreadAllocation {
    /// Local development uses 75% execution and 25% for each async runtime.
    pub fn local() -> Self {
        Self::for_cpus(available_cpus(), true)
    }

    /// Provisional production allocation: 75% execution, 25% shared by async pools.
    pub fn production() -> Self {
        Self::for_cpus(available_cpus(), false)
    }

    fn for_cpus(cpus: usize, local: bool) -> Self {
        let async_share = (cpus / 4).max(1);
        let execution = cpus.saturating_sub(async_share).max(1);
        let (reactor, outer) = if local {
            (async_share, async_share)
        } else {
            (
                (async_share - async_share / 2).max(1),
                (async_share / 2).max(1),
            )
        };
        Self {
            execution,
            reactor,
            outer,
        }
    }
}

fn available_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_pools_round_toward_execution_and_keep_each_pool_alive() {
        for (cpus, execution, asynchronous) in [
            (1, 1, 1),
            (2, 1, 1),
            (3, 2, 1),
            (4, 3, 1),
            (7, 6, 1),
            (8, 6, 2),
            (16, 12, 4),
        ] {
            assert_eq!(
                ThreadAllocation::for_cpus(cpus, true),
                ThreadAllocation {
                    execution,
                    reactor: asynchronous,
                    outer: asynchronous
                },
                "local allocation for {cpus} CPUs must favor execution and retain both I/O pools"
            );
        }
    }

    #[test]
    fn production_shares_the_async_allocation_between_both_runtimes() {
        for (cpus, execution, reactor, outer) in
            [(1, 1, 1, 1), (8, 6, 1, 1), (12, 9, 2, 1), (16, 12, 2, 2)]
        {
            assert_eq!(
                ThreadAllocation::for_cpus(cpus, false),
                ThreadAllocation {
                    execution,
                    reactor,
                    outer
                },
                "production allocation for {cpus} CPUs must share the async budget"
            );
        }
    }
}
