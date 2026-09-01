---
id: sdk-rust
title: Rust SDK
sidebar_position: 12
---

# Rust guest helpers

`guest-sdks/rust` supports internal fixtures, hostile guests and runtime
measurements. It has no customer SDK compatibility or feature-parity commitment.
The application SDK is described in [C# SDK](sdk-csharp.md).

## Project setup

Rust guests belong to the separate `guests/` Cargo workspace and target
`wasm32-unknown-unknown`. Build the repository fixtures with:

```bash
scripts/dev guests/build.sh
```

Their manifests select `cdylib` output and the workspace's size-oriented release
profile. The workspace target configuration supplies the module memory maximum
required by validation.

## HTTP handlers and host futures

The retained handler helpers connect an HTTP handler to the host entrypoints.
Host I/O is represented by futures: a pending guest returns to the host, which
owns the operation and schedules its wake. Guest memory and host-owned resources
remain live while the operation is pending.

The supported interfaces and their contracts are documented beside their
implementations in `guest-sdks/rust`. Use the fixture source at the same checkout
for examples of the entrypoint macros, async helpers and capability bindings.
