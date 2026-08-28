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

:::info Everything runs in the sandbox
Anything that executes guest WebAssembly — and **every performance number** —
goes through `scripts/dev`, which runs inside a
[sysbox](https://github.com/nestybox/sysbox)-isolated container. This is not a
style preference: the prototype uses `mmap`, `madvise`, `clock_gettime` and
procfs in ways that should not be pointed at a host kernel namespace, and a
number taken on the host was not taken where the system runs. See
[Use the sandbox](../how-to/use-the-sandbox.md).
:::
