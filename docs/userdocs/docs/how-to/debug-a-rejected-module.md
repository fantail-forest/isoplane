---
id: debug-a-rejected-module
title: Debug a rejected module
sidebar_position: 5
---

# Debug a rejected module

Deploy-time validation refuses a module for one of four reasons. This guide
maps a rejection to its cause and its fix.

## Start with `wisp doctor`

```bash
wisp doctor guests/out/quotes-api.wasm
```

`doctor` needs no running node. It prints the module's size and blake3, the
declared capabilities grouped by family with their future class and policy
flags, any undeclared imports, and any missing exports, then the verdict of
`wisp_control::validate` against the configured limits.
It exits 2 if the module would be rejected. `--json` gives a machine-readable
report:

```bash
wisp doctor mod.wasm --json | jq '{rejected, undeclared, missing_required_exports}'
```

A build of the CLI with `--no-default-features` drops the validator, and then
the derived manifest is all `doctor` reports.

:::tip C# guests are checked by their own build
`guest-sdks/dotnet/Wisp.Guest.props` runs `wisp doctor` on the module as soon
as `wasm-ld` produces it, so a violation fails `dotnet publish` instead of
waiting for a deploy. The check is mandatory: a publish without a `wisp` CLI
to judge the module fails too, with the error naming where to install it or
which property points at it.
:::

## The four rules

### V1: an undeclared import is an attempted capability

Every import must be in module `wisp` and must be one of the declared names.

```
✗ V1UndeclaredImport  env::__wasi_fd_write
✗ V1UndeclaredImport  wisp::fs_open
```

V1 also covers missing required exports (`wisp_alloc`, `wisp_handle`,
`wisp_wake`, `memory`) and signature mismatches on a declared import or a
required export.

| Cause | Fix |
|---|---|
| A Rust guest pulled in `std::fs`, `std::net`, or a crate that did | Remove it; the SDK offers no equivalent |
| A C# guest dragged in a new BCL surface | Define the `__wasm_import_*` symbol in `guest-sdks/dotnet/native/wisp_pal.c`; see [below](#the-c-case-a-new-wasi-import-appeared) |
| A hand-written module | Check the name against `wisp_abi::imports::IMPORTS` |

### V2: the binary requires a disabled engine feature

Threads, shared memory, atomics, relaxed SIMD and exceptions are off, so a
module using them fails bytecode validation. Build for
`wasm32-unknown-unknown` without `+atomics` and without
`-C target-feature=+relaxed-simd`.

### V3: memory shape

Exactly one linear memory, not shared, with a declared maximum no larger than
the configured ceiling. The two common causes are opposite mistakes:

- No declared maximum at all. Rust's wasm linker emits none by default;
  `guests/.cargo/config.toml` passes `--max-memory`.
- A maximum that is far too large. NativeAOT-LLVM's own link line declares
  2 GiB; `Wisp.Guest.props` appends `-Wl,--max-memory=$(WispMemoryMaxBytes)`
  after it, and `wasm-ld` honours the last one.

Validation compares the declared memory maximum with the configured ceiling.
A module that exceeds that ceiling is rejected.

### V4: structural resource bombs

Heuristics on function count, local counts, table sizes and custom-section
bytes. Hitting this usually means an unstripped debug build; the guests'
release profile strips.

## The C# case: a new wasi import appeared

`build-guest.sh` fails the build before you reach a deploy rejection. The PAL
shim covers the BCL surface the guests so far touch, and a new library can
reach past it. Find the symbol name and signature:

```bash
./scripts/dev guests/dotnet/build.sh -p:NativeDebugSymbols=true
./scripts/dev bash -lc "wasm-tools print guests/dotnet/out/wisp_guest_dotnet.wasm | grep '(import \"wasi'"
```

Then define its `__wasm_import_*` symbol in
`guest-sdks/dotnet/native/wisp_pal.c` so `wasm-ld` resolves it locally.

To find out whether a stubbed import is ever reached, run the checker with
everything trapping:

```bash
./scripts/dev guests/dotnet/check.sh --wasi=trap
./scripts/dev guests/dotnet/check.sh --wasi=stub    # enumerate what is reachable
```

## Cross-check with the validator

```bash
# The C# guest ships a binary that runs wisp_control::validate directly
./scripts/dev cargo run --release --features validator --bin wisp-validate \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml

# Or deploy and read the refusal
wisp deploy --module mod.wasm --app scratch --tenant scratch
```

The validator's regression corpus is `crates/wisp-control/tests/validator_corpus.rs`,
and `guests/` ships two rejection fixtures: `undeclared-import.wasm` (right
module, undeclared field) and `foreign-module.wasm` (undeclared module). Both
are otherwise complete guests, so a rejection can only be about the import.
