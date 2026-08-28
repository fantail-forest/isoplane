---
id: first-rust-guest
title: The Rust reference guest
sidebar_position: 3
---

# The Rust reference guest

:::info Internal fixture support
WISP targets .NET applications; see [Your first C# guest](first-csharp-guest.md).
The Rust SDK in `guests/wisp-guest` supports repository fixtures. It has no customer SDK
compatibility or feature-parity commitment.
:::

This walkthrough adds a small HTTP handler with an asynchronous host timer.

## 1. Build the Rust fixtures

```bash
./scripts/dev guests/build.sh
```

`guests/` is a separate Cargo workspace targeting `wasm32-unknown-unknown`.
The build script copies completed modules into `guests/out/`.

## 2. Create a crate

The example uses `hello` as its crate name. If that fixture already exists,
choose a new name and use it consistently below. Add the crate to the members
in `guests/Cargo.toml` and the `GUESTS` list in `guests/build.sh`.
Create `guests/hello/Cargo.toml`:

```toml
[package]
name = "hello"
version.workspace = true
edition.workspace = true
license-file.workspace = true
description = "HTTP and timer guest fixture"

[lib]
crate-type = ["cdylib"]

[dependencies]
wisp-guest.workspace = true
```

The workspace supplies package metadata and the SDK path. `cdylib` produces a
module with callable exports rather than a Rust library archive.

## 3. Write the handler

Create `guests/hello/src/lib.rs`:

```rust
use wisp_guest::{Clock, Ctx, Handler, Request, Response};

struct Hello;

impl Handler for Hello {
    fn new() -> Self {
        Hello
    }

    fn handle(
        &'static self,
        req: Request,
        _ctx: Ctx,
    ) -> impl core::future::Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/park" => {
                    // The host owns the timer. Awaiting it lets this invocation return.
                    if let Err(error) = Clock::delay(50).await {
                        return Response::text(500, format!("timer failed: {error}"));
                    }
                    Response::text(200, "woke up")
                }
                _ => Response::text(200, "hello from wasm"),
            }
        }
    }
}

wisp_guest::export_handler!(Hello);
```

`Handler::new` constructs the per-instance handler object. `handle` receives
the decoded HTTP request and dispatch context. The `/park` branch awaits a
host-owned timer and handles its failure; other paths return an inline reply.

`export_handler!` supplies `wisp_alloc`, `wisp_handle` and `wisp_wake`, plus the
optional initialization and GC-hint hooks. The toolchain exports linear memory.
When the timer is pending, the SDK returns `Pending` to the host. A later
`wisp_wake` call delivers its completion on a fresh invocation stack.

Build the new member with the same script:

```bash
./scripts/dev guests/build.sh
```

## 4. Inspect the module

```bash
./scripts/dev wasm-tools validate guests/out/hello.wasm
./scripts/dev wasm-tools print guests/out/hello.wasm
./scripts/dev cargo run -p wisp-cli -- doctor guests/out/hello.wasm
```

The WebAssembly import section records the host functions this module needs.
`guests/.cargo/config.toml` supplies a maximum linear-memory size of 128 MiB;
the initial number of pages depends on the linked program. Doctor checks the
module against the validator's supported imports, signatures and resource
bounds.

## Read the implementation

The fixture SDK definitions and helpers are in `guests/wisp-guest`.
`crates/wisp-abi/src/imports.rs` defines host imports and guest export
signatures. `dispatch.rs`, `result.rs` and `status.rs` in the same crate define
the records and entrypoint outcomes used by this example.

See [Guest ABI](../reference/guest-abi.md) for the boundary and encoding.
