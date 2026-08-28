---
id: validator
title: Validator rules
sidebar_position: 15
---

# Validator rules

Deploy-time validation runs on the module **bytes**, in the daemon, before
anything is instantiated. It is the enforcement point for the capability-only
claim, and it is intentionally decidable from the bytes alone.

Validation checks the module against configured limits.

## V1 — the capability surface

Any import outside the declared set is an **attempted capability**, and the
module is rejected. There is no dynamic-linking escape hatch, and nested
instantiation is treated as one.

| Code | Meaning |
|---|---|
| `V1UndeclaredImport` | An import whose module is not `wisp`, or whose field is not in the declared set. Also raised for component types that fail to match the world. |
| `V1MissingExport` | A required guest export is missing: `wisp_alloc`, `wisp_handle`, `wisp_wake`, `memory` |
| `V1SignatureMismatch` | A declared import, or a required export, has the wrong core-wasm signature |

Signature checking matters as much as name checking: a mismatched signature
would let the engine read a scalar as a pointer, i.e. hand the host an
attacker-chosen offset into guest memory.

## V2 — the engine profile

The binary must not require a WebAssembly proposal the engine has disabled.
This is not implemented as a scan for instructions: the engine's own bytecode
validator runs with the platform's feature set, so a module using a disabled
proposal **fails validation outright**.

Disabled:

| Proposal | Why |
|---|---|
| `threads`, `shared_everything_threads` | No threads. One logical thread per instance is what makes the empty-stack invariant checkable, and `memory.atomic.wait*` is the only in-sandbox blocking instruction. |
| `relaxed_simd` | A deterministic SIMD subset only; relaxed SIMD's results are implementation-defined |
| `exceptions`, `legacy_exceptions` | Beyond the profile |
| `tail_call` | Tail-calls-to-imports tricks sit outside the world |

Because this runs the real validator, a module that fails V2 still produces a
complete report for the other rules — the structural pass runs first.

An encoding that would hide an undeclared import from V1 behind a proposal V2
rejects is caught by both.

## V3 — memory shape

| Requirement |
|---|
| Exactly one linear memory |
| Not shared |
| A declared maximum, no larger than the configured ceiling |

Code: `V3MemoryShape`. An **imported** memory is still a memory for this rule.

Both failure directions are common in practice:

- **No declared maximum.** Rust's wasm linker emits none by default, leaving
  the rule with nothing to check.
- **A maximum far too large.** NativeAOT-LLVM's own link line declares 2 GiB,
  which can exceed the configured ceiling.


## V4 — structural resource bombs

Heuristics over function count, local count, table size, and total
custom-section bytes. Code: `V4ResourceBomb`.

Hitting this usually means an unstripped debug build.

## Where to run it

| Tool | Runs |
|---|---|
| `wisp doctor <module>` | The derived manifest. With `--features control`, the real validator too. |
| `wisp deploy` | Client-side manifest check, then the real validator in the daemon on the bytes it sent |
| `wisp-validate` | The real validator, standalone. See [Other tools](cli-other.md#wisp-validate). |

The regression corpus is `crates/wisp-control/tests/validator_corpus.rs`, and
`guests/` ships two purpose-built rejection fixtures — `undeclared-import.wasm`
(right module, undeclared field) and `foreign-module.wasm` (undeclared module
entirely). Both are otherwise complete, well-formed guests, so a rejection can
only be about the import.

## Why the client's check is not the authority

`wisp deploy` runs the same manifest analysis and refuses to send a module that
would be rejected. That is a convenience. The **module bytes** are sent to the
daemon, which validates them itself, because the platform treats the artifact
as hostile from byte zero — and it can only do that if it has the artifact.
Sending only a hash would leave the daemon trusting the client's own
validation, and the compile-time layer is advisory precisely because the tenant
controls the toolchain.
