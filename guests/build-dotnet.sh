#!/usr/bin/env bash
# Build the C# test fixtures; samples have their own build scripts.
# Usage: guests/build-dotnet.sh [--if-stale]
# Serial because the variants share one project's intermediate files.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IF_STALE=0
[ "${1:-}" = "--if-stale" ] && IF_STALE=1

TOOLS="$ROOT/../scripts/fixture-tools.py"
if [ "${WISP_FIXTURE_LOCK:-}" != "$ROOT/build-dotnet.sh" ]; then
  exec python3 "$TOOLS" lock "$ROOT/.build-dotnet.lock" "$ROOT/build-dotnet.sh" "$@"
fi

# name | output path | guest dir | extra dotnet publish args | expect-gate-fail
#
# `_stock` is built without the PAL shim object, so it carries the raw
# `wasi:*` imports NativeAOT-LLVM emits; `wisp-engine`'s `dotnet_pal` test
# reads it to check what the adapter composes them down to.
FIXTURES=(
  "wisp_guest_dotnet_request|$ROOT/dotnet/out/wisp_guest_dotnet_request.wasm|$ROOT/dotnet|-p:WispRequestProbe=true|"
  "wisp_guest_dotnet|$ROOT/dotnet/out/wisp_guest_dotnet.wasm|$ROOT/dotnet||"
  "wisp_guest_dotnet_compat|$ROOT/dotnet/out/wisp_guest_dotnet_compat.wasm|$ROOT/dotnet|-p:WispCompatProbe=true|"
  "wisp_guest_dotnet_entropy|$ROOT/dotnet/out/wisp_guest_dotnet_entropy.wasm|$ROOT/dotnet|-p:WispEntropyProbe=true|"
  "wisp_guest_dotnet_stock|$ROOT/dotnet/out/wisp_guest_dotnet_stock.wasm|$ROOT/dotnet|-p:WispPalObject=|gate-fails"
  "wisp_guest_dotnet_compat_stock|$ROOT/dotnet/out/wisp_guest_dotnet_compat_stock.wasm|$ROOT/dotnet|-p:WispCompatProbe=true -p:WispPalObject=|gate-fails"
)

# Coarse on purpose: a per-fixture dependency graph would be a second copy of
# what MSBuild knows, and a stale fixture costs more than a spare rebuild.
newest_source() {
  python3 "$TOOLS" newest "$ROOT/dotnet" "$ROOT/../guest-sdks/dotnet" "$TOOLS" "$ROOT/build-dotnet.sh" "$ROOT/../nuget.config"
}

is_stale() {
  local out="$1" src="$2"
  [ -f "$out" ] || return 0
  local t; t=$(python3 "$TOOLS" mtime "$out")
  awk -v a="$t" -v b="$src" 'BEGIN { exit !(a < b) }'
}

SRC_T=$(newest_source)
: "${SRC_T:=0}"

if [ "$IF_STALE" = 1 ]; then
  fresh=1
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra gate <<<"$spec"
    if is_stale "$out" "$SRC_T"; then fresh=0; break; fi
  done
  if [ "$fresh" = 1 ]; then
    echo "==> C# fixtures are up to date"
    exit 0
  fi
fi

# Checked only once a build is needed: fresh fixtures need no SDK.
if ! command -v dotnet >/dev/null 2>&1; then
  {
    echo "FATAL: no .NET SDK on PATH, so the C# guest fixtures cannot be built."
    echo "Install the .NET SDK to build C# fixtures."
  } >&2
  exit 1
fi

rc=0
built=0 skipped=0
for spec in "${FIXTURES[@]}"; do
  IFS='|' read -r name out dir extra gate <<<"$spec"
  if [ "$IF_STALE" = 1 ] && ! is_stale "$out" "$SRC_T"; then
    skipped=$((skipped + 1))
    continue
  fi
  log=$(mktemp)
  ok=0
  build_rc=0
  rm -f "$out"
  # shellcheck disable=SC2086
  if WISP_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1; then ok=1; else build_rc=$?; fi
  built=$((built + 1))

  # `build-guest.sh`'s import gate refuses the `_stock` fixtures after the
  # module is on disk, so for them a clean build is the failure.
  if [ "$gate" = "gate-fails" ]; then
    if [ "$ok" = 1 ]; then
      echo "  !! $name: passed the import gate, and it exists to fail it"; rc=1
    elif [ "$build_rc" = 3 ] && [ -s "$out" ]; then
      echo "  ok      $name  (import gate refused it, as this fixture requires)"
    else
      echo "  FAILED  $name  (compiler failed or import gate did not reject the module)"; sed 's/^/      /' "$log" >&2; rc=1
    fi
  elif [ "$ok" = 1 ]; then
    echo "  ok      $name"
  else
    echo "  FAILED  $name"; sed 's/^/      /' "$log" >&2; rc=1
  fi
  rm -f "$log"
done

echo
echo "==> built $built, skipped $skipped (up to date)"
[ "$rc" = 0 ] || echo "BUILD FAILED — see the log above" >&2
exit $rc
