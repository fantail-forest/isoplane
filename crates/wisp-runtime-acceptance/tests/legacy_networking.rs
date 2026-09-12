//! Legacy BCL networking through socket-backed asynchronous I/O and restored guest state.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
#[path = "common/http_peers.rs"]
pub mod http_peers;
#[path = "common/smtp_peer.rs"]
mod smtp_peer;
#[path = "common/tls_chains.rs"]
pub mod tls_chains;
#[path = "common/tls_material.rs"]
pub mod tls_material;
#[path = "common/trust_bundle.rs"]
mod trust_bundle;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use common::{guest, GuestRuntime};
use tokio::sync::Notify;
use wisp_engine::Completion;
use wisp_harness::EmittedGuestLogs as _;
use wisp_harness::Reactor;
use wisp_host::egress::DnsError;
use wisp_host::{InstanceId, Resolver};

async fn request(runtime: GuestRuntime, reactor: &Reactor, path: &str) -> String {
    wisp_harness::LogCapture::global();
    reactor.context.begin_slice(1_893_456_000_000, 1);
    let mut guest = guest(reactor, runtime);
    let started = guest.start(path);
    assert_eq!(
        started.completion,
        Completion::Parked,
        "legacy networking must enter asynchronous I/O before capture; response: {:?}; logs: {:?}",
        if started.completion == Completion::Done {
            Some(reactor.response_text(1))
        } else {
            None
        },
        reactor.context.emitted_logs()
    );
    guest.finish(reactor, 128).await
}

async fn http_exchange(runtime: GuestRuntime, mode: &str) -> String {
    let peer = http_peers::http_peer().await;
    let reactor = Reactor::new(
        InstanceId::alloc(),
        admitted::admitting(
            vec![peer.address],
            Box::new(fixed_dns::FixedDns(vec![peer.address.ip()])),
        ),
    );
    let answer = request(
        runtime,
        &reactor,
        &format!("/legacy/{mode}/{}", peer.address.port()),
    )
    .await;
    if mode == "request-cancel" {
        let egress = reactor.runtime().services.egress.clone();
        assert!(
            reactor.futures.end_dispatch(reactor.context.instance, 1),
            "ending the dispatch must identify the send that upstream GET Abort leaves behind"
        );
        drop(reactor);
        tokio::time::timeout(wisp_harness::WAKE_DEADLINE, async {
            while egress.connections() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("discarding the completed guest must release its abandoned HTTP connection");
    } else {
        admitted::released(&reactor).await;
    }
    answer
}

struct MailResolver {
    data_received: Arc<Notify>,
}

#[async_trait::async_trait]
impl Resolver for MailResolver {
    async fn resolve(&self, name: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError> {
        if name == "cancel-gate.test" {
            self.data_received.notified().await;
        }
        Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
    }
}

async fn mail_exchange(runtime: GuestRuntime, tls: bool, cancel: bool) {
    let peer = smtp_peer::SmtpPeer::start(tls, cancel).await;
    let reactor = Reactor::with_platform_files(
        InstanceId::alloc(),
        admitted::admitting(
            vec![peer.addr],
            Box::new(MailResolver {
                data_received: peer.data_received.clone(),
            }),
        ),
        trust_bundle::bundle(&[tls_material::ROOT_CERTIFICATE]),
    );
    let mode = if cancel {
        "smtp-cancel"
    } else if tls {
        "smtp-tls"
    } else {
        "smtp"
    };
    let answer = request(
        runtime,
        &reactor,
        &format!("/legacy/{mode}/{}", peer.addr.port()),
    )
    .await;
    assert_eq!(
        answer,
        if cancel {
            "SMTP cancelled"
        } else {
            "SMTP delivered"
        },
        "SMTP must settle its awaited send after restoring each asynchronous boundary"
    );
    let delivery = tokio::time::timeout(wisp_harness::WAKE_DEADLINE, peer.task)
        .await
        .expect("SMTP peer must settle after guest completion")
        .expect("SMTP peer task must not panic")
        .expect("SMTP peer must accept the guest's protocol exchange");
    let message =
        String::from_utf8(delivery.message).expect("MIME fixture must be ASCII-compatible");
    assert!(
        message.contains("Subject: sandbox async message"),
        "MIME headers must reach the peer across asynchronous writes"
    );
    assert!(
        message.contains("AQIDBAU="),
        "multipart attachment must retain its final bytes and base64 padding across async close"
    );
    if cancel {
        assert!(delivery.closed && !delivery.quit, "cancellation awaiting DATA acknowledgement must close the connection without sending QUIT in the unfinished transaction");
    } else {
        assert!(
            delivery.quit,
            "successful send must finish QUIT asynchronously before guest completion"
        );
    }
    admitted::released(&reactor).await;
    assert_eq!(reactor.futures.pending_total(reactor.context.instance), 0, "settled SMTP send must dispose its timeout and network futures without request-boundary cleanup");
}

in_both_runtimes! {
    #[tokio::test]
    async fn legacy_http_request_upload_and_response_survive_store_restoration(runtime: GuestRuntime) {
        assert_eq!(http_exchange(runtime, "request").await, "legacy async body", "HttpWebRequest must upload and consume a response using asynchronous socket I/O across Store restorations");
    }

    #[tokio::test]
    async fn web_client_download_survives_store_restoration(runtime: GuestRuntime) {
        assert_eq!(http_exchange(runtime, "web-client").await, "hello from peer", "WebClient async download must use the enabled Requests runtime implementation across Store restorations");
    }

    #[tokio::test]
    async fn aborting_a_legacy_http_request_after_parking_releases_its_connection(runtime: GuestRuntime) {
        assert_eq!(http_exchange(runtime, "request-cancel").await, "request cancelled", "HttpWebRequest.Abort must cancel its pending async response after guest parking");
    }

    #[tokio::test]
    async fn smtp_multipart_delivery_and_async_close_survive_store_restoration(runtime: GuestRuntime) {
        mail_exchange(runtime, false, false).await;
    }

    #[tokio::test]
    async fn smtp_starttls_and_multipart_delivery_survive_store_restoration(runtime: GuestRuntime) {
        mail_exchange(runtime, true, false).await;
    }

    #[tokio::test]
    async fn cancelling_smtp_after_data_releases_the_parked_send(runtime: GuestRuntime) {
        mail_exchange(runtime, false, true).await;
    }
}
