//! `undeclared-import` — the validator's rule **V1** corpus.
//!
//! V1 rejects any import outside the world's allowlist. Imports are the
//! complete capability surface, so an undeclared import is an attempted
//! capability and there is no "dynamic linking" escape hatch.
//!
//! This module is otherwise a perfectly well-formed guest: it exports
//! `isoplane_alloc`, `isoplane_handle`, `isoplane_wake`, `isoplane_gc_hint` and
//! `isoplane_initialize`, and it imports the real capabilities through the SDK.
//! The *only* thing wrong with it is one extra import. It must therefore be
//! rejected at deploy time, not at run time -- if it ever instantiates, V1 is
//! not being enforced.
//!
//! Two build variants, because there are two ways to smuggle a capability:
//!
//! * default: an **unknown field** of the declared module -- `isoplane::fs_open`.
//!   The module name is right, so a validator that only checks module names
//!   would pass it.
//! * `--features foreign-module`: an **unknown module** -- `env::__wasi_fd_write`.
//!   A validator that only checks field names against `IMPORTS` would pass it.
//!
//! `isoplane_abi::imports::is_declared` requires *both* to match, and this crate
//! exists to prove that it does.

use core::future::Future;

use isoplane_guest::{Ctx, Handler, Request, Response};

// NOT in `isoplane_abi::imports::IMPORTS`. Right module, wrong field.
#[cfg(not(feature = "foreign-module"))]
#[link(wasm_import_module = "isoplane")]
unsafe extern "C" {
    fn fs_open(ptr: i32, len: i32) -> i32;
}

// NOT in the world at all. Wrong module entirely.
#[cfg(feature = "foreign-module")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    #[link_name = "__wasi_fd_write"]
    fn wasi_fd_write(fd: i32, iovs: i32, len: i32) -> i32;
}

#[cfg(not(feature = "foreign-module"))]
fn smuggled() -> i32 {
    let path = b"/etc/passwd";
    unsafe { fs_open(path.as_ptr() as i32, path.len() as i32) }
}

#[cfg(feature = "foreign-module")]
fn smuggled() -> i32 {
    let buf = b"escape";
    unsafe { wasi_fd_write(1, buf.as_ptr() as i32, buf.len() as i32) }
}

pub struct Sneaky;

impl Handler for Sneaky {
    fn new() -> Self {
        Sneaky
    }
    fn handle(&'static self, _req: Request, _ctx: Ctx) -> impl Future<Output = Response> + 'static {
        async move {
            // Referenced from a live path so the linker cannot garbage-collect
            // the import out of the module -- the point of the fixture is that
            // the import *is present in the binary*.
            let fd = smuggled();
            Response::text(200, format!("fd={fd}"))
        }
    }
}

isoplane_guest::export_handler!(Sneaky);
