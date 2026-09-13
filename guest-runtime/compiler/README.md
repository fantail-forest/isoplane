# NativeAOT host compiler

This directory builds the NativeAOT host compiler and LLVM backend from the
pinned runtimelab commit. The patches implement the WebAssembly C ABI for narrow
integers and restrict WASI direct native calls to the selected export inventory. The BCL packs have a
separate identity and do not need rebuilding for a host compiler change.

The qualified builder is Linux x64 with Clang, CMake, Ninja, Python 3.11+, LLVM
18.1.3 development files and their native dependencies (including libzstd-dev).
The build uses the fork's pinned .NET SDK. The published compiler is self-contained;
customers do not install that SDK.

```sh
python3 guest-runtime/compiler/build.py build --output guest-runtime/out/compiler
python3 guest-runtime/compiler/build.py verify-current --output guest-runtime/out/compiler
python3 -m unittest discover -s guest-runtime/compiler/tests
```

The compiler output belongs beside the runtime `feed/` directory. Its receipt
names the source inputs, original package hash, source-built compiler file hashes, backend hash and
composed package hashes. `verify-artifact` checks a deliberately selected historical
artifact without requiring current compiler sources. SDK publication verifies
the selected NuGet package against the receipt. The toolchain bundle includes
an identical backend outside the package so `isoplane doctor` can check its native
dependencies before the first application build.

Output directories are immutable; choose a new output directory when rebuilding,
then select it when assembling the toolchain. No package version is overwritten.
