# `guest-sdks/dotnet` — the C# WISP guest SDK

The shared library used by .NET guests. The separate
[ASP.NET adapter](AspNetCore/README.md) supplies framework hosting and generated exports.

| | |
|---|---|
| `Wisp.Sdk.csproj` | The library. Compiles to IL and names no tenant type. |
| `Wisp.Guest.props` | Every critical guest build setting, imported by each guest's `.csproj`. |
| `build-guest.sh` | A wrapper that publishes and copies a named module into `out/`. |
| `native/wisp_pal.c` | The PAL shim. Read it before touching the build. |
| `Abi/` | The `wisp` imports, the five exports, the postcard codec, the records. |
| `Runtime/` | The continuation queue and the future table. |
| `Sdk/` | Managed HTTP, context and handler/router APIs. |

## HTTP entrypoints

The [host entrypoints](Abi/Exports.cs) route HTTP dispatches through the SDK's
handler/router interfaces. Their [source definitions](Sdk/) describe the
request, response and background-work APIs available at this revision.
An entry project emits one set of host exports.

## Build a guest

```bash
dotnet build guests/dotnet/src/WispGuest.csproj
dotnet publish guests/dotnet/src/WispGuest.csproj -c Release
```

`dotnet build` compiles IL for IDE diagnostics. Publishing compiles the PAL,
links WASM against the source-built runtime pack the local feed offers the
profile (`guest-runtime/build.sh` builds and publishes both packs),
records that pack's identity beside the module, and judges the module with
`wisp doctor`. A guest's `build.sh` uses this same MSBuild integration and
copies the module and its identity into `out/`.

On macOS, install the .NET 10 SDK and start Docker Desktop or a compatible
local Docker runtime. Publishing provisions the Linux compiler image and
NuGet cache on first use. It mounts source read-only, uses separate build
intermediates per invocation, and returns developer-owned output. It removes
its compiler container on completion or cancellation. `wisp dev` uses the
same publication path and executes the guest in the native runtime.

After the first online publish, `dotnet publish -p:WispOffline=true` uses the
cached image and packages with compiler networking disabled. Missing inputs
are errors naming what needs provisioning. `WispOffline=true` in the
environment applies the same policy to builds launched by `wisp dev`.

The cache defaults to the user's application-data directory under `Wisp`;
`-p:WispCompilerCache=/path/to/cache` relocates it. Projects and references are
mounted beneath the nearest ancestor containing `nuget.config`, or the project
directory if there is none. Set `-p:WispBuildRoot=/path/to/source` when project
references or build inputs live outside that tree. The SDK is mounted separately
when needed. Container control operations have a 60-second default timeout,
adjustable with `-p:WispContainerCommandTimeoutSeconds=N`.

`dotnet clean -c Release` removes that configuration's recorded WASM outputs,
including custom publish locations, while retaining shared packages and images.

On Linux, publishing uses the native installed compiler, wasi-sdk 29 and
wasm-tools. Set `WASI_SDK_PATH` to the wasi-sdk installation. The
NativeAOT-LLVM package remains experimental; targeting `net10.0` does not make
it an officially supported .NET WASM distribution.

## The module is judged by the build that produced it

The SDK publish integration configures validation of the linked module.
Its diagnostics distinguish rejection from a missing verdict. Keep the module
memory maximum within the application's configured runtime limits.

This is the **publish** pipeline, not `Build`: NativeAOT compilation hangs off
`Publish`, so `dotnet build` on a guest compiles to IL and produces no module
at all. It says so rather than passing quietly.


The gate is mandatory: a module nothing has judged is not a publication, so
the absence of a verdict fails the publish the same as a rejection. A local
debugging publish (`WispLocalDebug=true`) links a raw artifact the control
plane refuses by design and is the one publish the gate does not judge.

## Randomness after activation

An activation inherits captured guest memory. The BCL's
`RandomNumberGenerator` requests host entropy when generating bytes. Values
already stored in guest memory remain captured. A generator that seeds itself
once and then advances private state (`new Random(seed)` or a library DRBG)
can therefore repeat across copies if initialization created it before capture.
Create that generator after activation, or recreate it on
`Wisp.Activation.Activated`, raised on the first dispatch of each activation:

```csharp
Wisp.Activation.Activated += () => _random = new Random(RandomNumberGenerator.GetInt32(int.MaxValue));
```

## The other SDK

[The Rust helpers](../rust) support internal fixtures. They
evolve with those tests and belong to the separate guest Cargo workspace.

## Guests in this repo

| | |
|---|---|
| [`../../guests/dotnet`](../../guests/dotnet) | Managed fixtures used by host and SDK tests. |

`Request` exposes `Method`, `Url`, `Path`, `Query`, `Headers` and raw `Body`
bytes for every HTTP method, preserving repeated query and header pairs.

## SDK APIs

Use the [managed API source](Sdk/) and [host bindings](Abi/) for the interfaces.
The adapter and fixture projects show the supported entrypoint setup for this
revision.
