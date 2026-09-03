# `guest-sdks/dotnet` — the C# WISP guest SDK

One SDK, referenced by every .NET guest.

| | |
|---|---|
| `Wisp.Sdk.csproj` | The library. Compiles to IL and names no tenant type. |
| `Wisp.Guest.props` | Every critical guest build setting, imported by each guest's `.csproj`. |
| `build-guest.sh` | The one build driver. A guest's `build.sh` is a two-line wrapper. |
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
scripts/dev guests/<guest>/build.sh [dotnet publish args...]
```

The driver compiles the PAL with the WASI SDK's clang, publishes the guest's
single `.csproj`, and then **fails the build** if the module imports anything
outside module `wisp` — the import list is the capability manifest, and a new
BCL reference can pull in a new wasi import.

## The module is judged by the build that produced it

The SDK publish integration configures validation of the linked module.
Its diagnostics distinguish rejection from a missing verdict. Keep the module
memory maximum within the application's configured runtime limits.

This is the **publish** pipeline, not `Build`: NativeAOT compilation hangs off
`Publish`, so `dotnet build` on a guest compiles to IL and produces no module
at all. It says so rather than passing quietly.


Errors are verdicts and warnings are the absence of one: a developer who has
the SDK and not yet the CLI gets a module and a loud warning, not a failed
build. `-warnaserror:WISP0002` makes it a hard gate for CI.

## The other SDK

[The Rust helpers](../rust) support internal fixtures. They
evolve with those tests and belong to the separate guest Cargo workspace.

## Guests in this repo

| | |
|---|---|
| [`../../guests/dotnet`](../../guests/dotnet) | Managed fixtures used by host and SDK tests. |

## SDK APIs

Use the [managed API source](Sdk/) and [host bindings](Abi/) for the interfaces.
The adapter and fixture projects show the supported entrypoint setup for this
revision.
