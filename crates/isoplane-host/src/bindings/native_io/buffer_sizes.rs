//! Kernel buffer sizes on a guest's outbound socket: the bounds a request is
//! clamped to, and the record the host applies to its own socket.

use isoplane_abi::native_io::{ByteCount, Error, SocketBuffer};
use tokio::net::TcpStream;

/// A smaller buffer moves a tenant's bytes in smaller segments, each costing
/// the host a wake and the peer a segment. This bounds the requested size;
/// the kernel can resize its actual buffer as traffic flows.
const MIN_BYTES: u32 = 4_096;

/// Linux cuts a request to `net.core.rmem_max` or `wmem_max` without
/// reporting it. Both were 212,992 on stock kernels until they were raised to
/// 4 MiB, so a request up to this is one every stock host's socket really
/// gets, and the host holds a guest to it where the sysctls are higher. A
/// buffer the guest leaves unset is sized by the kernel itself. This ceiling
/// bounds requests, not kernel memory: TCP autotuning can resize the buffer.
const MAX_BYTES: u32 = 212_992;

/// What a guest reads for a buffer it has not sized. The host leaves that
/// buffer to its kernel, which on Linux resizes it as the connection runs,
/// and before the connection there is no host socket to ask. 64 KiB is what
/// Windows gives a TCP socket, and code that sizes its own reads from this
/// figure gets a moderate one on every host OS.
const UNSET_BYTES: u32 = 65_536;

/// The sizes a guest has asked for, clamped, as the host applies them to
/// one socket.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Settings {
    receive_bytes: Option<u32>,
    send_bytes: Option<u32>,
}

impl Settings {
    /// These settings with `buffer` sized to `requested`, clamped to what
    /// the host allows. Fails on zero, which `wasi:sockets` defines as
    /// invalid.
    pub(super) fn with(self, buffer: SocketBuffer, requested: ByteCount) -> Result<Self, Error> {
        if requested.0 == 0 {
            return Err(Error::InvalidArgument("socket buffer size is zero"));
        }
        let bytes = Some(
            u32::try_from(requested.0)
                .unwrap_or(u32::MAX)
                .clamp(MIN_BYTES, MAX_BYTES),
        );
        Ok(match buffer {
            SocketBuffer::Receive => Self {
                receive_bytes: bytes,
                ..self
            },
            SocketBuffer::Send => Self {
                send_bytes: bytes,
                ..self
            },
        })
    }

    /// The size of `buffer` as a guest reads it back: the clamped request,
    /// not the kernel's figure, which Linux doubles to count its bookkeeping.
    pub(super) fn effective(self, buffer: SocketBuffer) -> ByteCount {
        let bytes = match buffer {
            SocketBuffer::Receive => self.receive_bytes,
            SocketBuffer::Send => self.send_bytes,
        };
        ByteCount(u64::from(bytes.unwrap_or(UNSET_BYTES)))
    }

    /// Puts the sizes the guest has set on the host's socket.
    pub(super) fn apply(self, stream: &TcpStream) -> std::io::Result<()> {
        let socket = socket2::SockRef::from(stream);
        if let Some(bytes) = self.receive_bytes {
            socket.set_recv_buffer_size(bytes as usize)?;
        }
        if let Some(bytes) = self.send_bytes {
            socket.set_send_buffer_size(bytes as usize)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUFFERS: [SocketBuffer; 2] = [SocketBuffer::Receive, SocketBuffer::Send];

    #[test]
    fn a_requested_size_is_clamped_to_the_host_bounds() {
        for buffer in BUFFERS {
            for (requested, effective) in [
                (1, MIN_BYTES),
                (MIN_BYTES - 1, MIN_BYTES),
                (MIN_BYTES, MIN_BYTES),
                (MIN_BYTES + 1, MIN_BYTES + 1),
                (65_537, 65_537),
                (MAX_BYTES, MAX_BYTES),
                (MAX_BYTES + 1, MAX_BYTES),
            ] {
                let settings = Settings::default()
                    .with(buffer, ByteCount(u64::from(requested)))
                    .unwrap_or_else(|error| panic!("{requested} must be accepted: {error}"));
                assert_eq!(
                    settings.effective(buffer),
                    ByteCount(u64::from(effective)),
                    "{buffer:?} buffer requested as {requested} bytes"
                );
            }
            let largest = Settings::default()
                .with(buffer, ByteCount(u64::MAX))
                .expect("the largest request must be accepted");
            assert_eq!(
                largest.effective(buffer),
                ByteCount(u64::from(MAX_BYTES)),
                "{buffer:?} buffer requested as u64::MAX bytes"
            );
        }
    }

    #[test]
    fn a_zero_size_is_refused_as_an_invalid_argument() {
        for buffer in BUFFERS {
            assert!(
                matches!(
                    Settings::default().with(buffer, ByteCount(0)),
                    Err(Error::InvalidArgument(_))
                ),
                "a zero {buffer:?} buffer must be refused"
            );
        }
    }

    #[test]
    fn sizing_one_buffer_leaves_the_other_unset() {
        let settings = Settings::default()
            .with(SocketBuffer::Receive, ByteCount(32_768))
            .expect("the size is within bounds");
        assert_eq!(
            (
                settings.effective(SocketBuffer::Receive),
                settings.effective(SocketBuffer::Send)
            ),
            (ByteCount(32_768), ByteCount(u64::from(UNSET_BYTES)))
        );
        assert_eq!(
            settings.send_bytes, None,
            "the send buffer stays the kernel's"
        );
    }
}
