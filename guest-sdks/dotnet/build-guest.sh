#!/usr/bin/env bash
# Build a C# WISP guest to a core wasm module.
#
#   build-guest.sh <guest-dir> [dotnet publish args...]
#
# One driver for every guest, next to the one SDK they all reference. A guest's
# own build.sh is a two-line wrapper around this.
#
# Runs INSIDE the wisp-dev container. From the host:
set -euo pipefail

SDK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ $# -lt 1 ]; then
  echo "usage: build-guest.sh <guest-dir> [dotnet publish args...]" >&2
  exit 2
fi
HERE="$(cd "$1" && pwd)"
shift

# Exactly one .csproj per guest. Two would make "which one is the module" a
# question this script would have to guess the answer to.
CSPROJ=$(find "$HERE/src" -maxdepth 1 -name '*.csproj' | sort)
if [ "$(printf '%s' "$CSPROJ" | grep -c .)" != 1 ]; then
  echo "FAIL: expected exactly one .csproj in $HERE/src, found:" >&2
  echo "$CSPROJ" >&2
  exit 1
fi
# Where the module lands. Defaults to this directory's `out/`; the throughput
# suite's flavour overrides it to `guests/out`, which is the directory
# `wisp-bench --guest-dir` reads and where the Rust bench guests already are.
# See guests/dotnet/build-bench-echo.sh.
OUT="${WISP_OUT_DIR:-$HERE/out}"

# Output basename. Distinct flavours must land at distinct paths so they can be
# compared against each other — `bench/dotnet-sharing` needs two materially
# different modules from the same toolchain to measure cross-template page
# dedup, i.e. whether two tenants' BCL images are byte-identical.
#   WISP_OUT=wisp_guest_dotnet_compat guests/dotnet/build.sh -p:WispCompatProbe=true
# Defaulted after the publish, from the module the project produced — see below.
NAME="${WISP_OUT:-}"

# NativeAOT-LLVM shells out to the WASI SDK's clang for the final link, and the
# PAL shim below is compiled with the same clang. The ILCompiler package pins
# an expected version (29.0 as of the pinned build) and warns on anything else.
#
# It comes from the IMAGE (see the Dockerfile), not from here. This script used
# to install it on first run, which could never work: `scripts/dev` runs the
# container as the invoking uid and /opt is root-owned, so the untar emitted
# two hundred lines of "Cannot mkdir: Permission denied" and then
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
# locally and the module's import list is `wisp` and nothing else.
# It has to be compiled by the WASI SDK's clang, not by ILC, so it happens here
# rather than in the csproj.
# The PAL lives with the SDK, not with the guest: it is a closure over the BCL
# surface the SDK and the guest between them touch, and there is one of it.
echo "==> compiling the WISP PAL shim ($SDK/native/wisp_pal.c)"
mkdir -p "$HERE/obj"
"$WASI_SDK_PATH/bin/clang" --target=wasm32-unknown-wasip2 -O2 -c \
  -o "$HERE/obj/wisp_pal.o" "$SDK/native/wisp_pal.c"

# MSBuild's up-to-date check does not see linker *arguments*, so changing a
# property like WispMemoryMaxBytes would otherwise silently reuse the previous
# link. Dropping the native output makes every build re-link.
rm -rf "$HERE/src/bin/Release/net10.0/wasi-wasm/native"

echo "==> dotnet publish (NativeAOT-LLVM -> wasm32 core module)"
dotnet publish "$CSPROJ" -c Release \
  "-p:WispPalObject=$HERE/obj/wisp_pal.o" "$@"

mkdir -p "$OUT"
PUBLISHED=$(find "$HERE/src/bin/Release/net10.0/wasi-wasm/publish" -maxdepth 1 -name '*.wasm' | sort)
if [ "$(printf '%s' "$PUBLISHED" | grep -c .)" != 1 ]; then
  echo "FAIL: expected exactly one published .wasm, found:" >&2
  echo "$PUBLISHED" >&2
  exit 1
fi
: "${NAME:=$(basename "$PUBLISHED" .wasm)}"
cp "$PUBLISHED" "$OUT/$NAME.wasm"

echo "==> module"
ls -l "$OUT/$NAME.wasm"
wasm-tools validate "$OUT/$NAME.wasm" && echo "    validates as core wasm"

echo "==> imports (this list IS the capability manifest)"
wasm-tools print "$OUT/$NAME.wasm" | grep -E '^\s*\(import' | sed -E 's/ \(func.*//;s/^\s*/    /'

# HARD GATE. The import list is the complete capability manifest; anything
# outside module `wisp` is an undeclared capability and
# `wisp_control::validate` rejects it. The PAL shim is a closure over what the
# tenant's code drags out of the BCL, not a fixed list — a new BCL reference
# can pull in a new wasi import. Failing the build here is the only way that
# stays visible instead of turning up as a deploy rejection.
stray=$(wasm-tools print "$OUT/$NAME.wasm" \
        | grep -E '^\s*\(import' | grep -v '(import "wisp"' || true)
if [ -n "$stray" ]; then
  {
    echo
    echo "FAIL: the module imports something outside the \`wisp\` module."
    echo "$stray"
    echo
    echo "Each one is an undeclared capability the validator rejects. Define"
    echo "its \`__wasm_import_*\` symbol in native/wisp_pal.c; the symbol name"
    echo "and signature come from the module itself:"
    echo "    $HERE/build.sh -p:NativeDebugSymbols=true"
    echo "    wasm-tools print $OUT/$NAME.wasm | grep '(import \"wasi'"
  } >&2
  exit 1
fi

echo "==> exports"
wasm-tools print "$OUT/$NAME.wasm" \
  | grep -E '^\s*\(export' | sed -E 's/ \(func.*//;s/ \(memory.*//;s/ \(global.*//;s/ \(table.*//;s/^\s*/    /'
