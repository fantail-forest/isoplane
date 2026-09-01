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

## The codes

{/* @generated:begin violation-codes */}
| Rule | Code | Meaning |
|---|---|---|
| **V1** | `V1UndeclaredImport` | An import whose module is not `wisp`, or whose field is not in the declared set. Also raised for component types that fail to match the world, and for nested instantiation, which is a linking escape hatch. |
| **V1** | `V1MissingExport` | A required guest export is missing |
| **V1** | `V1SignatureMismatch` | A declared import, or a required export, has the wrong core-wasm signature. Name-only matching would be theatre: the world is a set of *types*, and half a type check is not a type check. |
| **V2** | `V2ForbiddenFeature` | The binary requires a WebAssembly proposal the engine profile has disabled |
| **V3** | `V3MemoryShape` | More than one linear memory, a shared memory, or a declared memory maximum above the configured ceiling. An **imported** memory is still a memory for this rule. |
| **V4** | `V4ResourceBomb` | A structural resource-bomb heuristic tripped. Hitting this usually means an unstripped debug build. |
{/* @generated:end violation-codes */}

## V1 — the capability surface

Any import outside the declared set is an **attempted capability**, and the
module is rejected. There is no dynamic-linking escape hatch, and nested
instantiation is treated as one. The declared set is
[Capabilities](capabilities.md).

{/* @generated:begin required-exports */}
Required: `wisp_alloc`, `wisp_handle`, `wisp_wake`, `memory`. Optional but strongly recommended: `wisp_gc_hint`, `wisp_initialize`.
{/* @generated:end required-exports */}

Signature checking matters as much as name checking: a mismatched signature
would let the engine read a scalar as a pointer, i.e. hand the host an
attacker-chosen offset into guest memory.

## V2 — the engine profile

The binary must not require a WebAssembly proposal the engine has disabled.
This is not implemented as a scan for instructions: the engine's own bytecode
validator runs with the platform's feature set, so a module using a disabled
proposal **fails validation outright**. The feature set is declared once, in
`wisp_control::validator::ENGINE_FEATURES`, and mirrored into the engine's
configuration — same rule, two places, one definition.

Disabled:

{/* @generated:begin v2-disabled */}
| Proposal | Why it is off |
|---|---|
| `compact_imports` | Not in the profile |
| `component_model` | The documented deviation: the same world, lowered to a flat core-wasm ABI under module `wisp`. Its sub-proposals — async, threading, GC, values, error contexts and the rest — are off with it, and are not listed separately here. |
| `custom_descriptors` | Not in the profile |
| `custom_page_sizes` | One page size, so a delta's page arithmetic is the same on both sides |
| `exceptions` | Beyond the profile |
| `function_references` | Typed function references. Not in the baseline, and not needed: the guest ABI is five exports and a flat import table. |
| `gc` | The guest ships its own GC inside linear memory, so engine GC objects would be a second heap the platform cannot see — and all tenant state must be visible in linear memory, globals and tables |
| `gc_types` | Turning this off also removes `externref`, which is right for a second reason: no host pointer ever enters guest memory, and every host resource is an opaque `u32` handle |
| `legacy_exceptions` | Beyond the profile |
| `memory64` | One 32-bit linear memory |
| `memory_control` | Not in the profile |
| `multi_memory` | Exactly one linear memory. V3 says the same thing structurally; both are checked. |
| `relaxed_simd` | A deterministic SIMD subset only; relaxed SIMD's results are host-dependent, which would make `wisp replay` a lie |
| `shared_everything_threads` | The same argument as `THREADS`, for the newer proposal |
| `stack_switching` | The ABI is callback-CPS. Engine-suspended stacks are exactly what the empty-stack invariant exists to forbid, so the proposal that reintroduces them is off at the bytecode level. |
| `tail_call` | A tail call to an import would be a trick outside the declared world. Conservative reading; the V1 allowlist still covers the other half. |
| `threads` | No threads. One logical thread per instance is what makes the empty-stack invariant checkable, and `memory.atomic.wait*` is the only in-sandbox blocking instruction — with the feature off it fails bytecode validation rather than being policed at runtime. |
| `wide_arithmetic` | Not in the profile |
{/* @generated:end v2-disabled */}

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

An **imported** memory is still a memory for this rule.

Both failure directions are common in practice:

- **No declared maximum.** Rust's wasm linker emits none by default, leaving
  the rule with nothing to check.
- **A maximum far too large.** NativeAOT-LLVM's own link line declares 2 GiB,
  which can exceed the configured ceiling.

{/* @generated:begin v3-ceilings */}

A second gate sits in front of pre-initialization: a module may not declare more than 512 MB of linear memory at init time, checked before a single guest instruction runs. It covers a caller that reaches the pre-initializer directly without a prior `validate`.
{/* @generated:end v3-ceilings */}

## V4 — structural resource bombs

Heuristics over the module's structure. Hitting this usually means an
unstripped debug build.

{/* @generated:begin v4-limits */}
| Limit | Value | Bounds |
|---|---:|---|
| `MAX_FUNCTIONS` | 200,000 | Functions in the module |
| `MAX_LOCALS_PER_FUNCTION` | 50,000 | Locals declared by any one function |
| `MAX_TABLE_ELEMENTS` | 1,000,000 | Elements across all tables |
| `MAX_CUSTOM_SECTION_BYTES` | 16 MB | Total custom-section bytes — what makes a 50 MB "module" that is 49 MB of debug info visible |
| `MAX_IMPORTS` | 512 | Import entries |
| `MAX_EXPORTS` | 512 | Export entries |
| `MAX_TYPES` | 100,000 | Type entries |
| `MAX_ELEMENT_SEGMENTS` | 100,000 | Element segments |
| `MAX_DATA_SEGMENTS` | 100,000 | Data segments |
{/* @generated:end v4-limits */}

## Where to run it

| Tool | Runs |
|---|---|
| `wisp doctor <module>` | The derived manifest and then the real validator, checked against configured limits. Exits 2 on a rejection. |
| `dotnet publish` of a C# guest | The same `wisp doctor`, run by the SDK on the linker's output. See `guest-sdks/dotnet/Wisp.Guest.props`. |
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
