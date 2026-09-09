//! HTTP dispatch and guest response encoding.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{RouteTarget};

use crate::headers::strip_connection_headers;
use crate::request::parse_query;
use crate::response::{guest_stream_response, streaming, text};
use crate::server::{Body, Proxy};

impl Proxy {
    pub(crate) async fn dispatch_http(
        self: &Arc<Self>,
        req: Request<Incoming>,
        target: RouteTarget,
        _peer: SocketAddr,
    ) -> Response<Body> {
        let (mut parts, body) = req.into_parts();
        strip_connection_headers(&mut parts.headers);
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
        match self.dispatcher.dispatch_http_stream(&target, hreq).await {
            Ok(reply) => guest_stream_response(
                &parts.method,
                reply.status,
                &reply.headers,
                &[
                    ("x-wisp-tier", tier_str(target.tier).to_string()),
                    ("x-wisp-version", target.version.clone()),
                ],
                streaming(reply.body),
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
