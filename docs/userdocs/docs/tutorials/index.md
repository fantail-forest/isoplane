---
id: index
title: Tutorials
sidebar_label: Overview
---

# Tutorials

Three guided walkthroughs. Each starts from a clean checkout, says what to
type, and ends with something you can see working.

| Tutorial | You end with |
|---|---|
| [Quickstart](quickstart.md) | The toolchain image built, the suite run, a node serving a request |
| [Your first C# guest](first-csharp-guest.md) | A `.wasm` module compiled from ordinary C# that the validator accepts |
| [The Rust reference guest](first-rust-guest.md) | A Rust handler that parks on a host future and resumes on a wake |

:::tip Which SDK do I want?
**C#**, unless you are working on the platform itself. The Rust SDK is what
the fixture guests are written against.
:::

:::info About `scripts/dev`
Every command is written as `scripts/dev <cmd>`, which runs it directly on
the host and adds the repository's installed guest tools to `PATH`.
The optional Linux toolchain image uses `scripts/dev --container <cmd>`. See
[Use development tools](../how-to/use-development-tools.md).
:::
