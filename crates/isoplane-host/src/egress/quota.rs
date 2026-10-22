//! Open outbound connections, counted per instance, so that one instance
//! cannot hold more host sockets than its limit, whether it is running,
//! parked or hibernated.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::{ConnectionOwner, DenyReason};
use crate::InstanceId;

/// Requested per-instance outbound connection limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionCeilings {
    /// Connections one instance may have in progress before further attempts wait.
    pub in_progress: u64,
    /// Connections one instance may hold open before further connections are refused.
    pub open: u64,
}

/// One of the limits in [`ConnectionCeilings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionLimit {
    /// [`ConnectionCeilings::in_progress`].
    InProgress,
    /// [`ConnectionCeilings::open`].
    Open,
}

impl std::fmt::Display for ConnectionLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InProgress => "instance in-progress connection limit",
            Self::Open => "instance open connection limit",
        })
    }
}

/// Per-instance limits that are each at least one, with no more connections
/// in progress than may be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionLimits(ConnectionCeilings);

impl ConnectionLimits {
    /// Validates `ceilings`, reporting every limit that is out of range.
    ///
    /// A connection in progress already holds its open permit, so an
    /// in-progress limit above the open limit could never be reached.
    pub fn new(ceilings: ConnectionCeilings) -> Result<Self, ConnectionLimitsError> {
        let mut problems = Vec::new();
        for (limit, value) in [
            (ConnectionLimit::InProgress, ceilings.in_progress),
            (ConnectionLimit::Open, ceilings.open),
        ] {
            if value == 0 {
                problems.push(ConnectionLimitProblem::Zero { limit });
            }
        }
        if ceilings.in_progress > ceilings.open {
            problems.push(ConnectionLimitProblem::InProgressAboveOpen {
                in_progress: ceilings.in_progress,
                open: ceilings.open,
            });
        }
        let queue_maximum = super::attempts::MAX_PENDING;
        if ceilings.in_progress > queue_maximum {
            problems.push(ConnectionLimitProblem::AboveMaximum {
                limit: ConnectionLimit::InProgress,
                value: ceilings.in_progress,
                maximum: queue_maximum,
            });
        }
        if problems.is_empty() {
            Ok(Self(ceilings))
        } else {
            Err(ConnectionLimitsError { problems })
        }
    }

    /// The same value for both limits, so that attempts never wait before the
    /// open limit refuses them.
    pub fn uniform(connections: u64) -> Result<Self, ConnectionLimitsError> {
        Self::new(ConnectionCeilings {
            in_progress: connections,
            open: connections,
        })
    }

    /// The validated limits.
    pub fn ceilings(&self) -> ConnectionCeilings {
        self.0
    }
}

/// Connection limits that cannot be enforced as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionLimitsError {
    /// Every out-of-range limit.
    pub problems: Vec<ConnectionLimitProblem>,
}

impl std::error::Error for ConnectionLimitsError {}

impl std::fmt::Display for ConnectionLimitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, problem) in self.problems.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{problem}")?;
        }
        Ok(())
    }
}

/// One out-of-range connection limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionLimitProblem {
    /// Zero would refuse or stall every outbound connection.
    #[error("the {limit} must be at least 1")]
    Zero { limit: ConnectionLimit },
    /// An in-progress limit the open limit would always reach first.
    #[error(
        "the {} of {in_progress} exceeds the {} of {open}",
        ConnectionLimit::InProgress,
        ConnectionLimit::Open
    )]
    InProgressAboveOpen { in_progress: u64, open: u64 },
    /// A limit larger than the queue can represent.
    #[error("the {limit} of {value} exceeds the supported maximum of {maximum}")]
    AboveMaximum {
        limit: ConnectionLimit,
        value: u64,
        maximum: u64,
    },
}

/// Open connections of each instance that holds at least one.
pub(super) struct OpenConnections {
    limit: u64,
    instances: Mutex<HashMap<InstanceId, u64>>,
}

/// The instance one open connection is counted against.
pub(super) struct Holding {
    instance: InstanceId,
}

impl OpenConnections {
    pub(super) fn new(limits: ConnectionLimits) -> Self {
        Self {
            limit: limits.ceilings().open,
            instances: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn acquire(&self, owner: &ConnectionOwner) -> Result<Holding, DenyReason> {
        let mut instances = self.instances.lock();
        let open = instances.get(&owner.instance()).copied().unwrap_or(0);
        let limit = owner
            .open_limit
            .map_or(self.limit, |limit| limit.min(self.limit));
        if open >= limit {
            return Err(DenyReason::OpenConnectionLimit {
                tenant: owner.tenant().to_owned(),
                instance: owner.instance(),
                limit,
            });
        }
        instances.insert(owner.instance(), open + 1);
        Ok(Holding {
            instance: owner.instance(),
        })
    }

    pub(super) fn release(&self, holding: &Holding) {
        let mut instances = self.instances.lock();
        // Dropping the entry at zero keeps the map to instances with an open connection.
        if let Some(count) = instances.get_mut(&holding.instance) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                instances.remove(&holding.instance);
            }
        }
    }

    pub(super) fn instance(&self, instance: InstanceId) -> u64 {
        self.instances.lock().get(&instance).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn tracked(&self) -> usize {
        self.instances.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(instance: u64) -> ConnectionOwner {
        ConnectionOwner::new("tenant".into(), InstanceId(instance)).expect("fixture owner")
    }

    fn open(limit: u64) -> OpenConnections {
        OpenConnections::new(ConnectionLimits::uniform(limit).expect("fixture limits"))
    }

    #[test]
    fn an_instances_entry_is_removed_when_its_last_connection_is_released() {
        let open = open(2);
        let first = open.acquire(&owner(1)).expect("first connection");
        let second = open.acquire(&owner(1)).expect("second connection");
        assert_eq!(open.instances.lock().get(&InstanceId(1)), Some(&2));
        open.release(&first);
        assert_eq!(
            open.instances.lock().get(&InstanceId(1)),
            Some(&1),
            "an instance with an open connection keeps its count"
        );
        open.release(&second);
        let instances = open.instances.lock();
        assert!(
            instances.is_empty(),
            "an instance with no connection must not retain an entry: {instances:?}"
        );
    }

    #[test]
    fn limits_outside_their_range_are_all_reported() {
        assert_eq!(
            ConnectionLimits::new(ConnectionCeilings {
                in_progress: 0,
                open: 0
            })
            .expect_err("zero limits are refused")
            .problems,
            vec![
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::InProgress
                },
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::Open
                },
            ]
        );
        assert_eq!(
            ConnectionLimits::new(ConnectionCeilings {
                in_progress: 5,
                open: 4
            })
            .expect_err("an in-progress limit above the open limit is refused")
            .problems,
            vec![ConnectionLimitProblem::InProgressAboveOpen {
                in_progress: 5,
                open: 4
            }]
        );
        let huge = super::super::attempts::MAX_PENDING + 1;
        assert_eq!(
            ConnectionLimits::uniform(huge)
                .expect_err("an in-progress limit the queue cannot represent is refused")
                .problems,
            vec![ConnectionLimitProblem::AboveMaximum {
                limit: ConnectionLimit::InProgress,
                value: huge,
                maximum: huge - 1
            }]
        );
        assert_eq!(
            ConnectionLimits::new(ConnectionCeilings {
                in_progress: 4,
                open: 4
            })
            .map(|limits| limits.ceilings()),
            Ok(ConnectionCeilings {
                in_progress: 4,
                open: 4
            }),
            "limits at the edges of their range are accepted"
        );
    }
}
