//! The peer that serves the material's issuers, CRLs and OCSP responses at
//! the fixed loopback port the certificates name, and the leaves that point
//! at it. One test at a time owns the port; every request it answers is
//! recorded, so a test can assert what a chain build fetched and in what
//! order.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::task::JoinHandle;

use isoplane_test_support::hex_bytes as hex;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::chain_material::{
    CRL_EXPIRED, CRL_GOOD, CRL_REVOKED, DEEP_CERTIFICATE, DEEP_KEY, DENIED_CERTIFICATE, DENIED_KEY,
    DENIED_PORT, INTERMEDIATE2_DER, INTERMEDIATE_DER, MATERIAL_PORT, OCSP_LEAF_CERTIFICATE,
    OCSP_LEAF_GOOD, OCSP_LEAF_KEY, OCSP_REMOTE_EXPIRED, OCSP_REMOTE_GOOD, OCSP_REMOTE_REVOKED,
    OCSP_REMOTE_STRANGER,
};
use crate::tls_chains::parse_chain;

/// A leaf presented alone whose chain a build completes by retrieval.
#[derive(Clone, Copy)]
pub enum RetrievalLeaf {
    /// `deep.acceptance.test` under the second intermediate: two issuers to
    /// retrieve, each named by the certificate below it.
    Deep,
    /// `ocsp.acceptance.test`: an issuer and an OCSP responder at URLs, no CRL.
    OcspLeaf,
    /// `denied.acceptance.test`: its issuer's URL is at the port no policy
    /// admits.
    Denied,
}

impl RetrievalLeaf {
    /// The leaf alone and its private key, as the peer's material.
    pub fn presented(self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        match self {
            RetrievalLeaf::Deep => parse_chain(DEEP_CERTIFICATE, DEEP_KEY),
            RetrievalLeaf::OcspLeaf => parse_chain(OCSP_LEAF_CERTIFICATE, OCSP_LEAF_KEY),
            RetrievalLeaf::Denied => parse_chain(DENIED_CERTIFICATE, DENIED_KEY),
        }
    }
}

/// The ports the certificates name are fixed, so the tests that need one
/// take turns at it: within this process through a mutex, across the
/// processes of this host (another test binary, another checkout) through
/// a lock file named for the port. A turn lasts as long as the listener: it
/// travels with the accept task, and dropping it closes the listener
/// before either lock is released, so the next test binds only once the
/// port is free, whether the previous test passed, failed, or was still
/// shutting its runtime down. The close goes through the std listener
/// because tokio's own drop hands the socket to its driver and the driver
/// closes it later, after these locks would already be gone.
struct Turn {
    listener: Option<tokio::net::TcpListener>,
    _host: std::fs::File,
    _port: tokio::sync::MutexGuard<'static, ()>,
}

impl Turn {
    async fn take(port: &'static tokio::sync::Mutex<()>, number: u16) -> Self {
        let _port = port.lock().await;
        let host = std::fs::File::create(
            std::env::temp_dir().join(format!("isoplane-acceptance-port-{number}.lock")),
        )
        .expect("the port's lock file is creatable in the temporary directory");
        // Blocking the test's thread is the point: nothing of this test runs
        // until the port is its own.
        lock_exclusively(&host);
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, number))
            .await
            .unwrap_or_else(|error| {
                let holders = std::process::Command::new("ss")
                    .args(["-tlnp", &format!("sport = :{number}")])
                    .output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                    .unwrap_or_else(|error| format!("(ss unavailable: {error})"));
                panic!(
                    "a fixed port is free once its turn is taken: {error}; listeners:\n{holders}"
                )
            });
        Self {
            listener: Some(listener),
            _host: host,
            _port,
        }
    }

    fn listener(&self) -> &tokio::net::TcpListener {
        self.listener
            .as_ref()
            .expect("the listener is taken only by the turn's drop")
    }
}

/// An exclusive advisory lock on `file`, released when the file closes.
fn lock_exclusively(file: &std::fs::File) {
    if let Err(error) = fs4::FileExt::lock(file) {
        panic!("the port's lock file is lockable: {error}");
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.take() {
            match listener.into_std() {
                Ok(closing) => drop(closing),
                Err(error) => panic!("the fixed port's listener must close with its turn: {error}"),
            }
        }
    }
}

static MATERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DENIED: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A listener at the port no policy admits, counting whatever reaches it.
pub struct Sentinel {
    reached: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl Sentinel {
    /// Listens at [`DENIED_PORT`]; waits for the port if another test holds it.
    pub async fn start() -> Self {
        let turn = Turn::take(&DENIED, DENIED_PORT).await;
        let reached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reached.clone();
        let task = tokio::spawn(async move {
            while turn.listener().accept().await.is_ok() {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        Self { reached, task }
    }

    /// How many connections reached the port.
    pub fn reached(&self) -> usize {
        self.reached.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How the peer answers a request for an issuer certificate.
#[derive(Clone, Copy)]
pub enum Issuers {
    /// The DER of the intermediate asked for.
    Served,
    /// Accepts the request and never answers.
    Hold,
    /// A body far past any download limit a test sets.
    Oversized,
}

/// How the peer answers a request for the intermediate's CRL.
#[derive(Clone, Copy)]
pub enum Crl {
    /// Lists nothing.
    Good,
    /// Lists the remote leaf.
    Revoked,
    /// Its next update is in 2020.
    Expired,
    /// 404.
    Unavailable,
    /// Accepts the request and never answers.
    Hold,
}

/// How the peer answers an OCSP query.
#[derive(Clone, Copy)]
pub enum Ocsp {
    /// Good, fresh for a century, for the remote leaf.
    Good,
    /// Revoked, for the remote leaf.
    Revoked,
    /// Good, but its next update was a minute after generation.
    Expired,
    /// Good, signed by the stranger.
    Stranger,
    /// Good, for the ocsp leaf.
    LeafGood,
    /// Not a DER response at all.
    Malformed,
    /// 404.
    Unavailable,
    /// Accepts the request and never answers.
    Hold,
}

/// A running material peer; dropping it releases the port.
pub struct MaterialPeer {
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl MaterialPeer {
    /// Starts the peer with these answers; waits for the port if another
    /// test holds it.
    pub async fn start(issuers: Issuers, crl: Crl, ocsp: Ocsp) -> Self {
        let turn = Turn::take(&MATERIAL, MATERIAL_PORT).await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = turn.listener().accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let log = log.clone();
                        async move { answer(request, issuers, crl, ocsp, log).await }
                    });
                    let _served = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self { requests, task }
    }

    /// The address the certificates' URLs name.
    pub fn address(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, MATERIAL_PORT))
    }

    /// The paths requested so far, in order; an OCSP query is recorded as `/ocsp`.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log is never poisoned")
            .clone()
    }
}

impl Drop for MaterialPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type PeerBody = BoxBody<Bytes, std::convert::Infallible>;

fn bytes(content_type: &str, body: Vec<u8>) -> Response<PeerBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)).boxed())
        .expect("a static response builds")
}

fn not_found() -> Response<PeerBody> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::new()).boxed())
        .expect("a static response builds")
}

async fn hold() -> Response<PeerBody> {
    std::future::pending::<()>().await;
    unreachable!("a held request is never answered")
}

async fn answer(
    request: Request<Incoming>,
    issuers: Issuers,
    crl: Crl,
    ocsp: Ocsp,
    log: Arc<Mutex<Vec<String>>>,
) -> Result<Response<PeerBody>, std::convert::Infallible> {
    let path = request.uri().path().to_owned();
    let recorded = if path.starts_with("/ocsp") {
        "/ocsp".to_owned()
    } else {
        path.clone()
    };
    log.lock()
        .expect("the request log is never poisoned")
        .push(recorded.clone());
    Ok(match recorded.as_str() {
        "/intermediate.der" | "/intermediate2.der" => match issuers {
            Issuers::Served => bytes(
                "application/pkix-cert",
                hex(if recorded == "/intermediate.der" {
                    INTERMEDIATE_DER
                } else {
                    INTERMEDIATE2_DER
                }),
            ),
            Issuers::Hold => hold().await,
            Issuers::Oversized => bytes("application/pkix-cert", vec![0; 4 << 20]),
        },
        "/intermediate.crl" => match crl {
            Crl::Good => bytes("application/pkix-crl", hex(CRL_GOOD)),
            Crl::Revoked => bytes("application/pkix-crl", hex(CRL_REVOKED)),
            Crl::Expired => bytes("application/pkix-crl", hex(CRL_EXPIRED)),
            Crl::Unavailable => not_found(),
            Crl::Hold => hold().await,
        },
        "/ocsp" => match ocsp {
            Ocsp::Good => bytes("application/ocsp-response", hex(OCSP_REMOTE_GOOD)),
            Ocsp::Revoked => bytes("application/ocsp-response", hex(OCSP_REMOTE_REVOKED)),
            Ocsp::Expired => bytes("application/ocsp-response", hex(OCSP_REMOTE_EXPIRED)),
            Ocsp::Stranger => bytes("application/ocsp-response", hex(OCSP_REMOTE_STRANGER)),
            Ocsp::LeafGood => bytes("application/ocsp-response", hex(OCSP_LEAF_GOOD)),
            Ocsp::Malformed => bytes(
                "application/ocsp-response",
                b"not an OCSP response".to_vec(),
            ),
            Ocsp::Unavailable => not_found(),
            Ocsp::Hold => hold().await,
        },
        _ => not_found(),
    })
}
