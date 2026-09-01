---
id: index
title: Reference
sidebar_label: Overview
---

# Reference

Look-up material. Every page here describes what something *is*, not what to do
with it — for that, see the [how-to guides](../how-to/index.md).

## The tables are generated

Most of the tables on these pages are emitted from the definitions they
document — the limits from `wisp_abi::config`, the capability tables from
`wisp_abi::imports::IMPORTS`, the `wispd.toml` defaults from the daemon's own
serde defaults, the metric list from what the exporter actually renders. They
sit between `@generated` markers and the prose around them is written by hand.

```bash
cargo run  -p wisp-docs            # regenerate, after changing the code
cargo test -p wisp-docs            # fail if a checked-in page has drifted
```

A number in a marked region is not worth editing here: the next run puts it
back. Change the code, then regenerate.

The three `cli-*.md` pages are the exception. Their clap definitions live
inside `main.rs`, where no library can read them, and their curated structure
is worth more than a `--help` dump — so they are hand-written, and a separate
test runs the real binaries and checks that every flag is documented, that
every documented flag exists, and that no stated default contradicts the one
clap declares:

```bash
cargo test -p wisp-docs --test cli_pages_match_the_binaries
```

## Configuration

| Page | Covers |
|---|---|
| [`wisp.toml`](wisp-toml.md) | App manifest: handler lifecycle, outbound HTTP allowlist and numeric limits |

## Command-line tools

| Page | Covers |
|---|---|
| [`wisp`](cli-wisp.md) | Command usage and options |
| [Other tools](cli-other.md) | `scripts/dev` and guest builders |

## Interfaces

| Page | Covers |
|---|---|
| [Runtime architecture](architecture.md) | What runs where, on which ports, speaking which protocols |
| [Guest ABI](guest-abi.md) | The five exports, `PollStatus`, `DispatchRecord`, `ResultRecord` |
| [Capabilities](capabilities.md) | Declared imports and their initialization policy |

## SDKs

| Page | Covers |
|---|---|
| [C# SDK](sdk-csharp.md) | **The target ecosystem.** `guests/dotnet`: HTTP handlers, project settings and the initialization environment |
| [Rust SDK](sdk-rust.md) | `wisp-guest`: the reference implementation of the contract — traits, bindings, concurrency, pre-init |

## Tables

| Page | Covers |
|---|---|
| [Metrics](metrics.md) | Every series on `/metrics` |
| [Validator rules](validator.md) | Module validation rules and errors |
