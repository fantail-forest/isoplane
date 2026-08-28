---
id: quickstart
title: Quickstart
sidebar_position: 1
---

# Quickstart

The optional development image supplies the repository's toolchain. Build
it below, or use locally installed tools and continue with a guest tutorial.

## 1. Build the toolchain image

```bash
docker build -t wisp-dev:latest .
```

This installs Rust with the `wasm32-unknown-unknown` and `wasm32-wasip1`
targets, .NET 10, `wasm-tools`, and wasi-sdk 29.0. It also pre-creates the
cache mountpoints owned by your uid, which is what makes
`--user $(id -u):$(id -g)` usable under sysbox's user-namespace remap.

If your uid is not 1000, pass it:

```bash
docker build -t wisp-dev:latest \
    --build-arg HOST_UID=$(id -u) --build-arg HOST_GID=$(id -g) .
```

Check that the container answers:

```bash
./scripts/dev --status
./scripts/dev bash -lc 'rustc --version && dotnet --version && wasm-tools --version && $WASI_SDK_PATH/bin/clang --version | head -1'
```

## 2. Stop the optional container

```bash
./scripts/dev --stop
```

`scripts/dev --reap` removes your own stopped containers and nothing
else — it never touches a container it did not create.

## Where next

- Write a guest: [Your first C# guest](first-csharp-guest.md) or
  [Your first Rust guest](first-rust-guest.md).
- Understand the execution model: [The four invariants](../explanation/invariants.md).
