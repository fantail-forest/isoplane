---
id: sdk-csharp
title: C# SDK
sidebar_position: 12
---

# C# SDK

The SDK provides request and response types, async host bindings and guest
entrypoint support. Application code requests I/O through the host interface;
while awaiting a pending operation, the guest returns control to the host.
Parked guest memory and host-owned I/O resources still have a cost.

## HTTP handlers

`Request`, `Response` and `Ctx` describe the handler's request, response and
host context. Their source documentation defines the available members and
binding errors. Build and run the reference handler with
[Your first C# guest](../tutorials/first-csharp-guest.md).

## Handler routing {#wisp-durable}

The entry project supplies the handler and its host-facing exports. Keep the
SDK and guest entrypoint from the same checkout. See
[Your first C# guest](../tutorials/first-csharp-guest.md) for the entrypoint and
build configuration used by this version.

## API documentation

Public type and member documentation lives with the SDK source. Consult those
comments for the API in this checkout; the build guide describes how the guest
module is produced and admitted by the host.

## Pinned versions

Toolchain and package versions are declared in the repository's build files.
Use those files from the same checkout as the SDK and guest project.
