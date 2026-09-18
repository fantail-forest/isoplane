//! `hello` — the smallest complete Handler: every request gets a 200 and a
//! short body. A test that needs a real registered module, and nothing about
//! what it does, deploys this one.

use core::future::Future;

use isoplane_guest::{Ctx, Handler, Request, Response};

pub struct Hello;

impl Handler for Hello {
    fn new() -> Self {
        Hello
    }

    fn handle(&'static self, _req: Request, _ctx: Ctx) -> impl Future<Output = Response> + 'static {
        async move { Response::text(200, "hello from wasm") }
    }
}

isoplane_guest::export_handler!(Hello);
