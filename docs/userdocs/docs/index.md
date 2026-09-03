---
id: index
title: WISP
sidebar_label: Start here
slug: /
---

# WISP

WISP hosts customer-written .NET applications as WebAssembly guests. The
isolation boundary is the WASM sandbox, not the CLR and not a hypervisor.
Guests are pre-initialised at deploy time into a memory template, activate
copy-on-write from it, and park with an empty stack while waiting on host I/O.

## The target is .NET

A tenant writes ordinary C#, with async/await, LINQ, generics and source
generators, and compiles it with NativeAOT-LLVM to a core wasm module against
the `Wisp.Sdk` library. There is also a Rust SDK, which the platform's own
fixture guests are written against; it is not the audience.

## Where to go

| If you want to… | Go to |
|---|---|
| Get something running for the first time | [Tutorials](tutorials/index.md) |
| Accomplish a specific task you already understand | [How-to guides](how-to/index.md) |
| Look up a port, an endpoint or a header | [Reference](reference/index.md) |
| Understand why the system is shaped this way | [Explanation](explanation/index.md) |

The fastest path from a clean checkout to something you can poke is the
[Quickstart](tutorials/quickstart.md).
