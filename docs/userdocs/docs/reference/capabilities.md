---
id: capabilities
title: Capabilities
sidebar_position: 10
---

# Capabilities

Host import declarations identify functions a module can request. Permitted PAL
imports can also delegate to host capabilities; `wisp doctor` reports both direct
imports and those delegated capabilities.

{/* @generated:begin import-preamble */}
The `wisp` import definitions are recorded in `wisp_abi::imports::IMPORTS`. The tables list **18** definitions; 6 carry the initialization flag and 1 carry the sensitive flag. PAL imports can delegate to host capabilities, so inspect both direct imports and PAL declarations with `wisp doctor`.
{/* @generated:end import-preamble */}

## Reading the columns

{/* @generated:begin import-column-key */}
| Column | Meaning |
|---|---|
| **Kind** | `sync` returns a result directly. `future` returns a future handle for later polling. |
| **Init** | Reports the import definition's `init_visible` flag. The preparation sandbox's policy determines the calls available in a particular initialization context. |
| **Sens** | Marks potentially sensitive results. Guest memory can retain copies; this flag does not remove them from captured state. |
{/* @generated:end import-column-key */}

The buffer column reports declared `BufSpec` metadata; it is not a complete
data-flow analysis of guest memory.

## Core

{/* @generated:begin imports-core */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `abort` | `(ptr, len) -> ()` | guest → host | sync | ✔ |  | guest panic; available at init so a failed pre-initialization reports usefully rather than trapping anonymously |
| `init_unit_begin` | `(i32) -> ()` | — | sync | ✔ |  | names the init unit about to run, so a trap inside it can be attributed to that unit and deferred |
| `init_unit_deferred` | `(i32) -> i32` | — | sync | ✔ |  | asks whether this unit was deferred on a previous attempt; non-zero means skip it on the replay |
| `init_hash_seed` | `(ptr, cap) -> ()` | **host → guest** | sync | ✔ | ✔ | A per-attempt string-hash seed, separate from host entropy requests. |
| `init_config_get` | `(ptr, cap) -> i32` | **host → guest** | sync | ✔ |  | the init-visible config blob, so a config parse happens once at deploy time instead of once per activation; returns the blob's FULL length, so a guest can size its buffer with a zero-length probe |
{/* @generated:end imports-core */}

Preparation can attribute a forbidden import to the active initialization unit
and retry with that unit deferred. Restarting initialization does not undo host
effects that an earlier permitted call already performed. Values retained in
initialized guest memory become part of the prepared state.

## Observability

{/* @generated:begin imports-observe */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `log_emit` | `(i32, ptr, len) -> ()` | guest → host | sync | ✔ |  | structured log at `trace=0` .. `error=4`; the CPU counts against the dispatch budget, which is what stops log-as-compute abuse |
| `trace_span_begin` | `(ptr, len) -> i32` | guest → host | sync |  |  | opens a trace span and returns its id, for correlating work inside one dispatch |
| `trace_span_end` | `(i32) -> ()` | — | sync |  |  | closes the span with that id; unbalanced spans are the guest's problem, not the host's |
{/* @generated:end imports-observe */}

## Clock and entropy

{/* @generated:begin imports-clock-entropy */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `clock_now_coarse` | `() -> i64` | — | sync |  |  | Reads the host coarse clock; use host deadline futures to wait. |
| `clock_deadline_in` | `(i64) -> i64` | — | future |  |  | a timer as a host future, so waiting is never a guest loop burning budget |
| `entropy_fill` | `(ptr, cap) -> ()` | **host → guest** | sync |  |  | Host CSPRNG bytes for this request; values already stored in guest memory remain part of captured state. |
{/* @generated:end imports-clock-entropy */}

## Outbound HTTP

Outbound HTTP uses the host request path and its egress policy.

{/* @generated:begin imports-http */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `http_fetch` | `(ptr, len) -> i64` | guest → host | future |  |  | Issues an outbound request through the host egress policy and completes with its response head. |
| `http_body_read` | `(i32, i32) -> i64` | — | future |  |  | Reads the next chunk of a response body through a host-owned future. |
{/* @generated:end imports-http */}

## Inbound response

{/* @generated:begin imports-response */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `resp_new` | `(ptr, len) -> i32` | guest → host | sync |  |  | opens a response from a `ResponseHead` and returns its handle |
| `resp_write` | `(i32, ptr, len) -> i64` | guest → host | future |  |  | appends a streamed chunk to an open response |
| `resp_finish` | `(i32) -> ()` | — | sync |  |  | ends the response, after which the handle is spent |
{/* @generated:end imports-response */}

## Dispatch context

{/* @generated:begin imports-ctx */}
| Import | Signature | Buffer | Kind | Init | Sens | What it is for |
|---|---|---|---|---|---|---|
| `budget_remaining_ns` | `() -> i64` | — | sync |  |  | signed nanoseconds left against this dispatch's CPU budget; negative once the dispatch is in its grace window and still falling, so a guest can tell `at budget` from `deep into grace` and decide how much unwinding it can still afford |
| `unsafe_to_trap` | `(i32) -> ()` | — | sync |  |  | the runtime layer's unsafe-to-trap bit, set around GC phases and scheduler queue mutations. Grace entry defers while it is set and the hard deadline ignores it, so a pinned bit buys exactly the grace window and nothing more |
{/* @generated:end imports-ctx */}

## Initialization and buffer metadata

{/* @generated:begin import-structural-guarantees */}
Initialization metadata flags **6** imports: `abort`, `init_unit_begin`, `init_unit_deferred`, `init_hash_seed`, `init_config_get`, `log_emit`. Sensitive imports among them: `init_hash_seed`.
Imports with a declared output buffer: 3 — `init_hash_seed`, `init_config_get`, `entropy_fill`.
These lists describe metadata. They do not state what guest code retains in memory; returned values and values derived from them can enter captured guest state.
{/* @generated:end import-structural-guarantees */}

## Inspecting a module

Run `wisp doctor` to inspect a module's direct imports and permitted PAL
capabilities. The import metadata and validation verdict describe the module;
they do not prove that application values are absent from captured guest memory.
