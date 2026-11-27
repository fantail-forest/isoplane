use super::{ResourceKind, SocketBuffer};
use crate::imports::{Sig, Ty};

/// Outbound socket operations, authorized by the host for every connection.
pub const SOCKET_IMPORT: &str = "socket_call";

/// Readiness and coarse deadline operations; cannot create connections.
pub const POLL_IMPORT: &str = "poll_call";

/// Canonical adapter operation identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationId(pub u32);

/// One of a socket's TCP keep-alive settings, as `wasi:sockets/tcp` divides them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAliveSetting {
    /// Whether keep-alive is enabled (SO_KEEPALIVE).
    Enabled,
    /// Idle time before the first probe (TCP_KEEPIDLE).
    IdleTime,
    /// Time between unanswered probes (TCP_KEEPINTVL).
    Interval,
    /// Unanswered probes that end the connection (TCP_KEEPCNT).
    Count,
}

/// The host operation or explicit refusal implemented for a canonical entry.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// Read or set the host socket's TCP_NODELAY option.
    NoDelay,
    /// Read one of the host socket's TCP keep-alive settings.
    KeepAlive(KeepAliveSetting),
    /// Change one of the host socket's TCP keep-alive settings.
    SetKeepAlive(KeepAliveSetting),
    /// Read the size of one of the host socket's kernel buffers.
    BufferSize(SocketBuffer),
    /// Set the size of one of the host socket's kernel buffers.
    SetBufferSize(SocketBuffer),
    /// Obtain the instance network capability.
    Network,
    /// Begin a host-owned DNS lookup.
    Resolve,
    /// Take one address from a lookup.
    NextAddress,
    /// Create an outbound TCP socket.
    CreateTcp,
    /// Begin connecting a TCP socket.
    Connect,
    /// Obtain the connected socket's streams.
    FinishConnect,
    /// Read currently available stream bytes.
    Read,
    /// Send bytes without a host queue, returning the actual accepted count
    /// or a specific network error code on failure.
    TrySendWithErrorCode,
    /// Return an owned error's diagnostic message.
    ErrorMessage,
    /// Return an owned error's network error code.
    ErrorCode,
    /// Read a connected endpoint.
    SocketAddress {
        /// Select the remote endpoint.
        peer: bool,
    },
    /// Close socket directions without closing the other direction.
    Shutdown,
    /// Subscribe to resource readiness.
    Subscribe,
    /// A relative or absolute coarse deadline.
    Timer {
        /// Whether the supplied timestamp is absolute.
        absolute: bool,
    },
    /// Inspect a list of pollables without blocking.
    Poll,
    /// Inspect a single pollable without blocking.
    Ready,
    /// Drop the named resource.
    Drop(ResourceKind),
    /// Return a canonical error for a declared unsupported operation.
    Reject {
        /// Canonical alignment of the result payload.
        error_offset: usize,
        /// Canonical error code.
        code: u8,
    },
    /// Trap when an unsupported operation has no error return channel.
    Trap,
}

/// The single definition consumed by the native wrappers, the engine adapter and the WASI table.
#[derive(Debug)]
pub struct Definition {
    /// Stable operation id in the scalar packet.
    pub id: OperationId,
    /// Native wasi-libc symbol suffix.
    pub symbol: &'static str,
    /// Isoplane capability used for this operation.
    pub capability: &'static str,
    /// Canonical signature of the wasi-libc symbol.
    pub sig: Sig,
    /// Implemented action.
    pub action: Action,
}

const fn definition(
    id: u32,
    symbol: &'static str,
    capability: &'static str,
    sig: Sig,
    action: Action,
) -> Definition {
    Definition {
        id: OperationId(id),
        symbol,
        capability,
        sig,
        action,
    }
}

/// Arms a timer that becomes ready after a duration.
pub const MONOTONIC_CLOCK_SUBSCRIBE_DURATION: Definition = definition(
    0,
    "monotonic_clock_subscribe_duration",
    POLL_IMPORT,
    Sig {
        params: &[Ty::I64],
        results: &[Ty::I32],
    },
    Action::Timer { absolute: false },
);

/// Releases a pollable.
pub const POLL_POLLABLE_DROP: Definition = definition(
    1,
    "poll_pollable_drop",
    POLL_IMPORT,
    Sig {
        params: &[Ty::I32],
        results: &[],
    },
    Action::Drop(ResourceKind::Pollable),
);

/// Reports which of a list of pollables are ready.
pub const POLL_POLL: Definition = definition(
    2,
    "poll_poll",
    POLL_IMPORT,
    Sig {
        params: &[Ty::I32, Ty::I32, Ty::I32],
        results: &[],
    },
    Action::Poll,
);

/// Every canonical entry, with no ambient network fallback.
pub const DEFINITIONS: &[Definition] = &[
    MONOTONIC_CLOCK_SUBSCRIBE_DURATION,
    POLL_POLLABLE_DROP,
    POLL_POLL,
    definition(
        3,
        "udp_udp_socket_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        4,
        "udp_incoming_datagram_stream_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        5,
        "udp_outgoing_datagram_stream_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        6,
        "tcp_tcp_socket_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop(ResourceKind::Tcp),
    ),
    definition(
        7,
        "streams_method_input_stream_read",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::Read,
    ),
    definition(
        8,
        "instance_network_instance_network",
        SOCKET_IMPORT,
        Sig {
            params: &[],
            results: &[Ty::I32],
        },
        Action::Network,
    ),
    definition(
        9,
        "udp_method_udp_socket_start_bind",
        SOCKET_IMPORT,
        Sig {
            params: &[
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
            ],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        10,
        "udp_method_udp_socket_finish_bind",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        11,
        "udp_method_udp_socket_stream",
        SOCKET_IMPORT,
        Sig {
            params: &[
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
            ],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        12,
        "udp_method_udp_socket_local_address",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        13,
        "udp_method_udp_socket_remote_address",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        14,
        "udp_method_udp_socket_unicast_hop_limit",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        15,
        "udp_method_udp_socket_set_unicast_hop_limit",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        16,
        "udp_method_udp_socket_receive_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 8,
            code: 2,
        },
    ),
    definition(
        17,
        "udp_method_udp_socket_set_receive_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        18,
        "udp_method_udp_socket_send_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 8,
            code: 2,
        },
    ),
    definition(
        19,
        "udp_method_udp_socket_set_send_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        20,
        "udp_method_udp_socket_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Trap,
    ),
    definition(
        21,
        "udp_method_incoming_datagram_stream_receive",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        22,
        "udp_method_incoming_datagram_stream_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Trap,
    ),
    definition(
        23,
        "udp_method_outgoing_datagram_stream_check_send",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        24,
        "udp_method_outgoing_datagram_stream_send",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        25,
        "udp_method_outgoing_datagram_stream_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Trap,
    ),
    definition(
        26,
        "udp_create_socket_create_udp_socket",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 2,
        },
    ),
    definition(
        27,
        "tcp_method_tcp_socket_start_bind",
        SOCKET_IMPORT,
        Sig {
            params: &[
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
            ],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        28,
        "tcp_method_tcp_socket_finish_bind",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        29,
        "tcp_method_tcp_socket_start_connect",
        SOCKET_IMPORT,
        Sig {
            params: &[
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
                Ty::I32,
            ],
            results: &[],
        },
        Action::Connect,
    ),
    definition(
        30,
        "tcp_method_tcp_socket_finish_connect",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::FinishConnect,
    ),
    definition(
        31,
        "tcp_method_tcp_socket_start_listen",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        32,
        "tcp_method_tcp_socket_finish_listen",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        33,
        "tcp_method_tcp_socket_accept",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 4,
            code: 1,
        },
    ),
    definition(
        34,
        "tcp_method_tcp_socket_local_address",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::SocketAddress { peer: false },
    ),
    definition(
        35,
        "tcp_method_tcp_socket_remote_address",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::SocketAddress { peer: true },
    ),
    definition(
        36,
        "tcp_method_tcp_socket_is_listening",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Trap,
    ),
    definition(
        37,
        "tcp_method_tcp_socket_set_listen_backlog_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 1,
        },
    ),
    definition(
        38,
        "tcp_method_tcp_socket_keep_alive_enabled",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::KeepAlive(KeepAliveSetting::Enabled),
    ),
    definition(
        39,
        "tcp_method_tcp_socket_set_keep_alive_enabled",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::SetKeepAlive(KeepAliveSetting::Enabled),
    ),
    definition(
        40,
        "tcp_method_tcp_socket_keep_alive_idle_time",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::KeepAlive(KeepAliveSetting::IdleTime),
    ),
    definition(
        41,
        "tcp_method_tcp_socket_set_keep_alive_idle_time",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::SetKeepAlive(KeepAliveSetting::IdleTime),
    ),
    definition(
        42,
        "tcp_method_tcp_socket_keep_alive_interval",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::KeepAlive(KeepAliveSetting::Interval),
    ),
    definition(
        43,
        "tcp_method_tcp_socket_set_keep_alive_interval",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::SetKeepAlive(KeepAliveSetting::Interval),
    ),
    definition(
        44,
        "tcp_method_tcp_socket_keep_alive_count",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::KeepAlive(KeepAliveSetting::Count),
    ),
    definition(
        45,
        "tcp_method_tcp_socket_set_keep_alive_count",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::SetKeepAlive(KeepAliveSetting::Count),
    ),
    definition(
        46,
        "tcp_method_tcp_socket_hop_limit",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        47,
        "tcp_method_tcp_socket_set_hop_limit",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Reject {
            error_offset: 1,
            code: 2,
        },
    ),
    definition(
        48,
        "tcp_method_tcp_socket_receive_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::BufferSize(SocketBuffer::Receive),
    ),
    definition(
        49,
        "tcp_method_tcp_socket_set_receive_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::SetBufferSize(SocketBuffer::Receive),
    ),
    definition(
        50,
        "tcp_method_tcp_socket_send_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::BufferSize(SocketBuffer::Send),
    ),
    definition(
        51,
        "tcp_method_tcp_socket_set_send_buffer_size",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I64, Ty::I32],
            results: &[],
        },
        Action::SetBufferSize(SocketBuffer::Send),
    ),
    definition(
        52,
        "tcp_method_tcp_socket_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Subscribe,
    ),
    definition(
        53,
        "tcp_method_tcp_socket_shutdown",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Shutdown,
    ),
    definition(
        54,
        "tcp_create_socket_create_tcp_socket",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::CreateTcp,
    ),
    definition(
        57,
        "monotonic_clock_subscribe_instant",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I64],
            results: &[Ty::I32],
        },
        Action::Timer { absolute: true },
    ),
    definition(
        58,
        "poll_method_pollable_ready",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Ready,
    ),
    definition(
        59,
        "poll_method_pollable_block",
        POLL_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        60,
        "streams_input_stream_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop(ResourceKind::Input),
    ),
    definition(
        61,
        "streams_output_stream_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop(ResourceKind::Output),
    ),
    definition(
        62,
        "streams_method_input_stream_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Subscribe,
    ),
    definition(
        63,
        "streams_method_output_stream_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Subscribe,
    ),
    definition(
        64,
        "streams_method_output_stream_check_write",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        65,
        "streams_method_output_stream_write",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        66,
        "streams_method_output_stream_flush",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Trap,
    ),
    definition(
        67,
        "io_error_error_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop(ResourceKind::Error),
    ),
    definition(
        68,
        "io_error_method_error_to_debug_string",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::ErrorMessage,
    ),
    definition(
        70,
        "ip_name_lookup_resolve_addresses",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32],
            results: &[],
        },
        Action::Resolve,
    ),
    definition(
        71,
        "ip_name_lookup_method_resolve_address_stream_resolve_next_address",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::NextAddress,
    ),
    definition(
        72,
        "ip_name_lookup_method_resolve_address_stream_subscribe",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[Ty::I32],
        },
        Action::Subscribe,
    ),
    definition(
        73,
        "ip_name_lookup_resolve_address_stream_drop",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32],
            results: &[],
        },
        Action::Drop(ResourceKind::Addresses),
    ),
    definition(
        74,
        "isoplane_socket_no_delay",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[Ty::I32],
        },
        Action::NoDelay,
    ),
    definition(
        75,
        "network_network_error_code",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32],
            results: &[],
        },
        Action::ErrorCode,
    ),
    definition(
        76,
        "isoplane_socket_try_send_with_error_code",
        SOCKET_IMPORT,
        Sig {
            params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32],
            results: &[Ty::I64],
        },
        Action::TrySendWithErrorCode,
    ),
];

/// A native send should retry after output readiness.
pub const SEND_PENDING: u64 = u64::MAX;
/// A native send failed without accepting bytes. The send bridge encodes a
/// failure as `SEND_FAILED` less its network error code, see [`send_failure`].
pub const SEND_FAILED: u64 = u64::MAX - 1;
/// How many network error codes the failure range below `SEND_FAILED` holds.
pub const SEND_FAILURE_CODES: u64 = 64;

/// The send result reporting a failure with `code`: the wrapper recovers the
/// code and sets the errno libc maps it to.
pub const fn send_failure(code: super::network_error::NetworkErrorCode) -> u64 {
    SEND_FAILED - code as u64
}

/// Maximum number of scalar arguments accepted by a canonical packet.
pub const MAX_ARGUMENTS: usize = {
    let mut maximum = 0;
    let mut index = 0;
    while index < DEFINITIONS.len() {
        if DEFINITIONS[index].sig.params.len() > maximum {
            maximum = DEFINITIONS[index].sig.params.len();
        }
        index += 1;
    }
    maximum
};

/// Generate native wrappers from the signatures the engine's packet dispatch checks.
pub fn native_wrappers() -> String {
    let mut output = String::from("/* Generated from isoplane_abi::native_io::canonical. */\n");
    output.push_str(&format!("#define ISOPLANE_SEND_PENDING UINT64_C({SEND_PENDING})\n#define ISOPLANE_SEND_FAILED UINT64_C({SEND_FAILED})\n"));
    output.push_str(&format!("#define ISOPLANE_SEND_IS_FAILURE(result) ((uint64_t)(result) <= ISOPLANE_SEND_FAILED && (uint64_t)(result) > ISOPLANE_SEND_FAILED - UINT64_C({SEND_FAILURE_CODES}))\n"));
    output.push_str("#define ISOPLANE_SEND_FAILURE_CODE(result) ((uint8_t)(ISOPLANE_SEND_FAILED - (uint64_t)(result)))\n");
    for capability in [SOCKET_IMPORT, POLL_IMPORT] {
        output.push_str(&format!("__attribute__((import_module(\"isoplane\"), import_name(\"{capability}\")))\nextern int64_t isoplane_{capability}(int32_t operation, const uint64_t *arguments, int32_t count);\n"));
    }
    for definition in DEFINITIONS {
        let signature = definition.sig;
        let parameters: Vec<_> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| format!("{} a{index}", c_type(*ty)))
            .collect();
        let arguments: Vec<_> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                format!(
                    "({})a{index}",
                    match ty {
                        Ty::I32 => "uint32_t",
                        Ty::I64 => "uint64_t",
                    }
                )
            })
            .collect();
        let result = signature
            .results
            .first()
            .copied()
            .map(c_type)
            .unwrap_or("void");
        let returned = if signature.results.is_empty() {
            "".to_string()
        } else {
            format!("return ({result})")
        };
        let capture = if matches!(definition.action, Action::Poll) {
            "isoplane_capture_pollables((const uint32_t *)(uintptr_t)a0, (uint32_t)a1); "
        } else if matches!(definition.action, Action::Drop(ResourceKind::Pollable)) {
            "isoplane_forget_pollable((uint32_t)a0); "
        } else {
            ""
        };
        let completed = if matches!(definition.action, Action::CreateTcp) {
            " isoplane_capture_socket((const uint8_t *)(uintptr_t)a1);"
        } else {
            ""
        };
        output.push_str(&format!("{result} __wasm_import_{}({}) {{ const uint64_t args[] = {{ {} }}; {capture}{returned}isoplane_{}({}, args, {});{completed} }}\n",
            definition.symbol, if parameters.is_empty() { "void".to_string() } else { parameters.join(", ") },
            if arguments.is_empty() { "0".to_string() } else { arguments.join(", ") }, definition.capability, definition.id.0, parameters.len()));
    }
    output
}

fn c_type(ty: Ty) -> &'static str {
    match ty {
        Ty::I32 => "int32_t",
        Ty::I64 => "int64_t",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_wrappers_match_the_canonical_definition() {
        assert_eq!(
            super::native_wrappers(),
            include_str!("../../../../guest-sdks/dotnet/native/isoplane_native_io.h"),
            "regenerate the native I/O header from its ABI example"
        );
    }
}
