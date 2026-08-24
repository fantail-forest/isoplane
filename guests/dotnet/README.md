# `guests/dotnet`

C# HTTP handler and runtime probes compiled with NativeAOT-LLVM to a core
WebAssembly module for the host interface.


## Build and check

The commands below use the repository development helper.

```bash
scripts/dev guests/dotnet/build.sh        # -> guests/dotnet/out/wisp_guest_dotnet.wasm
scripts/dev guests/dotnet/check.sh        # runs it under wasmtime, asserts the ABI
scripts/dev guests/dotnet/check.sh --density 200   # per-instance RSS (§6.2)
```

`build.sh` installs wasi-sdk 29.0 on first run if it is missing. NuGet packages
live in the `wisp-nuget` docker volume, so only the first build pays restore.

Enable the optional LINQ, generated Regex and generated JSON probe with:

```bash
scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
```

## Layout

`src/` contains the fixture entrypoint and probes. The project selects their
build settings. The hostcheck harness lives in `hostcheck/`.

## Build settings

The project and imported SDK build files define the compiler, linker and
runtime settings for this revision. Keep those inputs together when building
these fixtures; the WebAssembly module is checked against the host contract.

## Host checks

The [hostcheck harness](hostcheck/) exercises this fixture through host calls.
Its assertions apply to the specific guest state and resource setup under test;
they do not establish portability of arbitrary host resources or live stacks.

## Toolchain inputs

The [fixture project](src/) and its imported build files record the compiler
and linker inputs. Use those inputs when reproducing this revision's build.
