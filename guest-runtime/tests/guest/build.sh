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
FIXTURE=runtime
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --fixture)
      [ "${2:-}" = compiler ] || { echo "unknown acceptance fixture" >&2; exit 2; }
      FIXTURE=compiler; shift 2 ;;
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
NAME="guest_${FIXTURE}_acceptance"
OUT="$HERE/out/$RUNTIME/$NAME.wasm"
COMPILER_INPUTS=()
if [ "$RUNTIME" = nativeaot ]; then
  COMPILER="$(dirname "$FEED")/compiler"
  VERIFY=verify-current
  if [ -n "${WISP_TEST_RUNTIME_FEED:-}" ]; then VERIFY=verify-artifact; fi
  python3 "$ROOT/guest-runtime/compiler/build.py" "$VERIFY" --output "$COMPILER"
  COMPILER_INPUTS+=("$COMPILER/compiler.json")
fi
INPUTS=$(python3 "$TOOLS" fingerprint "$HERE/src" "$HERE/compiler" "$ROOT/guest-sdks/dotnet" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED" "${COMPILER_INPUTS[@]}")
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
if [ "$FIXTURE" = compiler ]; then
  [ "$RUNTIME" = nativeaot ] || { echo "compiler fixture requires NativeAOT" >&2; exit 2; }
  : "${WASI_SDK_PATH:?WASI_SDK_PATH is required to compile the ABI fixture}"
  mkdir -p "$HERE/obj/compiler"
  "$WASI_SDK_PATH/bin/clang" --target=wasm32-wasip2 -O2 -c "$HERE/compiler/narrow.c" -o "$HERE/obj/compiler/narrow.o"
  profile+=(-p:WispCompilerProbe=true "-p:WispCompilerProbeObject=$HERE/obj/compiler/narrow.o")
fi
rm -f "$OUT"
WISP_OUT_DIR="$HERE/out/$RUNTIME" WISP_OUT="$NAME" "$ROOT/guest-sdks/dotnet/build-guest.sh" "$HERE" \
  "${profile[@]}" --artifacts-path "$HERE/obj/$RUNTIME-$FIXTURE"

python3 "$TOOLS" record "$OUT.fixture.json" "$INPUTS" "$OUT" "$OUT.runtime"
