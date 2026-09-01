---
id: cli-wisp
title: wisp
sidebar_position: 4
---

# `wisp`

This page is generated from the command's own definition. To change it, change the doc comments in the source and run `cargo run -p wisp-docs`.

WebAssembly validation.

```
wisp [OPTIONS] <COMMAND>
```

| Option | Default | Meaning |
|---|---|---|
| `--no-color` | off | Plain output. Colour is auto-disabled when stdout is not a terminal, so this is only needed to suppress it at a terminal |
| `--json` | off | Emit machine-readable JSON instead of the human rendering |

| Command | Purpose |
|---|---|
| [`wisp doctor`](#wisp-doctor) | Inspect a WebAssembly module's host interface and validation status |

`--no-color`, `--json` are global: they apply to every subcommand below and are not repeated in each table. Every command also takes `--help`.

## `wisp doctor`

Inspect a WebAssembly module's host interface and validation status

```
wisp doctor [OPTIONS] <MODULE>
```

| Argument | Required | Meaning |
|---|---|---|
| `<MODULE>` | yes | The `.wasm` to inspect |

| Option | Default | Meaning |
|---|---|---|
| `--guest-memory-bytes <GUEST_MEMORY_BYTES>` | none | Override the application memory ceiling in bytes |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Command failed; the reason is on stderr |
| `2` | `wisp doctor` only: the module would be rejected |

## Build features

From `crates/wisp-cli/Cargo.toml`, the manifest cargo reads.

| Feature | Default | Enables |
|---|---|---|
| `control` | **on** | `wisp-control` |
