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
