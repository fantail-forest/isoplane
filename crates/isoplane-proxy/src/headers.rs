//! Connection-specific HTTP fields never cross the guest boundary.

use hyper::header::{HeaderName, CONNECTION};
use hyper::HeaderMap;

pub(crate) fn strip_connection_headers(headers: &mut HeaderMap) {
    let nominated: Vec<_> = headers
        .get_all(CONNECTION)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        // An invalid token cannot name a valid header field.
        .filter_map(|token| HeaderName::from_bytes(token.trim_ascii()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    #[test]
    fn invalid_connection_tokens_do_not_hide_valid_options() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONNECTION,
            HeaderValue::from_bytes(b"x-first, \xff, invalid name, x-last").unwrap(),
        );
        headers.insert("x-first", HeaderValue::from_static("private"));
        headers.insert("x-last", HeaderValue::from_static("private"));
        headers.insert("x-app", HeaderValue::from_static("public"));
        strip_connection_headers(&mut headers);
        assert_eq!(
            headers.len(),
            1,
            "valid connection options must be removed even beside malformed tokens"
        );
        assert_eq!(
            headers["x-app"], "public",
            "unrelated application headers must survive malformed connection options"
        );
    }
}
