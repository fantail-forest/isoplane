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
targets, the .NET SDK, `wasm-tools` and wasi-sdk. It also pre-creates the
cache mountpoints owned by your uid, which is what lets the container run as
you. If your uid is not 1000, pass it:

```bash
docker build -t wisp-dev:latest \
    --build-arg HOST_UID=$(id -u) --build-arg HOST_GID=$(id -g) .
```

Check that the container answers:

```bash
./scripts/dev --status
./scripts/dev bash -lc 'rustc --version && dotnet --version && wasm-tools --version'
```

## 2. Stop the optional container

```bash
./scripts/dev --stop
```

`scripts/dev --reap` removes your own stopped containers and nothing else.

## Where next

- Write a guest: [Your first C# guest](first-csharp-guest.md) or
  [The Rust reference guest](first-rust-guest.md).
- Understand the execution model: [The four invariants](../explanation/invariants.md).
