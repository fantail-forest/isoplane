#!/usr/bin/env bash
# Build a C# Isoplane guest to a core wasm module.
#
#   build-guest.sh <guest-dir> [dotnet publish args...]
#   guests/dotnet/build.sh     # a guest's build.sh wraps this
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
# ISOPLANE_OUT_DIR is where the module lands and ISOPLANE_OUT its basename, so one
# project can be built in several flavours side by side.
OUT="${ISOPLANE_OUT_DIR:-$HERE/out}"
NAME="${ISOPLANE_OUT:-}"

# Build the managed validator separately from any parent Cargo invocation.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ISOPLANE_CLI_ASSEMBLY="$(python3 "$ROOT/scripts/fixture-tools.py" isoplane-cli "$ROOT")"

echo "==> dotnet publish (C# -> wasm32 core module)"
mkdir -p "$HERE/obj"
PUBLISH=$(mktemp -d "$HERE/obj/publish.XXXXXX")
PENDING=""
trap 'rm -rf "$PUBLISH"; if [ -n "$PENDING" ]; then rm -f "$PENDING"; fi' EXIT
dotnet publish "$CSPROJ" -c Release --disable-build-servers -o "$PUBLISH" "-p:IsoplaneCliAssembly=$ISOPLANE_CLI_ASSEMBLY" "$@"

mkdir -p "$OUT"
PUBLISHED=$(python3 "$ROOT/scripts/fixture-tools.py" published-guest "$PUBLISH")
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
