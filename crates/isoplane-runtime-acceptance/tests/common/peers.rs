//! Owned TCP peers that put a connection into each state the runtime must
//! handle, and a reactor that admits exactly them.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use isoplane_harness::Reactor;
use isoplane_host::egress::{ConnectionFactory, ConnectionOwner};
use isoplane_host::InstanceId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::admitted::admitting;
use crate::no_dns::NoDns;

/// A reactor whose outbound connections may only reach `peers`, resolving
/// nothing: the guest connects by address.
pub fn reactor_for_peers(owner: u64, peers: Vec<SocketAddr>) -> Reactor {
    Reactor::with_network(InstanceId(owner), admitting(peers, Box::new(NoDns)))
}

/// As [`reactor_for_peers`], requesting a small send buffer on outbound
/// connections. Platforms may accept an individual write larger than this.
pub fn reactor_for_peers_with_send_buffer(
    owner: u64,
    peers: Vec<SocketAddr>,
    send_buffer_bytes: u32,
) -> Reactor {
    Reactor::with_network(
        InstanceId(owner),
        admitting(peers, Box::new(NoDns))
            .with_connections(Arc::new(SmallSendBuffer(send_buffer_bytes))),
    )
}

/// Opens outbound connections with a requested kernel send-buffer size.
struct SmallSendBuffer(u32);

#[async_trait::async_trait]
impl ConnectionFactory for SmallSendBuffer {
    async fn connect(
        &self,
        _: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<TcpStream> {
        let socket = TcpSocket::new_v4()?;
        socket.set_send_buffer_size(self.0)?;
        socket.connect(destination).await
    }
}

/// As [`reactor_for_peers`], also returning the host's side of every
/// outbound connection it opens.
pub fn reactor_observing_host_sockets(
    owner: u64,
    peers: Vec<SocketAddr>,
) -> (Reactor, Arc<HostSockets>) {
    let host_sockets = Arc::new(HostSockets::default());
    let reactor = Reactor::with_network(
        InstanceId(owner),
        admitting(peers, Box::new(NoDns)).with_connections(host_sockets.clone()),
    );
    (reactor, host_sockets)
}

/// Opens outbound connections and keeps a duplicate handle to each, through
/// which a test reads the options on the socket the host holds for a guest.
#[derive(Default)]
pub struct HostSockets(std::sync::Mutex<Vec<socket2::Socket>>);

impl HostSockets {
    /// A handle to each connection opened so far, in order. A handle outlives
    /// the guest's disposal of its socket and keeps the connection open.
    pub fn opened(&self) -> Vec<socket2::Socket> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .expect("no holder of the host socket list panics"),
        )
    }
}

#[async_trait::async_trait]
impl ConnectionFactory for HostSockets {
    async fn connect(
        &self,
        _: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<TcpStream> {
        let stream = TcpStream::connect(destination).await?;
        self.0
            .lock()
            .expect("no holder of the host socket list panics")
            .push(socket2::SockRef::from(&stream).try_clone()?);
        Ok(stream)
    }
}

/// The receive and send sizes this host OS reports for a socket given
/// `bytes` for each, which Linux, unlike other systems, doubles.
pub fn as_the_kernel_reports(bytes: usize) -> (usize, usize) {
    let reference = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
        .expect("a reference socket must open");
    reference
        .set_recv_buffer_size(bytes)
        .expect("a reference socket must take a receive size");
    reference
        .set_send_buffer_size(bytes)
        .expect("a reference socket must take a send size");
    (
        reference
            .recv_buffer_size()
            .expect("a reference socket must report its receive size"),
        reference
            .send_buffer_size()
            .expect("a reference socket must report its send size"),
    )
}

/// The deterministic byte pattern a large transfer carries; the guest's
/// `Sockets.Pattern` produces the same bytes.
pub fn pattern(length: usize) -> Vec<u8> {
    (0..length as u64)
        .map(|i| ((i * 131 + (i >> 9) * 29 + 7) & 0xff) as u8)
        .collect()
}

/// The 64-bit FNV-1a digest of `bytes`, order-sensitive; the guest's `drain`
/// route computes the same over what it received.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// What an owned peer does with a connection the guest opens.
#[derive(Clone, Copy)]
pub enum Peer {
    /// Reads until the guest stops sending and writes every byte back.
    Echo,
    /// Closes without sending anything.
    Eof,
    /// Aborts the connection so the guest observes a reset.
    Reset,
    /// Waits for the guest's send shutdown, then answers "ping" and closes.
    HalfClose,
    /// Accepts and keeps the connection open without sending.
    Silent,
    /// Writes the first `n` bytes of [`pattern`] and closes.
    Pattern(usize),
}

/// Starts a listener behaving as `peer` for every connection, returning its
/// address; the listener lives as long as the runtime.
pub async fn peer(peer: Peer) -> SocketAddr {
    serve_peer(peer, listener(None))
}

/// An IPv6-only listener, so a dual-family client must actually use IPv6.
pub async fn ipv6_peer(peer: Peer) -> SocketAddr {
    let socket = TcpSocket::new_v6().expect("the IPv6 test peer must open a socket");
    socket2::SockRef::from(&socket)
        .set_only_v6(true)
        .expect("the test peer must require IPv6");
    socket
        .bind((std::net::Ipv6Addr::LOCALHOST, 0).into())
        .expect("the IPv6 test peer must bind");
    serve_peer(
        peer,
        socket.listen(8).expect("the IPv6 test peer must listen"),
    )
}

/// As [`peer`], with the accepted connections' kernel receive buffer capped
/// at `receive_buffer_bytes`, so a sender fills it quickly.
pub async fn peer_with_receive_buffer(peer: Peer, receive_buffer_bytes: u32) -> SocketAddr {
    serve_peer(peer, listener(Some(receive_buffer_bytes)))
}

fn listener(receive_buffer_bytes: Option<u32>) -> TcpListener {
    let socket = TcpSocket::new_v4().expect("the test peer must open a socket");
    if let Some(bytes) = receive_buffer_bytes {
        socket
            .set_recv_buffer_size(bytes)
            .expect("the test peer must cap its receive buffer");
    }
    socket
        .bind((Ipv4Addr::LOCALHOST, 0).into())
        .expect("the test peer must bind");
    socket.listen(8).expect("the test peer must listen")
}

fn serve_peer(peer: Peer, listener: TcpListener) -> SocketAddr {
    let address = listener.local_addr().expect("the test peer has an address");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve(peer, stream));
        }
    });
    address
}

/// Starts a peer that answers "ping" to each connection only once the test
/// releases the returned gate, so the guest's read is parked for certain.
pub async fn gated_peer() -> (SocketAddr, Arc<Notify>) {
    let listener = listener(None);
    let address = listener.local_addr().expect("the test peer has an address");
    let gate = Arc::new(Notify::new());
    let released = gate.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            released.notified().await;
            let _answered = stream.write_all(b"ping").await;
            let mut byte = [0u8];
            let _closed = stream.read(&mut byte).await;
        }
    });
    (address, gate)
}

/// What a slow reader found once its sender closed.
pub struct Drained {
    /// How many bytes arrived.
    pub bytes: usize,
    /// The FNV-1a digest of the bytes in arrival order.
    pub digest: u64,
}

/// Starts a peer that reads its first connection only as the test permits:
/// one read of `chunk_bytes` per unit sent on the returned channel, and the
/// rest of the stream once the channel closes. Its kernel receive buffer is
/// capped at `receive_buffer_bytes`. What it read arrives on the returned
/// receiver once the sender closes.
pub async fn slow_drain_peer(
    receive_buffer_bytes: u32,
    chunk_bytes: usize,
) -> (SocketAddr, mpsc::Sender<()>, oneshot::Receiver<Drained>) {
    let listener = listener(Some(receive_buffer_bytes));
    let address = listener.local_addr().expect("the test peer has an address");
    let (permits, mut drains) = mpsc::channel(64);
    let (report, drained) = oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut received = Vec::new();
        let mut buffer = vec![0u8; chunk_bytes];
        let mut open = true;
        while open {
            let limit = match drains.recv().await {
                Some(()) => chunk_bytes,
                None => usize::MAX,
            };
            let mut drained = 0;
            while drained < limit {
                let count = match stream
                    .read(&mut buffer[..(limit - drained).min(chunk_bytes)])
                    .await
                {
                    Ok(0) | Err(_) => {
                        open = false;
                        break;
                    }
                    Ok(count) => count,
                };
                received.extend_from_slice(&buffer[..count]);
                drained += count;
            }
        }
        let _reported = report.send(Drained {
            bytes: received.len(),
            digest: fnv1a(&received),
        });
    });
    (address, permits, drained)
}

async fn serve(peer: Peer, mut stream: TcpStream) {
    match peer {
        Peer::Echo => {
            let mut buffer = [0u8; 1024];
            loop {
                let Ok(count) = stream.read(&mut buffer).await else {
                    return;
                };
                if count == 0 || stream.write_all(&buffer[..count]).await.is_err() {
                    return;
                }
            }
        }
        Peer::Eof => drop(stream),
        Peer::Reset => {
            socket2::SockRef::from(&stream)
                .set_linger(Some(Duration::ZERO))
                .expect("the peer reset must be controlled");
            drop(stream);
        }
        Peer::HalfClose => {
            let mut byte = [0u8];
            if stream.read(&mut byte).await.ok() == Some(0) {
                let _answered = stream.write_all(b"ping").await;
            }
        }
        Peer::Silent => {
            let mut byte = [0u8];
            let _closed = stream.read(&mut byte).await;
        }
        Peer::Pattern(length) => {
            let _written = stream.write_all(&pattern(length)).await;
        }
    }
}
