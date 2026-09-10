#!/usr/bin/env bash
# Build a C# WISP guest to a core wasm module.
#
#   build-guest.sh <guest-dir> [dotnet publish args...]
set -euo pipefail

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

# The SDK judges every linked module with `wisp doctor`; the guests in this
# repository are judged by this checkout's CLI, built on demand.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WISP_CLI="$(python3 "$ROOT/scripts/fixture-tools.py" wisp-cli "$ROOT")"

echo "==> dotnet publish (C# -> wasm32 core module)"
mkdir -p "$HERE/obj"
PUBLISH=$(mktemp -d "$HERE/obj/publish.XXXXXX")
PENDING=""
trap 'rm -rf "$PUBLISH"; if [ -n "$PENDING" ]; then rm -f "$PENDING"; fi' EXIT
dotnet publish "$CSPROJ" -c Release --disable-build-servers -o "$PUBLISH" "-p:WispCli=$WISP_CLI" "$@"

mkdir -p "$OUT"
PUBLISHED=$(find "$PUBLISH" -maxdepth 1 -name '*.wasm' | sort)
if [ "$(printf '%s' "$PUBLISHED" | grep -c .)" != 1 ]; then
  echo "FAIL: expected exactly one published .wasm, found:" >&2
  echo "$PUBLISHED" >&2
  exit 1
fi
: "${NAME:=$(basename "$PUBLISHED" .wasm)}"
# A publish records the pack it linked beside the module; the sidecar
# travels with the module so a test can attribute its result.
if [ -f "$PUBLISHED.runtime" ]; then
  cp "$PUBLISHED.runtime" "$OUT/$NAME.wasm.runtime"
else
  rm -f "$OUT/$NAME.wasm.runtime"
fi
PENDING=$(mktemp "$OUT/.publish.XXXXXX")
cp "$PUBLISHED" "$PENDING"
mv -f "$PENDING" "$OUT/$NAME.wasm"

echo "==> module: $OUT/$NAME.wasm"
