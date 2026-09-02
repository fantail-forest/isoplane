---
id: index
title: Tutorials
sidebar_label: Overview
---

# Tutorials

Four guided walkthroughs. Each one starts from a clean checkout, tells you
exactly what to type, and ends with something you can see working. They are
written to be followed in order the first time, but each stands alone.

| Tutorial | You end with |
|---|---|
| [Quickstart](quickstart.md) | The sandbox built, a node serving a request |
| [Your first C# guest](first-csharp-guest.md) | A `.wasm` module compiled from ordinary C# that the validator accepts |
| [The Rust reference guest](first-rust-guest.md) | A Rust handler that parks on a host future and resumes on a wake |

:::tip Which SDK do I want?
**C#**, unless you have a specific reason not to. .NET is the guest ecosystem
WISP targets. The Rust SDK is the reference implementation of the guest
contract — read it when you are working on the platform, writing a fixture, or
want to see the ABI without a language runtime in the way.
:::

:::info `scripts/dev` is the path of least setup
Every command in these tutorials is written as `scripts/dev <cmd>`, which runs
it inside `wisp-dev:latest` — the image carrying Rust's wasm targets, .NET 10,
`wasm-tools` and wasi-sdk, started under a
[sysbox](https://github.com/nestybox/sysbox)-isolated runtime. That is the
sensible place to instantiate untrusted guest code, and the only place where
the toolchain is guaranteed to be present.

It is not a requirement of the code. Drop the `scripts/dev` prefix and the same
commands run wherever the toolchain is installed; performance numbers taken
there are labelled with the machine they came off, like every other run. See
[Use the sandbox](../how-to/use-the-sandbox.md).
:::
