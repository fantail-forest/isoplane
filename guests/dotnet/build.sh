#!/usr/bin/env bash
# Build the C# WISP guest to a core wasm module (spec.md §4.2, claim C10).
#
# Runs INSIDE the wisp-dev container (plan.md ground rule 1). From the host:
#   scripts/dev guests/dotnet/build.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Where the module lands. Defaults to this directory's `out/`; the throughput
# suite's flavour overrides it to `guests/out`, which is the directory
# `wisp-bench --guest-dir` reads and where the Rust bench guests already are.
# See build-bench-echo.sh.
OUT="${WISP_OUT_DIR:-$HERE/out}"

# Output basename. Distinct flavours must land at distinct paths so they can be
# compared against each other — `bench/dotnet-sharing` needs two materially
# different modules from the same toolchain to measure cross-template page
# dedup (§6.2: are two tenants' BCL images byte-identical?).
#   WISP_OUT=wisp_guest_dotnet_compat guests/dotnet/build.sh -p:WispCompatProbe=true
NAME="${WISP_OUT:-wisp_guest_dotnet}"

# NativeAOT-LLVM shells out to the WASI SDK's clang for the final link, and the
# PAL shim below is compiled with the same clang. The ILCompiler package pins
# an expected version (29.0 as of the pinned build) and warns on anything else.
#
# It comes from the IMAGE (see the Dockerfile), not from here. This script used
# to install it on first run, which could never work: `scripts/dev` runs the
# container as the invoking uid (ground rule 1) and /opt is root-owned, so the
# untar emitted two hundred lines of "Cannot mkdir: Permission denied" and then
# -- because the failure was inside a pipeline whose exit status belonged to
# something else -- carried on to a link failure that named neither the missing
# sysroot nor the reason. Refusing here, with the fix, is the whole of the
# improvement.
WASI_SDK_VERSION="${WASI_SDK_VERSION:-29.0}"
WASI_SDK_PATH="${WASI_SDK_PATH:-/opt/wasi-sdk-${WASI_SDK_VERSION}-x86_64-linux}"

if [ ! -x "$WASI_SDK_PATH/bin/clang" ]; then
  {
    echo "FAIL: no wasi-sdk at $WASI_SDK_PATH (looked for bin/clang)."
    echo
    echo "It ships in the wisp-dev image alongside .NET and wasm-tools. If this"
    echo "is the sandbox, the image predates the layer that adds it:"
    echo "    docker build --build-arg HOST_UID=\$(id -u) --build-arg HOST_GID=\$(id -g) \\"
    echo "        -t wisp-dev:latest ."
    echo "    scripts/dev --stop      # so the next run picks the new image up"
    echo
    echo "Outside the sandbox, point WASI_SDK_PATH at your own ${WASI_SDK_VERSION} install."
  } >&2
  exit 1
fi
export WASI_SDK_PATH

# The PAL shim (native/wisp_pal.c) defines the `__wasm_import_*` symbols that
# wasi-libc would otherwise leave as `wasi:*` imports, so wasm-ld resolves them
# locally and the module's import list is `wisp` and nothing else (§4.3 V1).
# It has to be compiled by the WASI SDK's clang, not by ILC, so it happens here
# rather than in the csproj.
echo "==> compiling the WISP PAL shim (native/wisp_pal.c)"
mkdir -p "$HERE/obj"
"$WASI_SDK_PATH/bin/clang" --target=wasm32-unknown-wasip2 -O2 -c \
  -o "$HERE/obj/wisp_pal.o" "$HERE/native/wisp_pal.c"

# MSBuild's up-to-date check does not see linker *arguments*, so changing a
# property like WispMemoryMaxBytes would otherwise silently reuse the previous
# link. Dropping the native output makes every build re-link; it costs ~1 s.
rm -rf "$HERE/src/bin/Release/net10.0/wasi-wasm/native"

echo "==> dotnet publish (NativeAOT-LLVM -> wasm32 core module)"
dotnet publish "$HERE/src/WispGuest.csproj" -c Release \
  "-p:WispPalObject=$HERE/obj/wisp_pal.o" "$@"

mkdir -p "$OUT"
cp "$HERE/src/bin/Release/net10.0/wasi-wasm/publish/wisp_guest_dotnet.wasm" "$OUT/$NAME.wasm"

echo "==> module"
ls -l "$OUT/$NAME.wasm"
wasm-tools validate "$OUT/$NAME.wasm" && echo "    validates as core wasm"

echo "==> imports (this list IS the capability manifest, §5.3 / rule V1)"
wasm-tools print "$OUT/$NAME.wasm" | grep -E '^\s*\(import' | sed -E 's/ \(func.*//;s/^\s*/    /'

# HARD GATE. Invariant 4 says the import list is the complete capability
# manifest; anything outside module `wisp` is an undeclared capability and
# `wisp_control::validate` rejects it under V1. The PAL shim is a closure over
# what the tenant's code drags out of the BCL, not a fixed list — a new BCL
# reference can pull in a new wasi import. Failing the build here is the only
# way that stays visible instead of turning up as a deploy rejection.
stray=$(wasm-tools print "$OUT/$NAME.wasm" \
        | grep -E '^\s*\(import' | grep -v '(import "wisp"' || true)
if [ -n "$stray" ]; then
  {
    echo
    echo "FAIL: the module imports something outside the \`wisp\` module."
    echo "$stray"
    echo
    echo "Each one is an undeclared capability (invariant 4, rule V1). Define"
    echo "its \`__wasm_import_*\` symbol in native/wisp_pal.c; the symbol name"
    echo "and signature come from the module itself:"
    echo "    guests/dotnet/build.sh -p:NativeDebugSymbols=true"
    echo "    wasm-tools print $OUT/$NAME.wasm | grep '(import \"wasi'"
  } >&2
  exit 1
fi

echo "==> exports"
wasm-tools print "$OUT/$NAME.wasm" \
  | grep -E '^\s*\(export' | sed -E 's/ \(func.*//;s/ \(memory.*//;s/ \(global.*//;s/ \(table.*//;s/^\s*/    /'
