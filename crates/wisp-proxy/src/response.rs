//! Guest response validation and host-owned HTTP framing.

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Response, StatusCode};

use crate::headers::strip_connection_headers;
use crate::server::Body;

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
pub(crate) fn guest_response(
    method: &Method,
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
    if let Some(headers) = b.headers_mut() {
        strip_connection_headers(headers);
    }
    for (k, v) in platform_headers {
        b = b.header(*k, v);
    }
    match b.body(Full::new(Bytes::from(body))) {
        Ok(mut resp) => {
            if resp.status().is_informational() {
                tracing::warn!(
                    status,
                    "guest reply cannot be an interim response or protocol upgrade"
                );
                return text(
                    StatusCode::BAD_GATEWAY,
                    "guest reply was not a final HTTP response",
                    &[],
                );
            }
            // The guest has no authority over wire framing. HEAD and 304 may omit
            // length metadata, since we cannot verify a corresponding GET response.
            resp.headers_mut().remove(CONTENT_LENGTH);
            if method == Method::HEAD
                || method == Method::CONNECT && resp.status().is_success()
                || matches!(
                    resp.status(),
                    StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
                )
            {
                *resp.body_mut() = Full::new(Bytes::new());
            }
            resp
        }
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
            &Method::GET,
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
        let resp = guest_response(&Method::GET, 9999, &[], &[], Vec::new());
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an invalid guest status must fail the request, not the proxy"
        );
    }

    #[test]
    fn guest_platform_headers_are_dropped_and_the_proxy_s_own_kept() {
        let resp = guest_response(
            &Method::GET,
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
    #[test]
    fn a_guest_cannot_use_a_final_reply_to_send_an_interim_response_or_upgrade() {
        for status in [100, 101, 103] {
            let response = guest_response(&Method::GET, status, &[], &[], Vec::new());
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "guest status {status} cannot replace the host's interim-response or upgrade path"
            );
        }
    }
}
