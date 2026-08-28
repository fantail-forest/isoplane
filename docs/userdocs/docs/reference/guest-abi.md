---
id: guest-abi
title: Guest ABI
sidebar_position: 9
---

# Guest ABI

The host and guest exchange flat core-WebAssembly calls and postcard records.
Host imports use the `wisp` module name. The capability definitions and core
signatures live in `wisp_abi::imports`; the guest export definitions live in
`wisp_abi::imports::exports`.

## Exports the guest provides

`wisp_alloc` hands the host a guest-memory buffer. The host copies a record
into that buffer and hands it back through `wisp_handle` for an HTTP dispatch
or `wisp_wake` for a host-future completion. Those entrypoints return a packed
`PollStatus`. The guest owns and frees its buffers; host pointers do not become
guest pointers.

The required exports are these three functions and `memory`.
`wisp_gc_hint` and `wisp_initialize` are optional hooks for memory reclamation
and application initialization during preparation. Their core signatures and
required/optional classification are defined in the ABI crate.

## Entrypoint outcomes

`wisp_abi::status::PollStatus` encodes its tag in the low 32 bits of a `u64`
and its payload in the high 32 bits. A done result carries a response handle
(zero means no response); a trapped result carries a trap code. A pending
result means the guest returned with an empty invocation stack to await a
host-owned wake. Host-owned I/O and other resources may remain live while
the guest is parked.

The status type and trap codes are defined in `crates/wisp-abi/src/status.rs`.

## Dispatches, completions and responses

`DispatchRecord` carries an HTTP request together with the dispatch identity,
trace context, cancellation-future handle, CPU budget and dispatch-start
clock value. `HttpRequest` contains the method, URL, path, query, headers and
request bytes. Their definitions are in `crates/wisp-abi/src/dispatch.rs`.

`ResultRecord` delivers either encoded result bytes or a `FutureError` through
`wisp_wake`. The SDK decodes successful bytes for the operation that created
the future. Errors remain explicit values, including cancellation, timeout,
host-policy refusal and I/O failure; see `crates/wisp-abi/src/result.rs`.

`ResponseHead` carries status, headers, inline response bytes and a streaming
flag. The guest submits it through `resp_new`; streamed chunks use
`resp_write`. Completing a dispatch and owning any remaining host-side work
are separate lifecycle responsibilities.

## Encoding

The shared `wisp-abi` crate defines the records used on both sides of the
boundary. Records are encoded with [postcard](https://docs.rs/postcard).
Use the crate definitions when constructing or decoding a record; the core
call signature alone does not describe its encoded fields.
