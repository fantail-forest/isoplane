#!/usr/bin/env bash
# Build the application-independent Mono runtime module or one application
# package fixture: the package is what `isoplane bundle` writes, unpacked.
# Usage: guest-runtime/tests/packages/build.sh [--if-stale] --fixture runtime|alpha|beta|native
# Serialised with the other C# guest builds: they share the SDK project's
# intermediate files.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TOOLS="$ROOT/scripts/fixture-tools.py"
LOCK="$ROOT/guests/.build-dotnet.lock"
if [ "${ISOPLANE_FIXTURE_LOCK:-}" != "$HERE/build.sh" ]; then
  exec python3 "$TOOLS" lock "$LOCK" "$HERE/build.sh" "$@"
fi

IF_STALE=0
FIXTURE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --fixture)
      case "${2:-}" in
        runtime|alpha|beta|native) FIXTURE="$2" ;;
        *) echo "unknown package fixture: ${2:-} (expected runtime, alpha, beta or native)" >&2; exit 2 ;;
      esac
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$FIXTURE" ] || { echo "missing --fixture runtime|alpha|beta|native" >&2; exit 2; }

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
  OUT="$HERE/out/isoplane_mono_runtime.wasm"
  OUTPUTS=("$OUT" "$OUT.runtime")
  INPUTS=$(python3 "$TOOLS" fingerprint "$SDK" "$ROOT/tools/BuildProtocol" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED")
  if [ "$IF_STALE" = 1 ] && [ -f "$OUT.runtime" ] \
     && [ "$(cat "$OUT.runtime")" = "$(python3 "$TOOLS" runtime-identity "$FEED/Isoplane.GuestRuntime.props" mono)" ] \
     && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"; then
    echo "==> shared Mono runtime is up to date"
    exit 0
  fi
  rm -f "$OUT"
  # Symbols stay in the interpreter's module so a trap names its frames.
  ISOPLANE_OUT_DIR="$HERE/out" ISOPLANE_OUT=isoplane_mono_runtime "$SDK/build-guest.sh" "$SDK/MonoRuntime" \
    -p:WasmNativeStrip=false --artifacts-path "$HERE/obj/runtime"
else
  OUT="$HERE/out/$FIXTURE"
  OUTPUTS=("$OUT.tar")
  INPUTS=$(python3 "$TOOLS" fingerprint "$HERE/$FIXTURE" "$HERE/Package.props" "$HERE/Package.targets" "$SDK" "$ROOT/tools/Isoplane.Cli" "$ROOT/tools/BuildProtocol" "$HERE/build.sh" "$TOOLS" "$ROOT/global.json" "$ROOT/nuget.config" "$FEED")
  if [ "$IF_STALE" = 1 ] && [ -f "$OUT/application.json" ] \
     && python3 "$TOOLS" fresh "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"; then
    echo "==> package $FIXTURE is up to date"
    exit 0
  fi
  CLI="$(python3 "$TOOLS" isoplane-cli "$ROOT")"
  rm -rf "$OUT" "$OUT.tar"
  dotnet "$CLI" bundle --project "$HERE/$FIXTURE" --output "$OUT.tar"
  python3 -c 'import sys, tarfile; tarfile.open(sys.argv[1]).extractall(sys.argv[2], filter="data")' "$OUT.tar" "$OUT"
fi

python3 "$TOOLS" record "$OUT.fixture.json" "$INPUTS" "${OUTPUTS[@]}"
