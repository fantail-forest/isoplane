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

{/* @generated:begin guest-exports */}
| Export | Core-wasm signature | Required | Purpose |
|---|---|---|---|
| `wisp_alloc` | `(i32 size, i32 align) -> i32` | ✔ | Host→guest buffer handoff. The **guest** frees the buffer when the host hands it back in `wisp_handle`/`wisp_wake`. |
| `wisp_handle` | `(i32 ptr, i32 len) -> i64` | ✔ | A new top-level work item. `(ptr,len)` is a postcard `DispatchRecord`. Returns a packed `PollStatus`. |
| `wisp_wake` | `(i64 future, i32 ptr, i32 len) -> i64` | ✔ | Completion of a previously returned future. `(ptr,len)` is a postcard `ResultRecord`. Returns a packed `PollStatus`. |
| `memory` | — | ✔ | The guest linear memory |
| `wisp_gc_hint` | `(i32 pressure) -> ()` | optional | Pre-snapshot compaction hint |
| `wisp_initialize` | `() -> ()` | optional | Optional application initialization entrypoint, called during preparation. |
{/* @generated:end guest-exports */}

## Entrypoint outcomes

`wisp_abi::status::PollStatus` encodes its tag in the low 32 bits of a `u64`
and its payload in the high 32 bits. A done result carries a response handle
(zero means no response); a trapped result carries a trap code. A pending
result means the guest returned with an empty invocation stack to await a
host-owned wake. Host-owned I/O and other resources may remain live while
the guest is parked.

The status type and trap codes are defined in `crates/wisp-abi/src/status.rs`.

### Trap codes

{/* @generated:begin trap-codes */}
| Code | Value | Meaning |
|---|---:|---|
| `Unknown` | 0 | No code was supplied, or one this build does not recognise |
| `Panic` | 1 | Guest panic |
| `OutOfMemory` | 2 | Allocation failed, or the memory ceiling was hit |
| `CapabilityViolation` | 3 | A capability was invoked that this module did not declare, or that the init sandbox forbids |
| `Protocol` | 4 | Malformed dispatch or result encoding |
| `TooManyFutures` | 5 | The simultaneous-pending-futures cap was exceeded |
{/* @generated:end trap-codes */}

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

## Fixed constants

{/* @generated:begin abi-constants */}
| Constant | Value | What it fixes |
|---|---:|---|
| `ABI_REV` | 1 | The version of the guest host-interface contract. |
| engine page | 4,096 bytes | Engine memory-page size |
| wasm page | 65,536 bytes | The unit a module declares its memory minimum and maximum in |
{/* @generated:end abi-constants */}

## Encoding

The shared `wisp-abi` crate defines the records used on both sides of the
boundary. Records are encoded with [postcard](https://docs.rs/postcard).
Use the crate definitions when constructing or decoding a record; the core
call signature alone does not describe its encoded fields.
