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

`doctor` is the only command that needs no running node: the capability
manifest is answerable from the bytes alone, which is exactly the claim it
exists to make checkable. It prints the module's size and blake3, the declared
capabilities grouped by family with their future class and policy flags
(`sensitive`, `init-visible`), any undeclared imports, and any missing exports.

It exits **2** if the module would be rejected. `--json` gives you a
machine-readable version:

```bash
wisp doctor mod.wasm --json | jq '{rejected, undeclared, missing_required_exports}'
```

:::note doctor runs the real validator
`doctor` prints the manifest it derives from the import section and then the
verdict of `wisp-control::validate` — the authority for rules V1–V4 — judged
against the configured limits. **The validator's verdict is the
exit code**: the derived manifest is presentation, and checks a strict subset
of V1. A build of the CLI with `--no-default-features` drops the validator, and
there the derived manifest is all there is.
:::

:::tip C# guests are checked by their own build
`guest-sdks/dotnet/Wisp.Guest.props` runs `wisp doctor` on the module as soon
as `wasm-ld` produces it, so a violation fails `dotnet publish` instead of
waiting for a deploy. `-p:WispValidate=false` turns it off.
:::

## The four rules

### V1 — an undeclared import is an attempted capability

Every import must be in module `wisp` and must be one of the declared names.
There is no dynamic-linking escape hatch, and nested instantiation is treated
as one.

```
✗ V1UndeclaredImport  env::__wasi_fd_write
✗ V1UndeclaredImport  wisp::fs_open
```

V1 also covers **missing required exports** (`wisp_alloc`, `wisp_handle`,
`wisp_wake`, `memory`) and **signature mismatches** on a declared import or a
required export.

Fixes:

| Cause | Fix |
|---|---|
| A Rust guest pulled in `std::fs`, `std::net`, or a crate that did | Remove it; the SDK deliberately offers no equivalent |
| A C# guest dragged in a new BCL surface | Define the `__wasm_import_*` symbol in `guest-sdks/dotnet/native/wisp_pal.c` — see [below](#the-c-case-a-new-wasi-import-appeared) |
| A hand-written module | Check the name against Capabilities; typos land here |

### V2 — the binary requires a disabled engine feature

Threads, shared memory, atomics, relaxed SIMD and exceptions are **off**, so a
module using them fails bytecode validation outright. This is what makes
`memory.atomic.wait*` — the only in-sandbox blocking instruction —
inexpressible rather than merely discouraged.

Fixes: build for `wasm32-unknown-unknown` without `+atomics`/`+bulk-memory`
threading features, and do not enable `-C target-feature=+relaxed-simd`.

### V3 — memory shape

Exactly one linear memory, not shared, with a declared maximum no larger than
the configured ceiling.

The two most common causes are opposite mistakes:

- **No declared maximum at all.** Rust's wasm linker emits none by default.
  `guests/.cargo/config.toml` passes
  `-C link-arg=--max-memory=134217728`.
- **A maximum that is far too large.** NativeAOT-LLVM's own link line declares
  2 GiB, which can exceed the configured ceiling. The C# project appends
  `-Wl,--max-memory=$(WispMemoryMaxBytes)` after the toolchain's, and
  `wasm-ld` honours the last one.

Validation compares the declared memory maximum with the configured ceiling.
A module that exceeds that ceiling is rejected.

### V4 — structural resource bombs

Heuristics on function count, local count, table size and total custom-section
bytes. Hitting this usually means an unstripped debug build:

```bash
# Rust: the release profile already sets opt-level="z", lto, codegen-units=1
./scripts/dev bash -lc 'cd guests && cargo build --release --target wasm32-unknown-unknown'
```

## The C# case: a new wasi import appeared

`guests/dotnet/build.sh` fails the build before you ever reach a deploy
rejection. The PAL shim is a **closure over the BCL surface your code
touches**, not a fixed list — adding LINQ, source-generated `Regex` and
source-generated `System.Text.Json` pulled in 12 new wasi imports.

Find the symbol name and signature:

```bash
./scripts/dev guests/dotnet/build.sh -p:NativeDebugSymbols=true
./scripts/dev bash -lc "wasm-tools print guests/dotnet/out/wisp_guest_dotnet.wasm | grep '(import \"wasi'"
```

Then define its `__wasm_import_*` symbol in `guest-sdks/dotnet/native/wisp_pal.c`
so `wasm-ld` resolves it locally and emits no import.

To find out whether a stubbed import is ever actually *reached* — an
inspection versus a negative test — run the checker with everything trapping:

```bash
./scripts/dev guests/dotnet/check.sh --wasi=trap
./scripts/dev guests/dotnet/check.sh --wasi=stub    # enumerate what is reachable
```

## Cross-check with the real validator

```bash
# The C# guest ships a binary that runs wisp-control::validate directly
./scripts/dev cargo run --release --features validator --bin wisp-validate \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml

# Or deploy and read the refusal
wisp deploy --module mod.wasm --app scratch
```

The validator's own regression corpus lives in
`crates/wisp-control/tests/validator_corpus.rs`, and `guests/` ships two
purpose-built rejection fixtures: `undeclared-import.wasm` (right module,
undeclared field) and `foreign-module.wasm` (undeclared module entirely). Both
are otherwise complete, well-formed guests, so a rejection can only be about
the import.

## Related

