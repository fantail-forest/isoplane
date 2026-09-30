#!/usr/bin/env bash
# Build the application-independent Mono runtime module or one application
# package fixture: the package is the archive `isoplane bundle` writes.
# Usage: guest-runtime/tests/packages/build.sh [--if-stale] --fixture runtime|alpha|beta|native|web|diagnostics
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

COMPILE=0
FIXTURE=""
FIXTURES=(runtime alpha beta native web diagnostics)
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) shift ;;
    --compile) COMPILE=1; shift ;;
    --fixture)
      for fixture in "${FIXTURES[@]}"; do
        [ "$fixture" != "${2:-}" ] || FIXTURE="$fixture"
      done
      [ -n "$FIXTURE" ] || { echo "unknown package fixture: ${2:-} (expected ${FIXTURES[*]})" >&2; exit 2; }
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ "$COMPILE" = 0 ]; then
  exec task --dir "$ROOT" "fixtures:package${FIXTURE:+:$FIXTURE}"
fi

FEED="${ISOPLANE_GUEST_RUNTIME_FEED:-${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/.build/wasm32/runtime/feed}}"
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the package fixtures cannot be built." >&2; exit 1; }
RUNTIME_OUTPUT="${ISOPLANE_TEST_RUNTIME_MODULES:-$ROOT/.build/wasm32/runtime-modules}"
PACKAGE_OUTPUT="$ROOT/.build/clr-anycpu/package-fixtures"

SDK="$ROOT/guest-sdks/dotnet"
if [ "$FIXTURE" = runtime ]; then
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
  OUT="$RUNTIME_OUTPUT/Isoplane.MonoRuntime.wasm"
  DEBUG="$RUNTIME_OUTPUT/Isoplane.MonoRuntime.Debug.wasm"
  [ "$(uname -s)" = Linux ] || { echo "Build shared fixtures on Linux first." >&2; exit 1; }
  "$SDK/MonoRuntime/build.sh" --output "$RUNTIME_OUTPUT"

else
  # A package carries the identity of the runtime it is built against, and
  # these packages run on the fixture runtime.
  export ISOPLANE_RUNTIME_DIRECTORY="$RUNTIME_OUTPUT"
  OUT="$PACKAGE_OUTPUT/$FIXTURE"
  [ "$(uname -s)" = Linux ] || { echo "Build shared fixtures on Linux first." >&2; exit 1; }
  mkdir -p "$PACKAGE_OUTPUT"
  dotnet "${ISOPLANE_CLI:?Task must supply the staged CLI}" bundle --project "$HERE/$FIXTURE" --output "$OUT.tar"
fi
