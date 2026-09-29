#!/usr/bin/env bash
# Publish the runtime acceptance guest for Mono.
# Usage: guest-runtime/tests/guest/build.sh [--if-stale]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"

IF_STALE=0
RUNTIME=mono
FIXTURE=runtime
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
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

FEED="${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/guest-runtime/out/feed}"
if [ -n "${ISOPLANE_TEST_RUNTIME_FEED:-}" ]; then
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
  echo "==> explicitly selected runtime artifacts: $FEED"
else
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-current "$ROOT/guest-runtime" "$FEED"
fi
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
PROPS="$FEED/Isoplane.GuestRuntime.props"
NAME="guest_${FIXTURE}_acceptance"
OUTPUT="$ROOT/.build/wasm32/acceptance"
OUT="$OUTPUT/$RUNTIME/$NAME.wasm"
INPUTS=$(python3 "$TOOLS" fingerprint "$HERE/src" "$ROOT/guest-sdks/dotnet" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED")
if [ "$IF_STALE" = 1 ] && [ -f "$OUT.runtime" ] \
   && [ "$(cat "$OUT.runtime")" = "$(python3 "$TOOLS" runtime-identity "$PROPS" "$RUNTIME")" ] \
   && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "$OUT" "$OUT.runtime"; then
  echo "==> acceptance guest ($RUNTIME) is up to date"
  exit 0
fi
python3 "$TOOLS" require-linux
python3 "$TOOLS" output-directory "$ROOT" wasm32 acceptance >/dev/null
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the acceptance guest cannot be built." >&2; exit 1; }

# Symbols stay in the interpreter's module so a trap names its frames.
profile=(-p:WasmNativeStrip=false)
if [ "$RUNTIME" = mono ]; then
  payload="$HERE/obj/$RUNTIME/payload"
  dotnet build "$HERE/src/Payload/Payload.csproj" --configuration Release \
    --artifacts-path "$HERE/obj/$RUNTIME/payload-artifacts" --output "$payload"
  profile+=("-p:IsoplaneAcceptancePayload=$payload/Isoplane.Acceptance.DynamicPayload.dll")
fi
ISOPLANE_OUT_DIR="$OUTPUT/$RUNTIME" ISOPLANE_OUT="$NAME" "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  "${profile[@]}" --artifacts-path "$HERE/obj/$RUNTIME-$FIXTURE"

python3 "$TOOLS" record "$OUT.fixture.json" "$INPUTS" "$OUT" "$OUT.runtime"
