//! Records the actual connection origin selected by HTTP and managed TCP.
use isoplane_host::egress::{ConnectionFactory, ConnectionOwner, LocalConnections};
use parking_lot::Mutex;
use std::net::SocketAddr;

#[derive(Default)]
pub struct RecordingConnections {
    pub calls: Mutex<Vec<(ConnectionOwner, SocketAddr)>>,
    pub substitute: Option<SocketAddr>,
}

#[async_trait::async_trait]
impl ConnectionFactory for RecordingConnections {
    async fn connect(
        &self,
        owner: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        self.calls.lock().push((owner.clone(), destination));
        LocalConnections
            .connect(owner, self.substitute.unwrap_or(destination))
            .await
    }
}
