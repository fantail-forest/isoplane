---
id: first-csharp-guest
title: Your first C# guest
sidebar_position: 2
---

# Your first C# guest

A `Task<Response>` HTTP handler compiled by NativeAOT-LLVM to a core
WebAssembly module. This tutorial builds the repository's reference fixture
and checks its HTTP response and sample state-copy behavior.

## 1. Build the reference guest

```bash
./scripts/dev guests/dotnet/build.sh
```

wasi-sdk 29.0 ships **in the image** and `WASI_SDK_PATH` is already set, so the
build goes straight to compiling the PAL shim and publishing. The first run
restores NuGet packages into the `wisp-nuget` volume; subsequent runs skip that
too. The output lands at `guests/dotnet/out/wisp_guest_dotnet.wasm`.

The build prints the module's imported functions. The validator checks them
against the host interface; use the validator step below after changing the
project's dependencies.

## 2. Check the reference fixture

```bash
./scripts/dev guests/dotnet/check.sh
./scripts/dev guests/dotnet/check.sh --wasi=trap
./scripts/dev guests/dotnet/check.sh --density 200
```

The harness checks required exports, the returned HTTP response handle and
body, and the sample's shadow stack pointer after completion. It then copies
the completed sample's linear memory and stack pointer into another instance,
without rerunning `_initialize`, and checks another request and instance reuse.
The round trip covers this fixture's state; host capture has its own state
and resource rules.

With `--wasi=trap`, a non-host import reached by this execution traps. This
checks the executed sample path.

## 3. Write the handler

The tenant-facing file is `guests/dotnet/src/App/Api.cs`, and the interesting
thing about it is that there is nothing interesting in it:

```csharp
using System.Threading.Tasks;

namespace Wisp.App;

public sealed class Api
{
    public Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        // The host supplies req; Text creates a UTF-8 HTTP response.
        return Task.FromResult(Response.Text(req.Path));
    }
}
```

The reference harness expects the echoed request path. Rebuild after an edit;
keep that response contract when running these fixture checks. `Request`,
`Response` and `Ctx` are described in the [C# SDK reference](../reference/sdk-csharp.md).

## 4. Run it through the validator

`wisp doctor` derives the capability manifest from the bytes alone. The
authority for rules V1–V4 is `wisp-control::validate`, and the C# guest ships
a small binary that runs the real one:

```bash
./scripts/dev cargo run --release --features validator --bin wisp-validate \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml
```

Expect `ACCEPTED`. If you add a NuGet reference and it turns into a rejection,
that is [the gate working](#why-the-pal-shim-exists), not a bug.

## Starting a project from scratch

The SDK is a library under `guest-sdks/dotnet/`. A NativeAOT guest supplies
its project, handler and export entrypoint. The reference guest shows this
layout:

```
guests/your-guest/
  build.sh                invokes ../../guest-sdks/dotnet/build-guest.sh
  src/YourGuest.csproj    imports Wisp.Guest.props and references Wisp.Sdk.csproj
  src/Entrypoint.cs       declares the HTTP HandlerRouter and five exports
  src/App/YourClass.cs    implements the HTTP handler
```

The `.csproj` is short because everything critical lives in the SDK's
`Wisp.Guest.props`, which you import:

```xml
<Import Project="../../../guest-sdks/dotnet/Wisp.Guest.props" />

<PropertyGroup>
  <AssemblyName>your_guest</AssemblyName>
</PropertyGroup>

<ItemGroup>
  <ProjectReference Include="../../../guest-sdks/dotnet/Wisp.Sdk.csproj" />
</ItemGroup>
```

That props file is where the three eccentric-looking settings live — no
`<PublishAot>`, `<MSBuildEnableWorkloadResolver>false</...>`,
`<LinkerFlavor>lld</LinkerFlavor>` — along with the two linker arguments that
decide whether the module passes validator rule V3. They are documented in
[C# SDK reference](../reference/sdk-csharp.md).
Importing them rather than copying them is the point: a correction lands once.

To set a 64 MiB maximum, build with `-p:WispMemoryMaxBytes=67108864`.
The declared maximum bounds memory growth; it is not occupied-memory measurement.

### Shipping a front end

An `EmbeddedResource` survives NativeAOT-LLVM and `TrimMode=full`, so a guest
can carry a real front-end bundle and hand it back from a trigger:

```xml
<ItemGroup>
  <EmbeddedResource Include="../web/dist/index.html" LogicalName="app.index.html" />
</ItemGroup>
```

```csharp
using Stream? st = typeof(YourClass).Assembly.GetManifestResourceStream("app.index.html");
```

A handler can return an embedded asset in its HTTP response.

## Why the PAL shim exists

Out of the box NativeAOT-LLVM leaves **32 `wasi:*` imports** in the module:
ambient filesystem, stdio, a fine-grained clock, a second entropy source.
Every one of them is an undeclared capability, and the validator rejects all
32 under rule V1. Three are genuinely called, so trapping them is not an
option either.

`guest-sdks/dotnet/native/wisp_pal.c` defines the `__wasm_import_*` symbols that
wasi-libc leaves undefined, so `wasm-ld` resolves them locally and emits **no**
wasi import. The three live ones are re-expressed as `wisp::clock_now_coarse`,
`wisp::entropy_fill`, and a locally computed empty environment — all already
in the declared capability set.

**The PAL is a closure over the BCL surface you touch, not a fixed list.**
Adding LINQ, source-generated `Regex` and source-generated `System.Text.Json`
pulled in 12 more wasi imports. `build.sh` fails the build if anything outside
`wisp` survives, and prints the command that shows you the missing symbol's
name and signature:

```bash
./scripts/dev guests/dotnet/build.sh -p:NativeDebugSymbols=true
./scripts/dev bash -lc "wasm-tools print guests/dotnet/out/wisp_guest_dotnet.wasm | grep '(import \"wasi'"
```

Then define that symbol in `guest-sdks/dotnet/native/wisp_pal.c`.

## Where next

- [C# SDK reference](../reference/sdk-csharp.md) — the full surface and the build flavours.
- [The guest ABI](../reference/guest-abi.md) — what the five exports actually receive.
- [Why suspension implies an empty stack](../explanation/invariants.md#invariant-3--suspension-implies-an-empty-stack).
