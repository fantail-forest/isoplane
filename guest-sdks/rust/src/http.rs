//! Request and response shapes for the Handler entrypoint.

use isoplane_abi::dispatch::ResponseHead;
use serde::Serialize;

use crate::abi;
use crate::error::Result;
use crate::future::{decode_unit, HostFuture};

pub use isoplane_abi::dispatch::HttpRequest as Request;

pub trait RequestExt {
    fn query_get(&self, key: &str) -> Option<&str>;
    fn header(&self, name: &str) -> Option<&str>;
}

impl RequestExt for Request {
    fn query_get(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

enum Inner {
    /// Small bodies go inline in the head; the host writes them in one go.
    Head(ResponseHead),
    /// A handle that `resp_new` already produced (streaming case).
    Handle(u32),
}

pub struct Response(Inner);

impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Response {
        Response(Inner::Head(ResponseHead {
            status,
            headers: Vec::new(),
            body,
            streaming: false,
        }))
    }
    pub fn status(status: u16) -> Response {
        Response::new(status, Vec::new())
    }
    pub fn text(status: u16, body: impl Into<String>) -> Response {
        Response::new(status, body.into().into_bytes())
            .with_header("content-type", "text/plain; charset=utf-8")
    }
    #[cfg(feature = "json")]
    pub fn json<T: Serialize>(v: &T) -> Response {
        match serde_json::to_vec(v) {
            Ok(b) => Response::new(200, b).with_header("content-type", "application/json"),
            Err(e) => Response::text(500, format!("serialize: {e}")),
        }
    }
    /// A response body encoded as Postcard.
    pub fn postcard<T: Serialize>(v: &T) -> Response {
        match postcard::to_allocvec(v) {
            Ok(b) => Response::new(200, b).with_header("content-type", "application/x-postcard"),
            Err(e) => Response::text(500, format!("serialize: {e}")),
        }
    }
    pub fn with_header(mut self, k: &str, v: &str) -> Response {
        if let Inner::Head(h) = &mut self.0 {
            h.headers.push((k.into(), v.into()));
        }
        self
    }
    pub fn from_stream(s: &ResponseStream) -> Response {
        Response(Inner::Handle(s.handle))
    }

    /// Turn the response into the host handle that `PollStatus::Done` carries.
    pub(crate) fn into_handle(self) -> u32 {
        match self.0 {
            Inner::Handle(h) => h,
            Inner::Head(head) => {
                let buf = postcard::to_allocvec(&head).expect("postcard encode");
                unsafe { abi::resp_new(buf.as_ptr() as i32, buf.len() as i32) as u32 }
            }
        }
    }
}

/// A streaming response whose writes await host-owned futures.
pub struct ResponseStream {
    handle: u32,
}

impl ResponseStream {
    pub fn open(status: u16, headers: Vec<(String, String)>) -> ResponseStream {
        let head = ResponseHead {
            status,
            headers,
            body: Vec::new(),
            streaming: true,
        };
        let buf = postcard::to_allocvec(&head).expect("postcard encode");
        let handle = unsafe { abi::resp_new(buf.as_ptr() as i32, buf.len() as i32) as u32 };
        ResponseStream { handle }
    }
    pub fn handle(&self) -> u32 {
        self.handle
    }
    pub fn write(&self, chunk: &[u8]) -> HostFuture<()> {
        let id = unsafe {
            abi::resp_write(
                self.handle as i32,
                chunk.as_ptr() as i32,
                chunk.len() as i32,
            )
        } as u64;
        HostFuture::new(id, decode_unit)
    }
    pub async fn write_all(&self, chunk: &[u8]) -> Result<()> {
        self.write(chunk).await
    }
    pub fn finish(self) {
        unsafe { abi::resp_finish(self.handle as i32) };
    }
}
