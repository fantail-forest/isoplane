//! An owned SMTP dialogue for guest mail delivery and cancellation tests.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::tls_chains::Certificate;

const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// The delivered DATA content and how its client ended the session.
pub struct Delivery {
    /// Message bytes including their CRLF line endings, without the dot terminator.
    pub message: Vec<u8>,
    /// Whether the client issued QUIT after receiving the delivery acknowledgement.
    pub quit: bool,
    /// Whether the client closed while the delivery acknowledgement was withheld.
    pub closed: bool,
}

/// A single-connection peer whose caller owns and awaits its completion.
pub struct SmtpPeer {
    /// The local endpoint this peer alone owns.
    pub addr: SocketAddr,
    /// Signals after the complete DATA message arrives, including its terminator.
    pub data_received: Arc<Notify>,
    /// The bounded dialogue; errors identify an unexpected SMTP command or I/O failure.
    pub task: JoinHandle<io::Result<Delivery>>,
}

impl SmtpPeer {
    /// Binds a local peer, optionally negotiating STARTTLS or withholding the DATA reply.
    pub async fn start(start_tls: bool, withhold_data_ack: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the SMTP test must own a local listener");
        let addr = listener
            .local_addr()
            .expect("the SMTP listener has an address");
        let data_received = Arc::new(Notify::new());
        let received = data_received.clone();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            tokio::time::timeout(Duration::from_secs(120), async move {
                greeting(&mut stream).await?;
                ehlo(&mut stream, start_tls).await?;
                if start_tls {
                    command(&mut stream, b"STARTTLS").await?;
                    stream.write_all(b"220 Begin TLS\r\n").await?;
                    let (chain, key) = Certificate::Leaf.presented();
                    let config = rustls::ServerConfig::builder()
                        .with_no_client_auth()
                        .with_single_cert(chain, key)
                        .map_err(io::Error::other)?;
                    let mut stream = TlsAcceptor::from(Arc::new(config)).accept(stream).await?;
                    ehlo(&mut stream, false).await?;
                    deliver(&mut stream, withhold_data_ack, &received).await
                } else {
                    deliver(&mut stream, withhold_data_ack, &received).await
                }
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "SMTP test dialogue did not finish")
            })?
        });
        Self {
            addr,
            data_received,
            task,
        }
    }
}

async fn greeting(stream: &mut TcpStream) -> io::Result<()> {
    for part in [
        b"220-leaf.acceptance.test\r\n220-".as_slice(),
        b"fragmented greeting\r\n220 ready\r",
        b"\n",
    ] {
        stream.write_all(part).await?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn ehlo<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    start_tls: bool,
) -> io::Result<()> {
    command(stream, b"EHLO ").await?;
    stream.write_all(b"250-leaf.acceptance.test\r\n").await?;
    if start_tls {
        stream.write_all(b"250-STARTTLS\r\n").await?;
    }
    stream
        .write_all(format!("250 SIZE {MAX_MESSAGE_BYTES}\r\n").as_bytes())
        .await
}

async fn command<S: AsyncRead + Unpin>(stream: &mut S, prefix: &[u8]) -> io::Result<()> {
    let line = line(stream).await?;
    if line.starts_with(prefix) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected SMTP {}, received {:?}",
                String::from_utf8_lossy(prefix),
                String::from_utf8_lossy(&line)
            ),
        ))
    }
}

async fn line<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < MAX_LINE_BYTES {
        bytes.push(stream.read_u8().await?);
        if bytes.ends_with(b"\r\n") {
            return Ok(bytes);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "SMTP test line exceeds its byte bound",
    ))
}

async fn deliver<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    withhold_data_ack: bool,
    received: &Notify,
) -> io::Result<Delivery> {
    command(stream, b"MAIL FROM:").await?;
    stream.write_all(b"250 Sender accepted\r\n").await?;
    command(stream, b"RCPT TO:").await?;
    stream.write_all(b"250 Recipient accepted\r\n").await?;
    command(stream, b"DATA").await?;
    stream.write_all(b"354 End with a dot\r\n").await?;
    let mut message = Vec::new();
    loop {
        let line = line(stream).await?;
        if line == b".\r\n" {
            break;
        }
        if message.len() + line.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SMTP test message exceeds its byte bound",
            ));
        }
        message.extend_from_slice(&line);
    }
    received.notify_one();
    if withhold_data_ack {
        let mut byte = [0];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cancelled SMTP client sent data instead of closing",
                ))
            }
            Err(error) => return Err(error),
        }
        return Ok(Delivery {
            message,
            quit: false,
            closed: true,
        });
    }
    stream.write_all(b"250 Delivered\r\n").await?;
    command(stream, b"QUIT").await?;
    Ok(Delivery {
        message,
        quit: true,
        closed: false,
    })
}
