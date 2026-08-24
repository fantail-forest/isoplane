# `guests/dotnet`

C# HTTP handler and runtime probes compiled with NativeAOT-LLVM to a core
WebAssembly module for the host interface.


## Build and check

The commands below use the repository development helper.

```bash
scripts/dev guests/dotnet/build.sh                  # -> out/wisp_guest_dotnet.wasm
scripts/dev guests/dotnet/check.sh                  # asserts the ABI and invariant 3
scripts/dev guests/dotnet/check.sh --wasi=trap      # the decisive V1 experiment
scripts/dev guests/dotnet/check.sh --density 200    # per-instance RSS (§6.2)
```

`--wasi=` controls how non-`wisp` imports are bound: `pal` (default) implements
only the measured runtime-PAL set, `trap` makes every one abort if called, and
`stub` enumerates which are reachable. Since the PAL shim landed all three
report zero non-`wisp` imports; they stay as the regression guard for a
toolchain bump that reintroduces some.

Set a 64 MiB ceiling with
`-p:WispMemoryMaxBytes=67108864`.

`WISP_OUT=<basename>` changes the output filename so flavours can coexist under
`out/` and be compared against each other:

```bash
WISP_OUT=wisp_guest_dotnet_compat  scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
WISP_OUT=wisp_guest_dotnet_entropy scripts/dev guests/dotnet/build.sh -p:WispEntropyProbe=true
```

## Initialization copies

The clone probe compares values after copying a prepared guest image.
Stateful generators and values initialized before capture can repeat in each
copy. This does not imply that later host entropy requests repeat.

```bash
WISP_OUT=wisp_guest_dotnet_entropy scripts/dev guests/dotnet/build.sh -p:WispEntropyProbe=true
scripts/dev cargo run --release --bin wisp-dotnet-hostcheck \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml -- \
    guests/dotnet/out/wisp_guest_dotnet_entropy.wasm --entropy-clone
```


`build.sh` installs wasi-sdk 29.0 on first run if it is missing. NuGet packages
live in the `wisp-nuget` docker volume, so only the first build pays restore.

Enable the optional LINQ, generated Regex and generated JSON probe with:

```bash
scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
```

## The PAL

The PAL connects runtime operations to the declared host interface. Validation
and linking determine which host capabilities the module can reach. See the
SDK source and build inputs for the implementation used by this revision.

## Layout

`src/` contains the fixture entrypoint and probes. The project selects their
build settings. The hostcheck harness lives in `hostcheck/`.

```bash
scripts/dev cargo run --release --features validator --bin wisp-validate \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml
```

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
