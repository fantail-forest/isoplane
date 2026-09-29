#!/usr/bin/env bash
# Publish the runtime acceptance guest for Mono.
# Usage: guest-runtime/tests/guest/build.sh [--if-stale]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"

COMPILE=0
RUNTIME=mono
FIXTURE=runtime
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) shift ;;
    --compile) COMPILE=1; shift ;;
    --runtime)
      [ $# -ge 2 ] || { echo "missing runtime name" >&2; exit 2; }
      case "$2" in
        mono) RUNTIME="$2" ;;
        *) echo "unknown runtime: $2 (expected mono)" >&2; exit 2 ;;
      esac
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$COMPILE" = 0 ]; then
  exec python3 "$ROOT/scripts/tasks.py" fixtures:acceptance
fi

FEED="${ISOPLANE_GUEST_RUNTIME_FEED:-${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/.build/wasm32/runtime/feed}}"
python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
PROPS="$FEED/Isoplane.GuestRuntime.props"
NAME="guest_${FIXTURE}_acceptance"
OUTPUT="$ROOT/.build/wasm32/acceptance"
OUT="$OUTPUT/$RUNTIME/$NAME.wasm"
python3 "$TOOLS" require-linux
python3 "$TOOLS" output-directory "$ROOT" wasm32 acceptance >/dev/null
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the acceptance guest cannot be built." >&2; exit 1; }

# Symbols stay in the interpreter's module so a trap names its frames.
profile=(-p:WasmNativeStrip=false)
if [ "$RUNTIME" = mono ]; then
  payload="$ROOT/.build/wasm32/acceptance-intermediates/$RUNTIME/payload"
  dotnet build "$HERE/src/Payload/Payload.csproj" --configuration Release \
    --artifacts-path "$ROOT/.build/wasm32/acceptance-intermediates/$RUNTIME/payload-artifacts" --output "$payload"
  profile+=("-p:IsoplaneAcceptancePayload=$payload/Isoplane.Acceptance.DynamicPayload.dll")
fi
ISOPLANE_OUT_DIR="$OUTPUT/$RUNTIME" ISOPLANE_OUT="$NAME" "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  "${profile[@]}" --artifacts-path "$ROOT/.build/wasm32/acceptance-intermediates/$RUNTIME-$FIXTURE"
