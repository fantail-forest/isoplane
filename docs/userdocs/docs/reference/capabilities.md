---
id: capabilities
title: Capabilities
sidebar_position: 10
---

# Capabilities

Host import declarations identify functions a module can request. Permitted PAL
imports can also delegate to host capabilities; `wisp doctor` reports both direct
imports and those delegated capabilities.

The canonical `wisp_abi::imports::IMPORTS` module defines **18** imports; 6 carry the initialization flag and 1 carry the sensitive flag. The tables below list that metadata.

## Reading the columns

| Column | Meaning |
|---|---|
| **Kind** | `sync` returns a result directly. `future` returns a handle for later polling. |
| **Init** | Reports the import definition's `init_visible` flag. The preparation sandbox's policy determines the calls available in a particular initialization context. |
| **Sens** | Marks potentially sensitive results. Guest memory can retain copies; this flag does not remove them from captured state. |

The buffer column reports declared `BufSpec` metadata; it is not a complete
data-flow analysis of guest memory.

## Core

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `abort` | `(ptr, len) -> ()` | guest → host | sync | ✔ |  |
| `init_unit_begin` | `(i32) -> ()` | — | sync | ✔ |  |
| `init_unit_deferred` | `(i32) -> i32` | — | sync | ✔ |  |
| `init_hash_seed` | `(ptr, cap) -> ()` | **host → guest** | sync | ✔ | ✔ |
| `init_config_get` | `(ptr, cap) -> i32` | **host → guest** | sync | ✔ |  |

Preparation can attribute a forbidden import to the active initialization unit
and retry with that unit deferred. Restarting initialization does not undo host
effects that an earlier permitted call already performed. Values retained in
initialized guest memory become part of the prepared state.

## Observability

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `log_emit` | `(i32, ptr, len) -> ()` | guest → host | sync | ✔ |  |
| `trace_span_begin` | `(ptr, len) -> i32` | guest → host | sync |  |  |
| `trace_span_end` | `(i32) -> ()` | — | sync |  |  |

## Clock and entropy

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `clock_now_coarse` | `() -> i64` | — | sync |  |  |
| `clock_deadline_in` | `(i64) -> i64` | — | future |  |  |
| `entropy_fill` | `(ptr, cap) -> ()` | **host → guest** | sync |  |  |

## Outbound HTTP

Outbound HTTP uses the host request path and its egress policy.

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `http_fetch` | `(ptr, len) -> i64` | guest → host | future |  |  |
| `http_body_read` | `(i32, i32) -> i64` | — | future |  |  |

## Inbound response

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `resp_new` | `(ptr, len) -> i32` | guest → host | sync |  |  |
| `resp_write` | `(i32, ptr, len) -> i64` | guest → host | future |  |  |
| `resp_finish` | `(i32) -> ()` | — | sync |  |  |

## Dispatch context

| Import | Signature | Buffer | Kind | Init | Sens |
|---|---|---|---|---|---|
| `budget_remaining_ns` | `() -> i64` | — | sync |  |  |
| `unsafe_to_trap` | `(i32) -> ()` | — | sync |  |  |

## Initialization and buffer metadata

Initialization metadata flags **6** imports: `abort`, `init_unit_begin`, `init_unit_deferred`, `init_hash_seed`, `init_config_get`, `log_emit`. Sensitive imports among them: `init_hash_seed`.
Imports with a declared output buffer: 3 — `init_hash_seed`, `init_config_get`, `entropy_fill`.
These lists describe metadata. They do not state what guest code retains in memory; returned values and values derived from them can enter captured guest state.

## Inspecting a module

Run `wisp doctor` to inspect a module's direct imports and permitted PAL
capabilities. The import metadata and validation verdict describe the module;
they do not prove that application values are absent from captured guest memory.
