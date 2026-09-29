//! Guest response validation and host-owned HTTP framing.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Body as HttpBodyTrait, Frame, SizeHint};
use hyper::header::{HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Response, StatusCode};
use isoplane_abi::host::{HttpExchange, HttpResponseBody, ResponseDisposition};

use crate::headers::strip_connection_headers;
use crate::server::Body;

pub(crate) fn text(
    status: StatusCode,
    body: &str,
    headers: &[(HeaderName, HeaderValue)],
) -> Response<Body> {
    let mut resp = Response::new(full(body.as_bytes().to_vec()));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    for (k, v) in headers {
        resp.headers_mut().insert(k.clone(), v.clone());
    }
    resp
}

pub(crate) fn full(body: Vec<u8>) -> Body {
    Full::new(Bytes::from(body))
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub(crate) fn streaming(body: Box<dyn HttpResponseBody>) -> Body {
    let remaining_bytes = body.remaining_bytes();
    let stream = futures::stream::unfold(Some(body), |body| async move {
        let mut body = body?;
        match body.read().await {
            Ok(Some(bytes)) => Some((Ok(Frame::data(Bytes::from(bytes))), Some(body))),
            Ok(None) => None,
            Err(error) => Some((Err(error), None)),
        }
    });
    GuardedBody {
        body: StreamBody::new(stream).boxed_unsync(),
        guard: None::<()>,
        remaining_bytes,
    }
    .boxed_unsync()
}

pub(crate) fn hold_until_body_end<G: Send + Unpin + 'static>(body: Body, guard: G) -> Body {
    let remaining_bytes = body.size_hint().exact();
    GuardedBody {
        body,
        guard: Some(guard),
        remaining_bytes,
    }
    .boxed_unsync()
}

struct GuardedBody<G> {
    body: Body,
    guard: Option<G>,
    remaining_bytes: Option<u64>,
}

impl<G: Unpin> HttpBodyTrait for GuardedBody<G> {
    type Data = Bytes;
    type Error = anyhow::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, anyhow::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.body).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(remaining), Some(data)) = (&mut this.remaining_bytes, frame.data_ref())
                {
                    *remaining = remaining.saturating_sub(data.len() as u64);
                }
            }
            Poll::Ready(None | Some(Err(_))) => {
                this.guard.take();
            }
            Poll::Pending => {}
        }
        result
    }

    fn size_hint(&self) -> SizeHint {
        self.remaining_bytes
            .map_or_else(|| self.body.size_hint(), SizeHint::with_exact)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
}

pub(crate) async fn guest_stream_response(
    method: &Method,
    reply: HttpExchange,
    platform_headers: &[(&'static str, String)],
) -> Response<Body> {
    let response = match guest_head(reply.status, &reply.headers, platform_headers) {
        Ok(response) => response,
        Err(rejection) => return rejection.into_response(),
    };
    if permits_body(method, response.status()) {
        return response.map(|()| streaming(reply.body));
    }
    match reply.body.finish(ResponseDisposition::NoBody).await {
        Ok(()) => response.map(|()| full(vec![])),
        Err(error) => {
            tracing::warn!(%error, "bodyless guest response did not complete successfully");
            text(
                StatusCode::BAD_GATEWAY,
                "guest response failed before completion",
                &[],
            )
        }
    }
}

fn permits_body(method: &Method, status: StatusCode) -> bool {
    method != Method::HEAD
        && !(method == Method::CONNECT && status.is_success())
        && !matches!(
            status,
            StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
        )
}

enum GuestHeadRejection {
    InterimResponse,
    InvalidHttp,
}

impl GuestHeadRejection {
    fn into_response(self) -> Response<Body> {
        let message = match self {
            Self::InterimResponse => "guest reply was not a final HTTP response",
            Self::InvalidHttp => "guest reply was not valid HTTP",
        };
        text(StatusCode::BAD_GATEWAY, message, &[])
    }
}

fn guest_head(
    status: u16,
    guest_headers: &[(String, String)],
    platform_headers: &[(&'static str, String)],
) -> Result<Response<()>, GuestHeadRejection> {
    let mut b = Response::builder().status(status);
    for (k, v) in guest_headers {
        if k.to_ascii_lowercase().starts_with("x-isoplane-") {
            continue;
        }
        b = b.header(k, v);
    }
    if let Some(headers) = b.headers_mut() {
        strip_connection_headers(headers);
    }
    for (k, v) in platform_headers {
        b = b.header(*k, v);
    }
    match b.body(()) {
        Ok(mut resp) => {
            if resp.status().is_informational() {
                tracing::warn!(
                    status,
                    "guest reply cannot be an interim response or protocol upgrade"
                );
                return Err(GuestHeadRejection::InterimResponse);
            }
            // The guest has no authority over wire framing. HEAD and 304 may omit
            // length metadata, since we cannot verify a corresponding GET response.
            resp.headers_mut().remove(CONTENT_LENGTH);
            Ok(resp)
        }
        Err(e) => {
            tracing::warn!(error = %e, "guest reply carried a status or header HTTP cannot express");
            Err(GuestHeadRejection::InvalidHttp)
        }
    }
}

#[cfg(test)]
mod tests {
    async fn guest_response(
        method: &Method,
        status: u16,
        headers: &[(String, String)],
        platform: &[(&'static str, String)],
        body: Vec<u8>,
    ) -> Response<Body> {
        guest_stream_response(
            method,
            HttpExchange::buffered(isoplane_abi::host::HttpReply {
                status,
                headers: headers.to_vec(),
                body,
                streaming: false,
            }),
            platform,
        )
        .await
    }

    use super::*;

    #[tokio::test]
    async fn successful_connect_framing_waits_for_its_body_owner_without_reading() {
        struct AwaitEnd {
            requested: tokio::sync::oneshot::Sender<ResponseDisposition>,
            acknowledged: tokio::sync::oneshot::Receiver<()>,
        }
        #[async_trait::async_trait]
        impl isoplane_abi::host::HttpBody for AwaitEnd {
            async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
                panic!("CONNECT framing must not consume a forbidden response body");
            }
        }
        #[async_trait::async_trait]
        impl HttpResponseBody for AwaitEnd {
            async fn finish(
                self: Box<Self>,
                disposition: ResponseDisposition,
            ) -> anyhow::Result<()> {
                self.requested
                    .send(disposition)
                    .expect("observe the proxy's typed framing disposition");
                self.acknowledged.await?;
                Ok(())
            }
        }
        let (requested, mut request) = tokio::sync::oneshot::channel();
        let (acknowledge, acknowledged) = tokio::sync::oneshot::channel();
        let reply = HttpExchange {
            status: 200,
            headers: vec![],
            body: Box::new(AwaitEnd {
                requested,
                acknowledged,
            }),
        };
        let mut response = Box::pin(guest_stream_response(&Method::CONNECT, reply, &[]));
        assert!(
            futures::poll!(&mut response).is_pending(),
            "the proxy cannot finish CONNECT before the host acknowledges terminal cleanup"
        );
        assert_eq!(
            request.try_recv().expect("framing request"),
            ResponseDisposition::NoBody,
            "successful CONNECT must select deliberate no-body disposition"
        );
        acknowledge.send(()).expect("release terminal cleanup");
        let response = response.await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "successful terminal acknowledgement must retain the guest status"
        );
        assert!(
            response
                .into_body()
                .collect()
                .await
                .expect("valid empty body")
                .to_bytes()
                .is_empty(),
            "CONNECT must not transmit guest body bytes"
        );
    }

    #[tokio::test]
    async fn a_guest_header_http_cannot_carry_is_a_502() {
        let resp = guest_response(
            &Method::GET,
            200,
            &[("not a header".into(), "x".into())],
            &[],
            b"body".to_vec(),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an invalid guest header must fail the request, not the proxy"
        );
    }

    #[tokio::test]
    async fn a_guest_status_http_cannot_carry_is_a_502() {
        let resp = guest_response(&Method::GET, 9999, &[], &[], Vec::new()).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an invalid guest status must fail the request, not the proxy"
        );
    }

    #[tokio::test]
    async fn guest_platform_headers_are_dropped_and_the_proxy_s_own_kept() {
        let resp = guest_response(
            &Method::GET,
            200,
            &[("X-Isoplane-Version".into(), "forged".into())],
            &[("x-isoplane-version", "v2".into())],
            Vec::new(),
        )
        .await;
        let values: Vec<_> = resp
            .headers()
            .get_all("x-isoplane-version")
            .iter()
            .collect();
        assert_eq!(
            values,
            vec![&HeaderValue::from_static("v2")],
            "only the platform's value survives in its own namespace"
        );
    }
    #[tokio::test]
    async fn a_guest_cannot_use_a_final_reply_to_send_an_interim_response_or_upgrade() {
        for status in [100, 101, 103] {
            let response = guest_response(&Method::GET, status, &[], &[], Vec::new()).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "guest status {status} cannot replace the host's interim-response or upgrade path"
            );
        }
    }
    #[tokio::test]
    async fn a_body_lifetime_guard_preserves_known_length_and_releases_at_eof() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let mut body = hold_until_body_end(full(b"AB".to_vec()), Guard(released.clone()));
        assert_eq!(
            body.size_hint().exact(),
            Some(2),
            "lifetime ownership must preserve buffered wire framing"
        );
        assert!(
            !released.load(Ordering::Acquire),
            "admission must remain held before consumption"
        );
        body.frame().await.expect("body frame").expect("body data");
        assert_eq!(
            body.size_hint().exact(),
            Some(0),
            "the known remaining length must follow consumed bytes"
        );
        assert!(
            body.frame().await.is_none(),
            "the completed buffer must reach EOF"
        );
        assert!(
            released.load(Ordering::Acquire),
            "EOF must release admission even while the body object remains allocated"
        );
    }
}
