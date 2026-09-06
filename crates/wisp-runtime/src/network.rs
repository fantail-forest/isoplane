//! Network binding limits shared by the daemon and local guest execution.

/// Limits used when constructing the host's egress policy and HTTP fetcher.
pub struct NetworkLimits {
    /// Maximum simultaneous outbound connections.
    pub connections: u64,
    /// Maximum bytes admitted by the egress policy.
    pub egress_bytes: u64,
    /// Response bodies at or below this size are returned inline.
    pub inline_response_bytes: usize,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            connections: 64,
            egress_bytes: 64 << 20,
            inline_response_bytes: 1 << 20,
        }
    }
}
