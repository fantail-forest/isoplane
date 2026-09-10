//! Owned TCP peers that put a connection into each state the runtime must
//! handle, and a reactor that admits exactly them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use wisp_harness::Reactor;
use wisp_host::InstanceId;

use crate::admitted::admitting;
use crate::no_dns::NoDns;

/// A reactor whose outbound connections may only reach `peers`, resolving
/// nothing: the guest connects by address.
pub fn reactor_for_peers(owner: u64, peers: Vec<SocketAddr>) -> Reactor {
    Reactor::new(InstanceId(owner), admitting(peers, Box::new(NoDns)))
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

/// Starts a peer that answers "wisp" to each connection only once the test
/// releases the returned gate, so the guest's read is parked for certain.
pub async fn gated_peer() -> (SocketAddr, Arc<Notify>) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the test peer must bind");
    let address = listener.local_addr().expect("the test peer has an address");
    let gate = Arc::new(Notify::new());
    let released = gate.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            released.notified().await;
            let _answered = stream.write_all(b"wisp").await;
            let mut byte = [0u8];
            let _closed = stream.read(&mut byte).await;
        }
    });
    (address, gate)
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
