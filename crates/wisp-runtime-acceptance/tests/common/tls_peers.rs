//! Owned TLS peers for the `SslStream` family: a server presenting one of
//! the fixed certificates, optionally requiring a client certificate or
//! negotiating ALPN, that echoes what the guest sends, closes cleanly,
//! truncates a record, or never answers; every peer reports what it saw.

use std::net::SocketAddr;
use std::sync::Arc;

use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

pub use crate::tls_chains::Certificate;
use crate::tls_material::ROOT_CERTIFICATE;

/// What the peer does with a connection.
#[derive(Clone, Copy)]
pub enum Behaviour {
    /// Handshakes, then writes every byte back until the guest stops sending.
    Echo,
    /// Handshakes, then closes with a TLS close_notify without sending.
    Eof,
    /// Handshakes, then sends the first half of one record and drops the
    /// connection without a close_notify.
    TruncatedRecord,
    /// Reads the guest's ClientHello, then drops the connection.
    CloseDuringHandshake,
    /// Accepts the connection and never answers.
    Silent,
    /// Handshakes, then reads whatever the guest sends and never answers,
    /// keeping the connection open.
    Quiet,
    /// Handshakes, then answers each HTTP/1.1 request with a greeting and
    /// keeps the connection open.
    HttpHello,
}

/// What a peer observed on one connection.
#[derive(Debug, PartialEq, Eq)]
pub enum PeerEvent {
    /// The handshake completed with this negotiated ALPN protocol and
    /// whether the guest presented a certificate.
    Handshake {
        alpn: Option<Vec<u8>>,
        client_certificate: Option<String>,
    },
    /// The guest's side closed: cleanly (a close_notify) or not.
    Closed { clean: bool },
}

/// A TLS peer under construction.
pub struct TlsPeer {
    chain: (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>),
    behaviour: Behaviour,
    client_auth: bool,
    alpn: Vec<Vec<u8>>,
}

impl TlsPeer {
    pub fn new(certificate: Certificate, behaviour: Behaviour) -> Self {
        Self::presenting(certificate.presented(), behaviour)
    }

    /// A peer presenting exactly this chain with its leaf's key.
    pub fn presenting(
        chain: (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>),
        behaviour: Behaviour,
    ) -> Self {
        Self {
            chain,
            behaviour,
            client_auth: false,
            alpn: Vec::new(),
        }
    }

    /// Requires a client certificate issued by the material's root.
    pub fn requiring_client_certificate(mut self) -> Self {
        self.client_auth = true;
        self
    }

    /// Offers these ALPN protocols, in preference order.
    pub fn with_alpn(mut self, protocols: &[&str]) -> Self {
        self.alpn = protocols.iter().map(|p| p.as_bytes().to_vec()).collect();
        self
    }

    /// Starts the peer; the listener lives as long as the runtime and every
    /// connection's observations arrive on the returned channel.
    pub async fn start(self) -> (SocketAddr, mpsc::UnboundedReceiver<PeerEvent>) {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the TLS peer must bind");
        let address = listener.local_addr().expect("the TLS peer has an address");
        let behaviour = self.behaviour;
        let acceptor = self.acceptor();
        let (events, observed) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(acceptor.clone(), behaviour, stream, events.clone()));
            }
        });
        (address, observed)
    }

    fn acceptor(self) -> TlsAcceptor {
        let (certificates, key) = self.chain;
        let builder = rustls::ServerConfig::builder();
        let builder = if self.client_auth {
            let mut roots = RootCertStore::empty();
            roots
                .add(
                    CertificateDer::from_pem_slice(ROOT_CERTIFICATE.as_bytes())
                        .expect("the test material's root parses"),
                )
                .expect("the test material's root is a trust anchor");
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .expect("the client verifier builds from the root");
            builder.with_client_cert_verifier(verifier)
        } else {
            builder.with_no_client_auth()
        };
        let mut config = builder
            .with_single_cert(certificates, key)
            .expect("the test peer's TLS configuration is valid");
        config.alpn_protocols = self.alpn.clone();
        TlsAcceptor::from(Arc::new(config))
    }
}

/// Starts a peer presenting `certificate` that behaves as `behaviour`,
/// observing nothing.
pub async fn tls_peer(certificate: Certificate, behaviour: Behaviour) -> SocketAddr {
    TlsPeer::new(certificate, behaviour).start().await.0
}

async fn serve(
    acceptor: TlsAcceptor,
    behaviour: Behaviour,
    mut tcp: TcpStream,
    events: mpsc::UnboundedSender<PeerEvent>,
) {
    match behaviour {
        Behaviour::Silent => {
            // Reads whatever the guest sends and never answers; a dropped
            // connection would be a reset, not silence.
            let mut sink = [0u8; 1024];
            while matches!(tcp.read(&mut sink).await, Ok(count) if count > 0) {}
            return;
        }
        Behaviour::CloseDuringHandshake => {
            let mut hello = [0u8; 64];
            let _read = tcp.read(&mut hello).await;
            return;
        }
        Behaviour::Echo
        | Behaviour::Eof
        | Behaviour::TruncatedRecord
        | Behaviour::HttpHello
        | Behaviour::Quiet => {}
    }
    // A handshake the client refuses ends here; that is the stranger's
    // purpose.
    let Ok(mut stream) = acceptor.accept(tcp).await else {
        return;
    };
    {
        let (_, connection) = stream.get_ref();
        let client_certificate = connection
            .peer_certificates()
            .and_then(|chain| chain.first())
            .map(|certificate| format!("{} bytes", certificate.len()));
        let _reported = events.send(PeerEvent::Handshake {
            alpn: connection.alpn_protocol().map(<[u8]>::to_vec),
            client_certificate,
        });
    }
    match behaviour {
        Behaviour::Echo => {
            let mut buffer = [0; 4096];
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) => {
                        let _reported = events.send(PeerEvent::Closed { clean: true });
                        return;
                    }
                    Ok(count) => {
                        if stream.write_all(&buffer[..count]).await.is_err()
                            || stream.flush().await.is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => {
                        let _reported = events.send(PeerEvent::Closed { clean: false });
                        return;
                    }
                }
            }
        }
        Behaviour::Eof => {
            let _closed = stream.shutdown().await;
        }
        Behaviour::TruncatedRecord => {
            // The record is produced by rustls into a buffer, and only its
            // first half reaches the wire before the connection drops.
            let (tcp, connection) = stream.get_mut();
            let mut record = Vec::new();
            std::io::Write::write_all(&mut connection.writer(), b"half of this never arrives")
                .expect("rustls buffers plaintext");
            connection
                .write_tls(&mut record)
                .expect("rustls encodes the record");
            let _sent = tcp.write_all(&record[..record.len() / 2]).await;
            let _flushed = tcp.flush().await;
        }
        Behaviour::HttpHello => {
            let mut buffer = [0; 4096];
            let mut head = Vec::new();
            loop {
                let Ok(count) = stream.read(&mut buffer).await else {
                    return;
                };
                if count == 0 {
                    return;
                }
                head.extend_from_slice(&buffer[..count]);
                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                    head.clear();
                    let body = b"hello from peer";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n",
                        body.len()
                    );
                    if stream.write_all(response.as_bytes()).await.is_err()
                        || stream.write_all(body).await.is_err()
                        || stream.flush().await.is_err()
                    {
                        return;
                    }
                }
            }
        }
        Behaviour::Quiet => {
            let mut sink = [0u8; 1024];
            while matches!(stream.read(&mut sink).await, Ok(count) if count > 0) {}
        }
        Behaviour::Silent | Behaviour::CloseDuringHandshake => {}
    }
}
