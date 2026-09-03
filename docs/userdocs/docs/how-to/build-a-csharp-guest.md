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
  Wisp.Sdk.csproj         the SDK library; names no tenant type
  Wisp.Guest.props        the guest build settings, imported by each guest
  build-guest.sh          the build driver; a guest's build.sh wraps it
  native/wisp_pal.c       the PAL shim
  Abi/                    the import declarations, the exports, the wire codec
  Runtime/                the continuation queue and the future table
  Sdk/                    HTTP request, response, context and routing

guests/dotnet/            the reference guest
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
scripts/dev guests/dotnet/check.sh                  # the hostcheck harness
scripts/dev guests/dotnet/check.sh --wasi=trap      # every non-`wisp` import traps if called
scripts/dev guests/dotnet/check.sh --density 200    # marginal RSS per instance
```

wasi-sdk lives in the `wisp-dev` image with `WASI_SDK_PATH` set;
`build-guest.sh` refuses, naming the fix, if it is absent. NuGet packages live
in the `wisp-nuget` volume, so only the first build pays restore.

### Build flavours

`WISP_OUT=<basename>` changes the output filename, so flavours can coexist
under `out/`:

```bash
WISP_OUT=wisp_guest_dotnet_compat  scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
WISP_OUT=wisp_guest_dotnet_entropy scripts/dev guests/dotnet/build.sh -p:WispEntropyProbe=true
```

| Property | Effect |
|---|---|
| `-p:WispMemoryMaxBytes=N` | The declared memory maximum; `67108864` sets a 64 MiB ceiling. The .NET runtime reserves a share at start-up, leaving little headroom |
| `-p:WispCompatProbe=true` | Adds LINQ, source-generated `Regex` and source-generated `System.Text.Json`, which widen the BCL surface the PAL has to cover |
| `-p:WispEntropyProbe=true` | Adds the clone-uniqueness probe, for `check.sh --entropy-clone` |
| `-p:NativeDebugSymbols=true` | Keeps symbol names, for finding a stray import |

## Settings not to clean up

`guest-sdks/dotnet/Wisp.Guest.props` carries them, with the reason beside
each, so a guest's own `.csproj` is an import, a project reference and an
assembly name. Three decide whether a flat core module comes out at all:

| Setting | Failure mode if removed |
|---|---|
| No `<PublishAot>` | That path looks for a NativeAOT runtime package that does not exist for `wasi-wasm`; `PublishTrimmed` plus `SelfContained` is the entry point |
| `<MSBuildEnableWorkloadResolver>false</…>` | The Mono `wasi-experimental` workload otherwise claims the runtime identifier and injects its interpreter |
| `<LinkerFlavor>lld</LinkerFlavor>` | The default linker componentizes the output and rejects named host imports for not being a WIT interface |

Plus two linker arguments: `--export=__stack_pointer`, so the hostcheck
harness can read the shadow stack pointer, and
`--max-memory=$(WispMemoryMaxBytes)`, which overrides the toolchain's own
2 GiB maximum. `<OutputType>library</OutputType>` makes the module a reactor
with named exports rather than a command, and the GC settings are baked in
because the PAL answers the environment with an empty list.

## The PAL shim

NativeAOT-LLVM's wasi-libc reaches `wasi:*` imports through undefined C
symbols; some the runtime calls while coming up. `native/wisp_pal.c` defines
them, answering the clock, entropy and environment from imports already in
`wisp_abi::imports::IMPORTS` and aborting by name on everything else, so the
module's import list is `wisp` and nothing else. The set covers the BCL
surface the guests so far touch, and `build-guest.sh` fails the build if a
new import appears; see
[Debug a rejected module](debug-a-rejected-module.md#the-c-case-a-new-wasi-import-appeared).

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
initialized sample image. It reports repeated supported fields in those samples,
not a cryptographic uniqueness guarantee. Values and generator state already
written to guest memory remain captured.

## Pinned versions

`Wisp.Guest.props` pins the ILCompiler.LLVM package, and the `Dockerfile` pins
the .NET SDK channel and the wasi-sdk release the package expects. The
experimental feed publishes builds that are not mutually compatible, so an
unpinned float is a build that breaks on someone else's machine.
