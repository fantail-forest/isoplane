---
id: build-a-csharp-guest
title: Build a C# guest
sidebar_position: 9
---

# Build a C# guest

The SDK source under `guest-sdks/dotnet/` defines the managed APIs.
This guide covers the project layout and build options.

## What the SDK reaches

The SDK's `Abi/Imports.cs` binds the host contract. A module links the imports
its compiled code reaches; the validator checks the resulting module. The SDK source describes the managed surface at this revision.

## Layout

`guest-sdks/dotnet/` is the SDK library. The reference guest uses a
`ProjectReference` and imports the shared guest build settings.

```
guest-sdks/dotnet/
  Wisp.Sdk.csproj         the SDK library - names no tenant type
  Wisp.Guest.props        the critical guest build settings, imported by each guest
  build-guest.sh          the one build driver; a guest's build.sh wraps it
  native/wisp_pal.c       the PAL shim - read it before touching the build
  Abi/Imports.cs          the `wisp` import module
  Abi/Exports.cs          Exports<TRouter>: the five host exports, generic over the router
  Abi/PollStatus.cs       the packed u64
  Abi/Postcard.cs         a minimal postcard codec (six shapes, not a port)
  Abi/Records.cs          DispatchRecord / ResultRecord decoding
  Runtime/                the continuation queue and the future table
  Sdk/Bindings.cs         HTTP request, response and context
  Sdk/Routers.cs          IWispRouter and HandlerRouter

guests/dotnet/            the reference guest
  src/Entrypoint.cs       the alias that names this module's entrypoint
  src/App/Api.cs          the handler
  src/App/Compat.cs       the compatibility probe
  check.sh                HTTP response and sample memory-copy checks
  hostcheck/src/bin/      wisp-validate: runs the REAL validator
```

## Declaring the entrypoint

A NativeAOT entry project supplies one alias above its export methods:

```csharp
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.Api>>;
```

`Api` implements `IWispHandler` with `Task<Response> HandleAsync(Request, Ctx)`.
The `Wisp.Durable` namespace here contains the HTTP handler/router interface.
`Exports<TRouter>` delegates the host exports to that router. The reference
guest's `src/Entrypoint.cs` supplies the five forwarding methods, including the
initialization signature declared by the host interface at this revision.

NativeAOT-LLVM emits these exports from the attributes. The forwarding methods
live in a nongeneric class because `UnmanagedCallersOnly` cannot annotate a
method on a generic class.

## Building

```bash
scripts/dev guests/dotnet/build.sh                  # -> out/wisp_guest_dotnet.wasm
scripts/dev guests/dotnet/check.sh                  # assert the ABI and the empty stack
scripts/dev guests/dotnet/check.sh --wasi=trap      # the decisive negative test
scripts/dev guests/dotnet/check.sh --density 200    # per-instance RSS
```

wasi-sdk 29.0 lives in the `wisp-dev` image with `WASI_SDK_PATH` preset;
`build.sh` will fetch it if it is genuinely absent, but that fallback cannot
succeed under `scripts/dev`, which runs as your uid while `/opt` is root-owned.
That is why it moved into the image. NuGet packages live in the `wisp-nuget`
Docker volume, so only the first build pays restore.

### Build flavours

`WISP_OUT=<basename>` changes the output filename, so flavours can coexist
under `out/` and be compared against each other:

```bash
WISP_OUT=wisp_guest_dotnet_compat  scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
WISP_OUT=wisp_guest_dotnet_entropy scripts/dev guests/dotnet/build.sh -p:WispEntropyProbe=true
```

| Property | Effect |
|---|---|
| `-p:WispMemoryMaxBytes=N` | The declared memory maximum. `67108864` sets a 64 MiB ceiling. |
| `-p:WispCompatProbe=true` | Adds LINQ, source-generated `Regex` and source-generated `System.Text.Json`. Off by default; enables the compatibility probe. |
| `-p:WispEntropyProbe=true` | Adds the clone-uniqueness probe |
| `-p:NativeDebugSymbols=true` | Keeps symbol names, for finding a stray import |

## Three settings you must not "clean up"

They live in `guest-sdks/dotnet/Wisp.Guest.props`, which every guest imports —
so a guest's own `.csproj` is four lines and none of this is in it. That is
deliberate: these are not stylistic, and each one, removed, breaks the build in
a way that names nothing useful. Importing them rather than copying them means
a correction lands once.

The build depends on each of these:

| Setting | Failure mode if removed |
|---|---|
| **No `<PublishAot>`** | NativeAOT-LLVM is not integrated into the .NET SDK. `PublishAot` sends the build down the shipping NativeAOT path, which looks for `Microsoft.NETCore.App.Runtime.NativeAOT.wasi-wasm` — a package that does not exist. `PublishTrimmed` + `SelfContained` is the documented entry point. |
| `<MSBuildEnableWorkloadResolver>false</…>` | The `wasi-experimental` (Mono) workload claims the `wasi-wasm` RID and injects a 12 MB Mono interpreter. |
| `<LinkerFlavor>lld</LinkerFlavor>` | The default linker componentizes the output and then rejects named host imports for not being a WIT interface. Plain `wasm-ld` gives the flat core module WISP loads. |

Plus two linker arguments:

```xml
<LinkerArg Include="-Wl,--export=__stack_pointer" />
<LinkerArg Include="-Wl,--max-memory=$(WispMemoryMaxBytes)" />
```

The first exports the shadow stack pointer so the sample harness can compare it
before and after a completed call. The second overrides the toolchain's own
2 GiB maximum, which validator rule V3 rejects when it exceeds the configured ceiling;
`wasm-ld` honours the last `--max-memory` flag, and ours is appended after the
toolchain's.

`<OutputType>library</OutputType>` — not `Exe` — because a WISP guest is a
reactor with named exports, not a command. That gets `-mexec-model=reactor` and
`_initialize`.

The runtime cannot read the environment, so every GC knob is baked in at build
time (`System.GC.Server=false`, `System.GC.Concurrent=false`).

A guest's own `.csproj` holds only what is that guest's: its `AssemblyName`, the
import and project reference above, any `EmbeddedResource`, and any build
flavours it defines. See
[Starting a project from scratch](../tutorials/first-csharp-guest.md#starting-a-project-from-scratch).

## The PAL shim

Out of the box NativeAOT-LLVM leaves **32 `wasi:*` imports** in the module:
ambient filesystem, stdio, a fine-grained clock, a second entropy source. Every
one is an undeclared capability and the validator rejects all 32 under rule V1.
Three of them are genuinely called, so trapping them is not an option either.

`guest-sdks/dotnet/native/wisp_pal.c` defines the `__wasm_import_*` symbols wasi-libc leaves
undefined, so `wasm-ld` resolves them locally and emits **no** wasi import. The
three live ones are re-expressed as `wisp::clock_now_coarse`,
`wisp::entropy_fill` and a locally computed empty environment — all already
declared. The module's whole import list becomes `wisp`, and the validator
accepts it.

**The PAL is a closure over the BCL surface your code touches, not a fixed
list.** Adding LINQ, `Regex` and `System.Text.Json` pulled in 12 more wasi
imports. `build.sh` therefore fails the build if anything outside `wisp`
survives, and tells you how to find the missing symbol. If you add a NuGet
reference and the build fails here, that is the gate working.

## What `check.sh` asserts

The standalone harness checks the HTTP fixture's required exports, one exported
linear memory and its actual response handle and body. The sample's shadow stack
pointer is equal before and after completion. It copies the completed linear
memory and stack pointer into another instance without `_initialize`, then checks
another request and reuse of that restored instance.

These are sample response and state-copy checks. They do not qualify arbitrary
C# async code or host-owned resource capture. `--wasi=trap` traps non-host imports
reached by this execution.

For a module built with `WispCompatProbe=true`, use `check.sh --compat`; its
response is the compatibility diagnostic instead of the ordinary echoed path.
For `--entropy-clone`, build with `WispEntropyProbe=true` into the harness's
normal output path before running the diagnostic.

## Initialization policy

Preparation applies host-selected capability and resource limits. A unit that
reaches a refused capability is deferred within the attempt limit. Retrying is
not transactional: earlier permitted host effects may run again. Reusing an
initialized image copies its captured configuration and random state.

## Captured randomness

`check.sh --entropy-clone` compares the probe output from two copies of one
initialized sample image. It reports which supported fields repeat in those
samples; it is not a cryptographic uniqueness guarantee.

Values and generator state already written to guest memory remain captured.
A new preparation can draw different entropy while reuse of that image copies
what it holds.

## Pinned versions

| | |
|---|---|
| `Microsoft.DotNet.ILCompiler.LLVM` | `10.0.0-rc.1.26357.1` (dnceng `dotnet-experimental`) |
| wasi-sdk | 29.0 (clang 21.1.4) — the version the ILCompiler package expects |
| .NET SDK | 10.0.400 |

Pinned in `guest-sdks/dotnet/Wisp.Guest.props` on purpose: the experimental feed publishes
builds that are not mutually compatible, and an unpinned float is a build that
breaks on someone else's machine on a different day.
