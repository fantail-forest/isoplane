#!/usr/bin/env bash
# Build the application-independent Mono runtime module or one application
# package fixture: the package is the archive `isoplane bundle` writes.
# Usage: guest-runtime/tests/packages/build.sh [--if-stale] --fixture runtime|alpha|beta|native|web|diagnostics
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"

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
  exec python3 "$ROOT/scripts/tasks.py" "fixtures:package${FIXTURE:+:$FIXTURE}"
fi

FEED="${ISOPLANE_GUEST_RUNTIME_FEED:-${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/.build/wasm32/runtime/feed}}"
python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the package fixtures cannot be built." >&2; exit 1; }
RUNTIME_OUTPUT="$ROOT/.build/wasm32/runtime-modules"
PACKAGE_OUTPUT="$ROOT/.build/clr-anycpu/package-fixtures"

SDK="$ROOT/guest-sdks/dotnet"
if [ "$FIXTURE" = runtime ]; then
  OUT="$RUNTIME_OUTPUT/Isoplane.MonoRuntime.wasm"
  DEBUG="$RUNTIME_OUTPUT/Isoplane.MonoRuntime.Debug.wasm"
  python3 "$TOOLS" require-linux
  "$SDK/MonoRuntime/build.sh" --output "$RUNTIME_OUTPUT"

else
  # A package carries the identity of the runtime it is built against, and
  # these packages run on the fixture runtime.
  export ISOPLANE_RUNTIME_DIRECTORY="$RUNTIME_OUTPUT"
  OUT="$PACKAGE_OUTPUT/$FIXTURE"
  python3 "$TOOLS" require-linux
  python3 "$TOOLS" output-directory "$ROOT" clr-anycpu package-fixtures >/dev/null
  CLI="$ROOT/.build/clr-anycpu/isoplane-cli/artifacts/bin/Isoplane.Cli/debug/Isoplane.Cli.dll"
  dotnet "$CLI" bundle --project "$HERE/$FIXTURE" --output "$OUT.tar"
fi
