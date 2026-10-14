//! Network binding limits shared by the daemon and local guest execution.

/// Default for [`NetworkLimits::connections`].
pub const DEFAULT_CONNECTIONS: u64 = 64;

/// Default for [`NetworkLimits::tenant_connections`].
// The owner is choosing this value; it equals the node's default until then.
pub const DEFAULT_TENANT_CONNECTIONS: u64 = 64;

/// Default for [`NetworkLimits::instance_pending_connections`], the number of
/// connections Cloudflare Workers lets one invocation have waiting at once.
pub const DEFAULT_INSTANCE_PENDING_CONNECTIONS: u64 = 6;

/// Limits used when constructing the host's egress policy and HTTP fetcher.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkLimits {
    /// Maximum open outbound connections on the node; further connections are refused.
    pub connections: u64,
    /// Maximum open outbound connections of one tenant's instances together; further
    /// connections are refused. At least 1 and at most `connections`.
    pub tenant_connections: u64,
    /// Maximum connections one instance has in progress: a socket from its connect
    /// until it is established or fails, a host HTTP fetch until its response headers
    /// arrive. Further attempts wait in order. At least 1 and at most `tenant_connections`.
    pub instance_pending_connections: u64,
    /// Response bodies at or below this size are returned inline.
    pub inline_response_bytes: usize,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            connections: DEFAULT_CONNECTIONS,
            tenant_connections: DEFAULT_TENANT_CONNECTIONS,
            instance_pending_connections: DEFAULT_INSTANCE_PENDING_CONNECTIONS,
            inline_response_bytes: 1 << 20,
        }
    }
}
