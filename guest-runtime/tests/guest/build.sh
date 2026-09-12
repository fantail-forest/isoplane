#!/usr/bin/env bash
# Publish the runtime acceptance guest for one runtime profile.
# Usage: guest-runtime/tests/guest/build.sh [--if-stale] [--runtime nativeaot|mono]
# Serialised with the other C# guest builds: they share the SDK project's
# intermediate files.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"
LOCK="$ROOT/guests/.build-dotnet.lock"
if [ "${WISP_FIXTURE_LOCK:-}" != "$HERE/build.sh" ]; then
  exec python3 "$TOOLS" lock "$LOCK" "$HERE/build.sh" "$@"
fi

IF_STALE=0
RUNTIME=nativeaot
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --runtime)
      [ $# -ge 2 ] || { echo "missing runtime name" >&2; exit 2; }
      case "$2" in
        nativeaot|mono) RUNTIME="$2" ;;
        *) echo "unknown runtime: $2 (expected nativeaot or mono)" >&2; exit 2 ;;
      esac
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

FEED="${WISP_TEST_RUNTIME_FEED:-$ROOT/guest-runtime/out/feed}"
if [ -n "${WISP_TEST_RUNTIME_FEED:-}" ]; then
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
  echo "==> explicitly selected runtime artifacts: $FEED"
else
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-current "$ROOT/guest-runtime" "$FEED"
fi
export WISP_GUEST_RUNTIME_FEED="$FEED"
PROPS="$FEED/Wisp.GuestRuntime.props"
OUT="$HERE/out/$RUNTIME/guest_runtime_acceptance.wasm"
INPUTS=$(python3 "$TOOLS" fingerprint "$HERE/src" "$ROOT/guest-sdks/dotnet" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED")
if [ "$IF_STALE" = 1 ] && [ -f "$OUT.runtime" ] \
   && [ "$(cat "$OUT.runtime")" = "$(python3 "$TOOLS" runtime-identity "$PROPS" "$RUNTIME")" ] \
   && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "$OUT" "$OUT.runtime"; then
  echo "==> acceptance guest ($RUNTIME) is up to date"
  exit 0
fi
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the acceptance guest cannot be built." >&2; exit 1; }

case "$RUNTIME" in
  # Symbols stay in the interpreter's module so a trap names its frames.
  mono) profile=(-p:WispGuestRuntime=Mono -p:WasmNativeStrip=false) ;;
  nativeaot) profile=(-p:WispGuestRuntime=NativeAOT) ;;
esac
if [ "$RUNTIME" = mono ]; then
  payload="$HERE/obj/$RUNTIME/payload"
  dotnet build "$HERE/src/Payload/Payload.csproj" --configuration Release \
    --artifacts-path "$HERE/obj/$RUNTIME/payload-artifacts" --output "$payload"
  profile+=("-p:WispAcceptancePayload=$payload/Wisp.Acceptance.DynamicPayload.dll")
fi
rm -f "$OUT"
WISP_OUT_DIR="$HERE/out/$RUNTIME" WISP_OUT=guest_runtime_acceptance "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  "${profile[@]}" --artifacts-path "$HERE/obj/$RUNTIME"

python3 "$TOOLS" record "$OUT.fixture.json" "$INPUTS" "$OUT" "$OUT.runtime"
