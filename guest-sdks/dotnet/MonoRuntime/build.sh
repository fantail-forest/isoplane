#!/usr/bin/env bash
# Build the shared Mono runtime modules the Isoplane.Sdk package carries, the
# production runtime and the local-debug one, and runtime.json recording
# the SDK version, source revision and Mono pack they were built from.
# Usage: guest-sdks/dotnet/MonoRuntime/build.sh --output DIRECTORY [--source-revision REVISION]
# The revision defaults to the checkout's HEAD, marked +dirty when it has changes.
# ISOPLANE_SQLITE_DIRECTORY names the output of sqlite/build.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

OUTPUT=""
REVISION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUTPUT="${2:?--output needs a directory}"; shift 2 ;;
    --source-revision) REVISION="${2:?--source-revision needs a revision}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUTPUT" ] || { echo "missing --output DIRECTORY" >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { echo "Build shared runtime modules on Linux first." >&2; exit 1; }
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the shared runtime cannot be built." >&2; exit 1; }

mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
SQLITE="${ISOPLANE_SQLITE_DIRECTORY:?Run task runtime:modules, which builds SQLite first}"
SQLITE="$(cd "$SQLITE" && pwd)/"
# Symbols stay in the interpreter's module so a trap names its frames.
ISOPLANE_OUT_DIR="$OUTPUT" ISOPLANE_OUT=Isoplane.MonoRuntime "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  -p:WasmNativeStrip=false "-p:IsoplaneSqliteDirectory=$SQLITE" --artifacts-path "$OUTPUT/intermediates/production"
ISOPLANE_OUT_DIR="$OUTPUT" ISOPLANE_OUT=Isoplane.MonoRuntime.Debug "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  -p:WasmNativeStrip=false -p:IsoplaneLocalDebug=true "-p:IsoplaneSqliteDirectory=$SQLITE" --artifacts-path "$OUTPUT/intermediates/debug"
for module in Isoplane.MonoRuntime Isoplane.MonoRuntime.Debug; do
  [ -f "$OUTPUT/$module.wasm.runtime" ] \
    || { echo "FATAL: the $module publish recorded no Mono pack identity" >&2; exit 1; }
done
# The native functions the runtime provides to application packages, for the
# SDK's native-import diagnostic.
cp "${SQLITE}native-imports.txt" "$OUTPUT/native-imports.txt"
python3 "$HERE/manifest.py" "$OUTPUT" "$ROOT" ${REVISION:+"$REVISION"}
