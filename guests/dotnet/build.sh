#!/usr/bin/env bash
# Build the C# WISP guest to a core wasm module (spec.md §4.2, claim C10).
#
# Runs INSIDE the wisp-dev container (plan.md ground rule 1). From the host:
#   scripts/dev guests/dotnet/build.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/out"

# NativeAOT-LLVM shells out to the WASI SDK's clang for the final link. The
# ILCompiler package pins an expected version (29.0 as of the pinned build) and
# warns on anything else, so we install exactly that one.
WASI_SDK_VERSION="${WASI_SDK_VERSION:-29.0}"
WASI_SDK_PATH="${WASI_SDK_PATH:-/opt/wasi-sdk-${WASI_SDK_VERSION}-x86_64-linux}"

if [ ! -d "$WASI_SDK_PATH" ]; then
  echo "==> installing wasi-sdk ${WASI_SDK_VERSION} into ${WASI_SDK_PATH}"
  tag="wasi-sdk-${WASI_SDK_VERSION%%.*}"
  curl -sSL -o /tmp/wasi-sdk.tar.gz \
    "https://github.com/WebAssembly/wasi-sdk/releases/download/${tag}/wasi-sdk-${WASI_SDK_VERSION}-x86_64-linux.tar.gz"
  mkdir -p "$(dirname "$WASI_SDK_PATH")"
  tar xzf /tmp/wasi-sdk.tar.gz -C "$(dirname "$WASI_SDK_PATH")"
  rm -f /tmp/wasi-sdk.tar.gz
fi
export WASI_SDK_PATH

echo "==> dotnet publish (NativeAOT-LLVM -> wasm32 core module)"
dotnet publish "$HERE/src/WispGuest.csproj" -c Release "$@"

mkdir -p "$OUT"
cp "$HERE/src/bin/Release/net10.0/wasi-wasm/publish/wisp_guest_dotnet.wasm" "$OUT/wisp_guest_dotnet.wasm"

echo "==> module"
ls -l "$OUT/wisp_guest_dotnet.wasm"
wasm-tools validate "$OUT/wisp_guest_dotnet.wasm" && echo "    validates as core wasm"

echo "==> imports (this list IS the capability manifest, §5.3 / rule V1)"
wasm-tools print "$OUT/wisp_guest_dotnet.wasm" | grep -E '^\s*\(import' | sed -E 's/ \(func.*//;s/^\s*/    /'

echo "==> exports"
wasm-tools print "$OUT/wisp_guest_dotnet.wasm" \
  | grep -E '^\s*\(export' | sed -E 's/ \(func.*//;s/ \(memory.*//;s/ \(global.*//;s/ \(table.*//;s/^\s*/    /'
