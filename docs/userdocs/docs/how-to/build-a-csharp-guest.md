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

`HttpClient` uses the runtime's `wasi:http` client, delegated to the host's
outbound HTTP capability under its destination policy. TLS terminates at the host.

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
dotnet build guests/dotnet/src/WispGuest.csproj      # IL and IDE diagnostics
dotnet publish guests/dotnet/src/WispGuest.csproj -c Release
bash guests/dotnet/build.sh                        # publish and copy into out/
```

The SDK compiles the PAL and checks imports as part of publication. On macOS,
install the .NET 10 SDK and start Docker Desktop or a compatible local Docker
runtime. The first publish provisions the pinned Linux compiler image and
NuGet cache. Later builds reuse those downloads, with separate intermediate
files for each invocation. Output belongs to the developer, and the build's
container is removed when publication finishes or is cancelled.

`dotnet publish -p:WispOffline=true` disables compiler networking and requires
cached inputs. A missing image or package produces a diagnostic. For automatic
native execution and source reload, see the [quickstart](../tutorials/quickstart.md).

On Linux, install wasi-sdk 29 and wasm-tools alongside .NET, and set
`WASI_SDK_PATH`. Publishing continues to use those native tools.

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

## The runtime the module is built on

Both profiles compile against a .NET runtime built from source by
`guest-runtime/build.sh`: the Mono runtime pack for the interpreter and the
NativeAOT framework pack the pinned ILCompiler.LLVM compiles against, from
the same `dotnet/runtimelab` commit as the compiler. The same patch series
integrates each with the host: the host-driven event loop, socket readiness
through libc's `poll`, asynchronous name resolution, OpenSSL as the
cryptography backend, and the network error codes libc drops; the runtime's
own `wasi:http` client is delegated as it is. The build
publishes both packs to a local feed whose `Wisp.GuestRuntime.props` names
each pack's version and digest; the SDK refuses a pack whose digest differs
from the feed's and records the identity it linked beside every module
(`<module>.wasm.runtime`). A project outside this checkout names its feed with
`WispGuestRuntimeFeed` or `WISP_GUEST_RUNTIME_FEED`.

## The PAL shim

wasi-libc reaches `wasi:*` imports through undefined C symbols; some the
runtime calls while coming up. `native/wisp_pal.c` defines the ones the host
interface answers differently, the clock, entropy, environment and empty
filesystem, through `wisp_abi::imports::IMPORTS`, and wraps the socket calls
onto the host's sockets. What remains in the module's import list is `wisp`
plus the `wasi:*` functions the engine's PAL adapter delegates by its
canonical table; anything outside that set fails the publish through
`wisp doctor`, see
[Debug a rejected module](debug-a-rejected-module.md#the-c-case-a-new-wasi-import-appeared).

The disk contains only an empty `/` directory. Relative paths start there;
no host directories or inherited file descriptors are available. Reads of
missing files return not-found errors. Attempts to create, write, rename,
delete or create links return read-only errors. Invalid descriptors are rejected
before either operation. .NET preserves its usual no-op behavior when deleting
a missing file. The root’s parent is the root itself; traversal cannot reach
anything outside it. No writable directory exists, so a no-space result is
not needed. `Path.GetTempPath()` supplies a conventional path, not a writable
location. Embedded resources and compiled framework metadata remain available
through the managed runtime.

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

The same applies to a generator your own startup code seeds once and then
only stirs (`new Random(seed)`, a library's own DRBG): created before the
snapshot, it replays the same sequence in every clone. Do not retain one
across pre-initialisation, or recreate it on `Wisp.Activation.Activated`,
which the SDK raises once on the first dispatch of each activation and not
again after a wake from hibernation:

```csharp
Wisp.Activation.Activated += () =>
    _random = new Random(RandomNumberGenerator.GetInt32(int.MaxValue));
```

## Pinned versions

`Wisp.Guest.props` pins the experimental ILCompiler.LLVM package.
`guest-sdks/dotnet/Build/Dockerfile` pins the macOS compiler image's .NET SDK,
wasi-sdk and wasm-tools versions. The
experimental feed publishes builds that are not mutually compatible, so an
unpinned float is a build that breaks on someone else's machine.
