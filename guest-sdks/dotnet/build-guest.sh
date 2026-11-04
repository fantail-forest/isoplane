#!/usr/bin/env bash
# Build a C# Isoplane guest to a core wasm module.
#
# Usage: build-guest.sh <project.csproj|guest-dir> [dotnet publish args...]
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: build-guest.sh <project.csproj|guest-dir> [dotnet publish args...]" >&2
  exit 2
fi
shopt -s nullglob
if [ -f "$1" ] && [[ "$1" == *.csproj ]]; then
  HERE="$(cd "$(dirname "$1")" && pwd)"
  CSPROJ="$HERE/$(basename "$1")"
elif [ -d "$1" ]; then
  HERE="$(cd "$1" && pwd)"
  projects=("$HERE/src/"*.csproj)
  if [ "${#projects[@]}" != 1 ]; then
    echo "FAIL: expected exactly one .csproj in $HERE/src" >&2
    exit 1
  fi
  CSPROJ="${projects[0]}"
else
  echo "FAIL: expected a .csproj file or guest directory: $1" >&2
  exit 1
fi
shift
# ISOPLANE_OUT_DIR is where the module lands and ISOPLANE_OUT its basename, so one
# project can be built in several flavours side by side.
OUT="${ISOPLANE_OUT_DIR:-$HERE/out}"
NAME="${ISOPLANE_OUT:-}"

echo "==> dotnet publish (C# -> wasm32 core module)"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
PUBLISH=$(mktemp -d "$OUT/.publish.XXXXXX")
PENDING=""
trap 'rm -rf "$PUBLISH"; if [ -n "$PENDING" ]; then rm -f "$PENDING"; fi' EXIT
dotnet publish "$CSPROJ" -c Release --disable-build-servers -o "$PUBLISH" "$@"

modules=("$PUBLISH"/*.wasm)
[ "${#modules[@]}" = 1 ] && [ -s "${modules[0]}.runtime" ] || {
  echo "Expected one published WASM module with runtime provenance in $PUBLISH" >&2
  exit 1
}
PUBLISHED="${modules[0]}"
: "${NAME:=$(basename "$PUBLISHED" .wasm)}"
# A publish records the pack it linked beside the module; the sidecar
# travels with the module so a test can attribute its result.
cp "$PUBLISHED.runtime" "$OUT/$NAME.wasm.runtime"
PENDING=$(mktemp "$OUT/.publish.XXXXXX")
cp "$PUBLISHED" "$PENDING"
mv -f "$PENDING" "$OUT/$NAME.wasm"

echo "==> module: $OUT/$NAME.wasm"
