---
id: index
title: WISP
sidebar_label: Start here
slug: /
---

# WISP

WISP is a WASM-based serverless platform where the isolation boundary is the
WebAssembly sandbox rather than the CLR or a hypervisor. Instances park on I/O
with an **empty stack**.

## The target is .NET

WISP exists to run **ordinary C# as-is** — async/await, LINQ, generics, source
generators, spans, `System.Text.Json` — compiled to WebAssembly. Not a dialect,
not a transpiler. Whether that actually works is the claim that decides whether
WISP is a product or an interesting runtime, so it is the claim everything else
here is built to support.

So the guest ecosystem this platform is for is .NET. There is also a **Rust
SDK**, and it matters, but it is a *reference implementation of the guest
contract*: a few thousand lines with no bundled runtime, used to prove the ABI
is implementable and to support guest fixtures. It is not the audience.

Read the two SDK pages with that asymmetry in mind:

| | |
|---|---|
| [C# SDK](reference/sdk-csharp.md) | **The target.** What a tenant writes. |
| [Rust SDK](reference/sdk-rust.md) | The reference implementation of the contract and its fixture guests. |

Where a mechanism is currently more complete on the Rust side, this
documentation says so on the page that documents it rather than implying
parity.

## Where to go

This documentation follows the [Diátaxis](https://diataxis.fr) framework. The
four sections answer four different questions, and mixing them is what makes
documentation unreadable:

| If you want to… | Go to |
|---|---|
| Get something running for the first time | [Tutorials](tutorials/index.md) |
| Accomplish a specific task you already understand | [How-to guides](how-to/index.md) |
| Look up a flag, a port, a record layout, a limit | [Reference](reference/index.md) |
| Understand why the system is shaped this way | [Explanation](explanation/index.md) |

The fastest path from a clean checkout to something you can poke is the
[Quickstart](tutorials/quickstart.md).
