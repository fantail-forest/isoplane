//! Open outbound connections, counted per tenant and per node, so that one
//! tenant cannot exhaust every other tenant's ability to connect out.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::{ConnectionOwner, DenyReason};

/// A level at which open outbound connections are counted and refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionLevel {
    /// The connections of every instance of one tenant.
    Tenant,
    /// Every connection on the node.
    Node,
}

impl std::fmt::Display for ConnectionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tenant => "tenant",
            Self::Node => "node",
        })
    }
}

/// Requested outbound connection limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionCeilings {
    /// Open connections on the node.
    pub node: u64,
    /// Open connections of one tenant's instances together.
    pub tenant: u64,
    /// Connections one instance may have in progress before further attempts queue.
    pub instance_pending: u64,
}

/// One of the limits in [`ConnectionCeilings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionLimit {
    /// [`ConnectionCeilings::node`].
    Node,
    /// [`ConnectionCeilings::tenant`].
    Tenant,
    /// [`ConnectionCeilings::instance_pending`].
    InstancePending,
}

impl std::fmt::Display for ConnectionLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Node => "node open connection limit",
            Self::Tenant => "tenant open connection limit",
            Self::InstancePending => "instance pending connection limit",
        })
    }
}

/// Limits that are each at least one and none above the level that contains it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionLimits(ConnectionCeilings);

impl ConnectionLimits {
    /// Validates `ceilings`, reporting every limit that is out of range.
    ///
    /// A connection in progress already holds its tenant's and the node's open
    /// permits, so a pending limit above the tenant's could never be reached.
    pub fn new(ceilings: ConnectionCeilings) -> Result<Self, ConnectionLimitsError> {
        let mut problems = Vec::new();
        for (limit, value, maximum) in [
            (ConnectionLimit::Node, ceilings.node, None),
            (
                ConnectionLimit::Tenant,
                ceilings.tenant,
                Some((ConnectionLimit::Node, ceilings.node)),
            ),
            (
                ConnectionLimit::InstancePending,
                ceilings.instance_pending,
                Some((ConnectionLimit::Tenant, ceilings.tenant)),
            ),
        ] {
            match maximum {
                _ if value == 0 => problems.push(ConnectionLimitProblem::Zero { limit }),
                Some((container, maximum)) if value > maximum => {
                    problems.push(ConnectionLimitProblem::AboveContainer {
                        limit,
                        value,
                        container,
                        maximum,
                    })
                }
                Some(_) | None => {}
            }
        }
        let queue_maximum = super::attempts::MAX_PENDING;
        if ceilings.instance_pending > queue_maximum {
            problems.push(ConnectionLimitProblem::AboveMaximum {
                limit: ConnectionLimit::InstancePending,
                value: ceilings.instance_pending,
                maximum: queue_maximum,
            });
        }
        if problems.is_empty() {
            Ok(Self(ceilings))
        } else {
            Err(ConnectionLimitsError { problems })
        }
    }

    /// The same value for every limit, so that only the node's open count refuses.
    pub fn uniform(connections: u64) -> Result<Self, ConnectionLimitsError> {
        Self::new(ConnectionCeilings {
            node: connections,
            tenant: connections,
            instance_pending: connections,
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
    /// Every out-of-range limit, in node, tenant, instance order.
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
    /// A limit the containing level would always reach first.
    #[error("the {limit} of {value} exceeds the {container} of {maximum}")]
    AboveContainer {
        limit: ConnectionLimit,
        value: u64,
        container: ConnectionLimit,
        maximum: u64,
    },
    /// A limit larger than the queue can represent.
    #[error("the {limit} of {value} exceeds the supported maximum of {maximum}")]
    AboveMaximum {
        limit: ConnectionLimit,
        value: u64,
        maximum: u64,
    },
}

/// Open connections at each level, under one lock so that taking a permit at
/// every level, or at none, is one step.
pub(super) struct OpenConnections {
    node_limit: u64,
    tenant_limit: u64,
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    node: u64,
    tenants: HashMap<String, u64>,
}

/// The tenant one open connection is counted against.
pub(super) struct Holding {
    tenant: String,
}

impl OpenConnections {
    pub(super) fn new(limits: ConnectionLimits) -> Self {
        let ceilings = limits.ceilings();
        Self {
            node_limit: ceilings.node,
            tenant_limit: ceilings.tenant,
            held: Mutex::new(Held::default()),
        }
    }

    pub(super) fn acquire(&self, owner: &ConnectionOwner) -> Result<Holding, DenyReason> {
        let mut held = self.held.lock();
        let tenant = held.tenants.get(owner.tenant()).copied().unwrap_or(0);
        let full = if tenant >= self.tenant_limit {
            Some((ConnectionLevel::Tenant, self.tenant_limit))
        } else if held.node >= self.node_limit {
            Some((ConnectionLevel::Node, self.node_limit))
        } else {
            None
        };
        if let Some((level, limit)) = full {
            return Err(DenyReason::ConnectionQuota {
                level,
                tenant: owner.tenant().to_owned(),
                instance: owner.instance(),
                limit,
            });
        }
        held.node += 1;
        *held.tenants.entry(owner.tenant().to_owned()).or_default() += 1;
        Ok(Holding {
            tenant: owner.tenant().to_owned(),
        })
    }

    pub(super) fn release(&self, holding: &Holding) {
        let mut held = self.held.lock();
        held.node = held.node.saturating_sub(1);
        // Dropping the entry at zero keeps the map to tenants with a live connection.
        if let Some(count) = held.tenants.get_mut(&holding.tenant) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                held.tenants.remove(&holding.tenant);
            }
        }
    }

    pub(super) fn node(&self) -> u64 {
        self.held.lock().node
    }

    pub(super) fn tenant(&self, tenant: &str) -> u64 {
        self.held.lock().tenants.get(tenant).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstanceId;

    fn owner(tenant: &str, instance: u64) -> ConnectionOwner {
        ConnectionOwner::new(tenant.into(), InstanceId(instance)).expect("fixture tenant")
    }

    fn open(node: u64, tenant: u64) -> OpenConnections {
        OpenConnections::new(
            ConnectionLimits::new(ConnectionCeilings {
                node,
                tenant,
                instance_pending: 1,
            })
            .expect("fixture limits"),
        )
    }

    #[test]
    fn a_tenants_entry_is_removed_when_its_last_connection_is_released() {
        let open = open(4, 2);
        let first = open.acquire(&owner("a", 1)).expect("first connection");
        let second = open.acquire(&owner("a", 2)).expect("second connection");
        assert_eq!(open.held.lock().tenants.get("a"), Some(&2));
        open.release(&first);
        assert_eq!(
            open.held.lock().tenants.get("a"),
            Some(&1),
            "a tenant with a live connection keeps its count"
        );
        open.release(&second);
        let held = open.held.lock();
        assert!(
            held.tenants.is_empty(),
            "a tenant with no connection must not retain an entry: {:?}",
            held.tenants
        );
        assert_eq!(held.node, 0);
    }

    #[test]
    fn limits_outside_their_range_are_all_reported() {
        let error = ConnectionLimits::new(ConnectionCeilings {
            node: 4,
            tenant: 5,
            instance_pending: 0,
        })
        .expect_err("a tenant limit above the node's and a zero pending limit are refused");
        assert_eq!(
            error.problems,
            vec![
                ConnectionLimitProblem::AboveContainer {
                    limit: ConnectionLimit::Tenant,
                    value: 5,
                    container: ConnectionLimit::Node,
                    maximum: 4
                },
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::InstancePending
                },
            ]
        );
        assert_eq!(
            ConnectionLimits::new(ConnectionCeilings {
                node: 8,
                tenant: 4,
                instance_pending: 5
            })
            .expect_err("a pending limit above the tenant's is refused")
            .problems,
            vec![ConnectionLimitProblem::AboveContainer {
                limit: ConnectionLimit::InstancePending,
                value: 5,
                container: ConnectionLimit::Tenant,
                maximum: 4
            }]
        );
        assert_eq!(
            ConnectionLimits::uniform(0)
                .expect_err("zero limits are refused")
                .problems,
            vec![
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::Node
                },
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::Tenant
                },
                ConnectionLimitProblem::Zero {
                    limit: ConnectionLimit::InstancePending
                },
            ]
        );
        let huge = super::super::attempts::MAX_PENDING + 1;
        assert_eq!(
            ConnectionLimits::uniform(huge)
                .expect_err("a pending limit the queue cannot represent is refused")
                .problems,
            vec![ConnectionLimitProblem::AboveMaximum {
                limit: ConnectionLimit::InstancePending,
                value: huge,
                maximum: huge - 1
            }]
        );
        assert!(
            ConnectionLimits::new(ConnectionCeilings {
                node: 4,
                tenant: 4,
                instance_pending: 1
            })
            .is_ok(),
            "limits at the edges of their range are accepted"
        );
    }
}
