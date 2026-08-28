---
id: index
title: Reference
sidebar_label: Overview
---

# Reference

Look-up material. Every page here describes what something *is*, not what to do
with it — for that, see the [how-to guides](../how-to/index.md).

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
