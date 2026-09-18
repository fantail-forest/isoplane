# `guests/dotnet`

The reference C# guest: a Handler written in ordinary C# against
`guest-sdks/dotnet`, compiled with NativeAOT-LLVM to a core wasm module that
imports from the `isoplane` module and nothing else.

## Build and check

```sh
scripts/dev guests/dotnet/build.sh                  # -> out/isoplane_guest_dotnet.wasm
scripts/dev guests/dotnet/check.sh                  # the hostcheck harness
scripts/dev guests/dotnet/check.sh --wasi=trap      # every non-`isoplane` import traps if called
scripts/dev guests/dotnet/check.sh --density 200    # marginal RSS per instance
```

`build.sh` wraps the SDK's `build-guest.sh`, which compiles the PAL shim,
publishes the project, and fails the build if any import outside module
`isoplane` survives. `-p:IsoplaneMemoryMaxBytes=67108864` declares a 64 MiB memory ceiling.

## Flavours

One project builds focused probe modules; `ISOPLANE_OUT` names their output.
The build script selects the fixture set for this revision.

| Module                            | Built with                                    | What it is                                                                                |
| --------------------------------- | --------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `isoplane_guest_dotnet`               |                                               | The Handler.                                                                              |
| `isoplane_guest_dotnet_compat`        | `-p:IsoplaneCompatProbe=true`                     | The Handler plus LINQ, source-generated `Regex` and `System.Text.Json`, which widen the BCL surface the PAL has to cover. |
| `isoplane_guest_dotnet_entropy`       | `-p:IsoplaneEntropyProbe=true`                    | Reports five sources of randomness, for `check.sh --entropy-clone`.                        |
| `isoplane_guest_dotnet_trust`         | `-p:IsoplaneTrustProbe=true`                      | Lists the machine root store at construction (pre-initialisation) and on request, for `isoplane-control`'s `dotnet_trust_rotation` test. |
| `isoplane_guest_dotnet_stock`         | `-p:IsoplanePalObject=`                           | Built without the PAL shim, so it carries the raw `wasi:*` imports; the import gate refuses it by design, and `isoplane-engine`'s `dotnet_pal` test reads it. |
| `isoplane_guest_dotnet_request` | `-p:IsoplaneRequestProbe=true` | Checks HTTP request fields after a host await; driven through the real node for every standard HTTP method. |

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
