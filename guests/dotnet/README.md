# `guests/dotnet`

The reference C# guest: a Handler written in ordinary C# against
`guest-sdks/dotnet`, compiled with NativeAOT-LLVM to a core wasm module that
imports from the `wisp` module and nothing else.

## Build and check

```sh
scripts/dev guests/dotnet/build.sh                  # -> out/wisp_guest_dotnet.wasm
scripts/dev guests/dotnet/check.sh                  # the hostcheck harness
scripts/dev guests/dotnet/check.sh --wasi=trap      # every non-`wisp` import traps if called
scripts/dev guests/dotnet/check.sh --density 200    # marginal RSS per instance
```

`build.sh` wraps the SDK's `build-guest.sh`, which compiles the PAL shim,
publishes the project, and fails the build if any import outside module
`wisp` survives. `-p:WispMemoryMaxBytes=67108864` declares a 64 MiB memory ceiling.

## Flavours

One project builds focused probe modules; `WISP_OUT` names their output.
The build script selects the fixture set for this revision.

| Module                            | Built with                                    | What it is                                                                                |
| --------------------------------- | --------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `wisp_guest_dotnet`               |                                               | The Handler.                                                                              |
| `wisp_guest_dotnet_compat`        | `-p:WispCompatProbe=true`                     | The Handler plus LINQ, source-generated `Regex` and `System.Text.Json`, which widen the BCL surface the PAL has to cover. |
| `wisp_guest_dotnet_entropy`       | `-p:WispEntropyProbe=true`                    | Reports five sources of randomness, for `check.sh --entropy-clone`.                        |
| `wisp_guest_dotnet_stock`         | `-p:WispPalObject=`                           | Built without the PAL shim, so it carries the raw `wasi:*` imports; the import gate refuses it by design, and `wisp-engine`'s `dotnet_pal` test reads it. |

The clone probe compares initialized values after copying a prepared guest
image. Stateful generators created before capture can repeat across those
copies; later requests to host entropy remain separate operations.

## The PAL

The PAL connects runtime operations to the declared host interface. Validation
and linking determine which host capabilities the module can reach. See the
SDK source and build inputs for the implementation used by this revision.

## Build settings

The project and imported SDK build files define the compiler, linker and
runtime settings for this revision. Keep those inputs together when building
these fixtures; the WebAssembly module is checked against the host contract.

## Host checks

The [hostcheck harness](hostcheck/) exercises this fixture through host calls.
Its assertions apply to the specific guest state and resource setup under test;
they do not establish portability of arbitrary host resources or live stacks.

## Layout

`src/` contains the fixture entrypoint and probes. The project selects their
build settings. The hostcheck harness lives in `hostcheck/`.
