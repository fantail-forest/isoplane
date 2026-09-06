#!/usr/bin/env bash
# Build a C# WISP guest to a core wasm module.
#
#   build-guest.sh <guest-dir> [dotnet publish args...]
set -euo pipefail

SDK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ $# -lt 1 ]; then
  echo "usage: build-guest.sh <guest-dir> [dotnet publish args...]" >&2
  exit 2
fi
HERE="$(cd "$1" && pwd)"
shift

CSPROJ=$(find "$HERE/src" -maxdepth 1 -name '*.csproj' | sort)
if [ "$(printf '%s' "$CSPROJ" | grep -c .)" != 1 ]; then
  echo "FAIL: expected exactly one .csproj in $HERE/src, found:" >&2
  echo "$CSPROJ" >&2
  exit 1
fi
# WISP_OUT_DIR is where the module lands and WISP_OUT its basename, so one
# project can be built in several flavours side by side.
OUT="${WISP_OUT_DIR:-$HERE/out}"
NAME="${WISP_OUT:-}"

# The pinned experimental compiler feed has Linux builds, but no macOS ARM64 build.
if [ "$(uname -s)" != Linux ]; then
  echo "FAIL: the pinned NativeAOT-LLVM compiler requires Linux. Build guests with:" >&2
  echo "    scripts/dev --container guests/build-dotnet.sh --if-stale" >&2
  exit 1
fi
WASI_SDK_PATH="${WASI_SDK_PATH:-/opt/wasi-sdk}"
if [ ! -x "$WASI_SDK_PATH/bin/clang" ]; then
  echo "FAIL: missing wasi-sdk clang at $WASI_SDK_PATH/bin/clang; set WASI_SDK_PATH to your installation." >&2
  exit 1
fi
export WASI_SDK_PATH

# The PAL shim defines the `__wasm_import_*` symbols wasi-libc leaves
# undefined, so the module imports from `wisp` and nothing else.
echo "==> compiling the WISP PAL shim ($SDK/native/wisp_pal.c)"
mkdir -p "$HERE/obj"
"$WASI_SDK_PATH/bin/clang" --target=wasm32-unknown-wasip2 -O2 -c \
  -o "$HERE/obj/wisp_pal.o" "$SDK/native/wisp_pal.c"

# MSBuild's up-to-date check does not see linker arguments, so every build
# re-links.
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

# An import outside module `wisp` is an undeclared capability the validator
# rejects; a new BCL reference can pull one in, so the build fails here.
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
  exit 3
fi

echo "==> exports"
wasm-tools print "$OUT/$NAME.wasm" \
  | grep -E '^\s*\(export' | sed -E 's/ \(func.*//;s/ \(memory.*//;s/ \(global.*//;s/ \(table.*//;s/^\s*/    /'
