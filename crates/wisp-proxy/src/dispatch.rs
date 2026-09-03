//! HTTP dispatch and guest response encoding.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use hyper::{Request, Response, StatusCode};
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{RouteTarget};

use crate::request::parse_query;
use crate::server::{Body, Proxy};

impl Proxy {
    pub(crate) async fn dispatch_http(
        self: &Arc<Self>,
        req: Request<Incoming>,
        target: RouteTarget,
        _peer: SocketAddr,
    ) -> Response<Body> {
        let (parts, body) = req.into_parts();
        let body = match body.collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return text(StatusCode::BAD_REQUEST, "bad body", &[]),
        };
        let hreq = HttpRequest {
            method: parts.method.to_string(),
            url: parts.uri.to_string(),
            path: parts.uri.path().to_string(),
            query: parts.uri.query().map(parse_query).unwrap_or_default(),
            headers: parts
                .headers
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: body.to_vec(),
        };
        match self.dispatcher.dispatch_http(&target, hreq).await {
            Ok(reply) => guest_response(
                reply.status,
                &reply.headers,
                &[
                    ("x-wisp-tier", tier_str(target.tier).to_string()),
                    ("x-wisp-version", target.version.clone()),
                ],
                reply.body,
            ),
            Err(e) => {
                self.metrics.requests_failed.fetch_add(1, Relaxed);
                tracing::warn!(error = %e, app = %target.app, "dispatch failed");
                text(StatusCode::BAD_GATEWAY, "dispatch failed", &[])
            }
        }
    }

}

fn tier_str(t: wisp_abi::host::CodeTier) -> &'static str {
    match t {
        wisp_abi::host::CodeTier::Fast => "fast",
        wisp_abi::host::CodeTier::Optimized => "optimized",
    }
}

pub(crate) fn text(
    status: StatusCode,
    body: &str,
    headers: &[(HeaderName, HeaderValue)],
) -> Response<Body> {
    let mut resp = Response::new(Full::new(Bytes::from(body.to_string())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    for (k, v) in headers {
        resp.headers_mut().insert(k.clone(), v.clone());
    }
    resp
}

/// A guest's reply as the client sees it. Guest headers in the `x-wisp-`
/// namespace are dropped, because a guest that could set `x-wisp-version`
/// could lie to its operator about which build served the request; the
/// platform's own values are appended. A status or header the guest wrote
/// that HTTP cannot carry is a 502, not a panic.
fn guest_response(
    status: u16,
    guest_headers: &[(String, String)],
    platform_headers: &[(&'static str, String)],
    body: Vec<u8>,
) -> Response<Body> {
    let mut b = Response::builder().status(status);
    for (k, v) in guest_headers {
        if k.to_ascii_lowercase().starts_with("x-wisp-") {
            continue;
        }
        b = b.header(k, v);
    }
    for (k, v) in platform_headers {
        b = b.header(*k, v);
    }
    match b.body(Full::new(Bytes::from(body))) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "guest reply carried a status or header HTTP cannot express");
            text(
                StatusCode::BAD_GATEWAY,
                "guest reply was not valid HTTP",
                &[],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_header_http_cannot_carry_is_a_502() {
        let resp = guest_response(
            200,
            &[("not a header".into(), "x".into())],
            &[],
            b"body".to_vec(),
        );
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an invalid guest header must fail the request, not the proxy"
        );
    }

    #[test]
    fn a_guest_status_http_cannot_carry_is_a_502() {
        let resp = guest_response(9999, &[], &[], Vec::new());
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an invalid guest status must fail the request, not the proxy"
        );
    }

    #[test]
    fn guest_platform_headers_are_dropped_and_the_proxy_s_own_kept() {
        let resp = guest_response(
            200,
            &[("X-Wisp-Version".into(), "forged".into())],
            &[("x-wisp-version", "v2".into())],
            Vec::new(),
        );
        let values: Vec<_> = resp.headers().get_all("x-wisp-version").iter().collect();
        assert_eq!(
            values,
            vec![&HeaderValue::from_static("v2")],
            "only the platform's value survives in its own namespace"
        );
    }
}
