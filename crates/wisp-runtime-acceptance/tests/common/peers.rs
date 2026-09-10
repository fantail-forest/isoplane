//! Owned TCP peers that put a connection into each state the runtime must
//! handle, and a reactor that admits exactly them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wisp_harness::Reactor;
use wisp_host::egress::{DenyReason, DestinationPolicy, HostEgressPolicy};
use wisp_host::InstanceId;

use crate::common::NoDns;

/// A destination policy admitting exactly the test's own peers.
struct OnlyPeers(Vec<SocketAddr>);

impl DestinationPolicy for OnlyPeers {
    fn authorize(&self, peer: SocketAddr) -> Result<(), DenyReason> {
        if self.0.contains(&peer) {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(peer.to_string()))
        }
    }
}

/// A reactor whose outbound connections may only reach `peers`.
pub fn reactor_for_peers(owner: u64, peers: Vec<SocketAddr>) -> Reactor {
    Reactor::new(
        InstanceId(owner),
        HostEgressPolicy::new(Box::new(NoDns), 8, 1 << 20)
            .with_destinations(Arc::new(OnlyPeers(peers))),
    )
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
    /// Waits for the guest's send shutdown, then answers "wisp" and closes.
    HalfClose,
    /// Accepts and keeps the connection open without sending.
    Silent,
}

/// Starts a listener behaving as `peer` for every connection, returning its
/// address; the listener lives as long as the runtime.
pub async fn peer(peer: Peer) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the test peer must bind");
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

async fn serve(peer: Peer, mut stream: tokio::net::TcpStream) {
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
                let _answered = stream.write_all(b"wisp").await;
            }
        }
        Peer::Silent => {
            let mut byte = [0u8];
            let _closed = stream.read(&mut byte).await;
        }
    }
}

/// An endpoint that refuses connections: bound so nothing else can take it,
/// never listening.
pub fn refusing_endpoint() -> (socket2::Socket, SocketAddr) {
    let reservation = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .expect("the test must reserve a non-listening TCP endpoint");
    reservation
        .bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .expect("the non-listening endpoint must bind");
    let address = reservation
        .local_addr()
        .expect("the reservation has an endpoint")
        .as_socket()
        .expect("the reservation is an IP endpoint");
    (reservation, address)
}
