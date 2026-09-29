#!/usr/bin/env bash
# Build the application-independent Mono runtime module or one application
# package fixture: the package is the archive `isoplane bundle` writes.
# Usage: guest-runtime/tests/packages/build.sh [--if-stale] --fixture runtime|alpha|beta|native|web|diagnostics
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"

IF_STALE=0
FIXTURE=""
FIXTURES=(runtime alpha beta native web diagnostics)
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --fixture)
      for fixture in "${FIXTURES[@]}"; do
        [ "$fixture" != "${2:-}" ] || FIXTURE="$fixture"
      done
      [ -n "$FIXTURE" ] || { echo "unknown package fixture: ${2:-} (expected ${FIXTURES[*]})" >&2; exit 2; }
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$FIXTURE" ]; then
  flags=()
  [ "$IF_STALE" = 0 ] || flags+=(--if-stale)
  for fixture in "${FIXTURES[@]}"; do
    "$HERE/build.sh" "${flags[@]}" --fixture "$fixture"
  done
  exit 0
fi

FEED="${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/guest-runtime/out/feed}"
if [ -n "${ISOPLANE_TEST_RUNTIME_FEED:-}" ]; then
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-artifact "$FEED"
else
  python3 "$ROOT/guest-runtime/feed-identity.py" verify-current "$ROOT/guest-runtime" "$FEED"
fi
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
command -v dotnet >/dev/null 2>&1 || { echo "FATAL: no .NET SDK on PATH, so the package fixtures cannot be built." >&2; exit 1; }
mkdir -p "$HERE/out"

SDK="$ROOT/guest-sdks/dotnet"
if [ "$FIXTURE" = runtime ]; then
  OUT="$HERE/out/runtime/Isoplane.MonoRuntime.wasm"
  DEBUG="$HERE/out/runtime/Isoplane.MonoRuntime.Debug.wasm"
  OUTPUTS=("$OUT" "$OUT.runtime" "$DEBUG" "$DEBUG.runtime" "$HERE/out/runtime/runtime-build.json" "$HERE/out/runtime/runtime.json")
  INPUTS=$(python3 "$TOOLS" fingerprint "$SDK" "$ROOT/tools/BuildProtocol" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED")
  if [ "$IF_STALE" = 1 ] && [ -f "$OUT.runtime" ] \
     && [ "$(cat "$OUT.runtime")" = "$(python3 "$TOOLS" runtime-identity "$FEED/Isoplane.GuestRuntime.props" mono)" ] \
     && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"; then
    echo "==> shared Mono runtime is up to date"
    exit 0
  fi
  SHARED_OUTPUTS=()
  for output in "${OUTPUTS[@]}"; do SHARED_OUTPUTS+=(--output "$output"); done
  python3 "$ROOT/scripts/build_storage.py" files \
    "mono-runtime:$INPUTS:$(git -C "$ROOT" rev-parse HEAD):$(dotnet --version)" \
    --linux-only "${SHARED_OUTPUTS[@]}" -- "$SDK/MonoRuntime/build.sh" --output "$HERE/out/runtime"
else
  # A package carries the identity of the runtime it is built against, and
  # these packages run on the fixture runtime.
  "$HERE/build.sh" --if-stale --fixture runtime
  export ISOPLANE_RUNTIME_DIRECTORY="$HERE/out/runtime"
  OUT="$HERE/out/$FIXTURE"
  OUTPUTS=("$OUT.tar")
  INPUTS=$(python3 "$TOOLS" fingerprint "$HERE/$FIXTURE" "$HERE/Package.props" "$HERE/Package.targets" "$SDK" "$ROOT/tools/Isoplane.Cli" "$ROOT/tools/BuildProtocol" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED" "$HERE/out/runtime/runtime.json")
  if [ "$IF_STALE" = 1 ] && [ -f "$OUT.tar" ] \
     && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"; then
    echo "==> package $FIXTURE is up to date"
    exit 0
  fi
  python3 "$TOOLS" require-linux
  CLI="$(python3 "$TOOLS" isoplane-cli "$ROOT")"
  dotnet "$CLI" bundle --project "$HERE/$FIXTURE" --output "$OUT.tar"
fi

python3 "$TOOLS" record "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"
