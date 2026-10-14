//! Network binding limits shared by the daemon and local guest execution.

/// Default for [`NetworkLimits::instance_pending_connections`], the number of
/// connections Cloudflare Workers lets one invocation have waiting at once.
pub const DEFAULT_INSTANCE_PENDING_CONNECTIONS: u64 = 6;

/// Default for [`NetworkLimits::instance_open_connections`].
pub const DEFAULT_INSTANCE_OPEN_CONNECTIONS: u64 = 32;

/// Limits used when constructing the host's egress policy and HTTP fetcher.
///
/// Outbound connections are limited per instance only.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkLimits {
    /// Maximum connections one instance has in progress: a socket from its connect
    /// until it is established or fails, a host HTTP fetch until its response headers
    /// arrive. Further attempts wait in order. At least 1 and at most
    /// `instance_open_connections`.
    pub instance_pending_connections: u64,
    /// Maximum outbound connections one instance holds open, each counted from when
    /// it leaves the wait for `instance_pending_connections` until its host socket is
    /// released, including while the instance is hibernated. Further connections are
    /// refused. At least 1.
    pub instance_open_connections: u64,
    /// Response bodies at or below this size are returned inline.
    pub inline_response_bytes: usize,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            instance_pending_connections: DEFAULT_INSTANCE_PENDING_CONNECTIONS,
            instance_open_connections: DEFAULT_INSTANCE_OPEN_CONNECTIONS,
            inline_response_bytes: 1 << 20,
        }
    }
}
