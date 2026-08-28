---
id: cli-wisp
title: wisp
sidebar_position: 4
---

# `wisp`

The WISP CLI

```
wisp [OPTIONS] <COMMAND>
```

| Option | Default | Meaning |
|---|---|---|
| `--no-color` | off |  |
| `--json` | off | Emit machine-readable JSON instead of the human rendering |

| Command | Purpose |
|---|---|
| [`wisp doctor`](#wisp-doctor) | Print the capability manifest the validator derives from a module |

`--no-color`, `--json` are global: they apply to every subcommand below and are not repeated in each table. Every command also takes `--help`.

## `wisp doctor`

Print the capability manifest the validator derives from a module

```
wisp doctor [OPTIONS] <MODULE>
```

| Argument | Required | Meaning |
|---|---|---|
| `<MODULE>` | yes |  |

## Build features

From `crates/wisp-cli/Cargo.toml`, the manifest cargo reads.

| Feature | Default | Enables |
|---|---|---|
| `control` | off | `wisp-control` |
