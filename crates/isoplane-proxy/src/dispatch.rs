//! HTTP dispatch and guest response encoding.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::{Body as HyperBody, Incoming};
use hyper::{Request, Response, StatusCode};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_abi::host::{HttpBody, HttpUpload, RouteTarget};

use crate::headers::strip_connection_headers;
use crate::request::parse_query;
use crate::response::{guest_stream_response, text};
use crate::server::{Body, Proxy};

impl Proxy {
    pub(crate) async fn dispatch_http(
        self: &Arc<Self>,
        req: Request<Incoming>,
        target: RouteTarget,
        _peer: SocketAddr,
    ) -> Response<Body> {
        let (mut parts, body) = req.into_parts();
        let can_have_body = !body.is_end_stream();
        strip_connection_headers(&mut parts.headers);
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
            body: vec![],
        };
        match self
            .dispatcher
            .dispatch_http_upload(
                &target,
                HttpUpload {
                    request: hreq,
                    body: Box::new(UploadBody(body)),
                    can_have_body,
                },
            )
            .await
        {
            Ok(reply) => {
                guest_stream_response(
                    &parts.method,
                    reply,
                    &[
                        ("x-isoplane-tier", tier_str(target.tier).to_string()),
                        ("x-isoplane-version", target.version.clone()),
                    ],
                )
                .await
            }
            Err(e) if e.downcast_ref::<UploadReadError>().is_some() => {
                text(StatusCode::BAD_REQUEST, "bad body", &[])
            }
            Err(e) => {
                self.metrics.requests_failed.fetch_add(1, Relaxed);
                tracing::warn!(error = %e, app = %target.app, "dispatch failed");
                text(StatusCode::BAD_GATEWAY, "dispatch failed", &[])
            }
        }
    }

}

fn tier_str(t: isoplane_abi::host::CodeTier) -> &'static str {
    match t {
        isoplane_abi::host::CodeTier::Fast => "fast",
        isoplane_abi::host::CodeTier::Optimized => "optimized",
    }
}

#[derive(Debug, thiserror::Error)]
#[error("request body read failed: {0}")]
struct UploadReadError(hyper::Error);

struct UploadBody(Incoming);

#[async_trait::async_trait]
impl HttpBody for UploadBody {
    fn remaining_bytes(&self) -> Option<u64> {
        self.0.size_hint().exact()
    }

    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        while let Some(frame) = self.0.frame().await {
            let frame = frame.map_err(UploadReadError)?;
            if let Ok(bytes) = frame.into_data() {
                if !bytes.is_empty() {
                    return Ok(Some(bytes.to_vec()));
                }
            }
        }
        Ok(None)
    }
}
