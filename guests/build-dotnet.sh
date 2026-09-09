#!/usr/bin/env bash
# Build the C# test fixtures; samples have their own build scripts.
# Usage: guests/build-dotnet.sh [--if-stale] [--fixture name]
# Serial because the variants share one project's intermediate files.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IF_STALE=0
SELECTED=""
ARGS=("$@")
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --fixture)
      [ $# -ge 2 ] || { echo "missing fixture name" >&2; exit 2; }
      SELECTED="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

TOOLS="$ROOT/../scripts/fixture-tools.py"
if [ "${WISP_FIXTURE_LOCK:-}" != "$ROOT/build-dotnet.sh" ]; then
  exec python3 "$TOOLS" lock "$ROOT/.build-dotnet.lock" "$ROOT/build-dotnet.sh" "${ARGS[@]}"
fi

# name | output path | guest dir | extra dotnet publish args
#
# `_stock` is built without the PAL shim object, so it carries the raw
# `wasi:*` imports NativeAOT-LLVM emits; `wisp-engine`'s `dotnet_pal` test
# reads it to check what the adapter composes them down to.
FIXTURES=(
  "wisp_guest_dotnet_bc_randomness|$ROOT/dotnet/out/wisp_guest_dotnet_bc_randomness.wasm|$ROOT/dotnet|-p:WispBcRandomnessProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_certificates|$ROOT/dotnet/out/wisp_guest_dotnet_certificates.wasm|$ROOT/dotnet|-p:WispCertificateProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_streaming|$ROOT/dotnet/out/wisp_guest_dotnet_streaming.wasm|$ROOT/dotnet|-p:WispStreamingProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_hashing|$ROOT/dotnet/out/wisp_guest_dotnet_hashing.wasm|$ROOT/dotnet|-p:WispHashingProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_randomness|$ROOT/dotnet/out/wisp_guest_dotnet_randomness.wasm|$ROOT/dotnet|-p:WispRandomnessProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_config|$ROOT/dotnet/out/wisp_guest_dotnet_config.wasm|$ROOT/dotnet|-p:WispConfigProbe=true"
  "wisp_guest_dotnet_failing_boot|$ROOT/dotnet/out/wisp_guest_dotnet_failing_boot.wasm|$ROOT/dotnet|-p:WispFailingBoot=true"
  "wisp_guest_dotnet_network_refusal|$ROOT/dotnet/out/wisp_guest_dotnet_network_refusal.wasm|$ROOT/dotnet|-p:WispNetworkRefusalProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_dns|$ROOT/dotnet/out/wisp_guest_dotnet_dns.wasm|$ROOT/dotnet|-p:WispDnsProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_stream|$ROOT/dotnet/out/wisp_guest_dotnet_stream.wasm|$ROOT/dotnet|-p:WispStreamProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_connect|$ROOT/dotnet/out/wisp_guest_dotnet_connect.wasm|$ROOT/dotnet|-p:WispConnectProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_timer|$ROOT/dotnet/out/wisp_guest_dotnet_timer.wasm|$ROOT/dotnet|-p:WispTimerProbe=true"
  "wisp_guest_dotnet_async|$ROOT/dotnet/out/wisp_guest_dotnet_async.wasm|$ROOT/dotnet|-p:WispAsyncProbe=true"
  "wisp_guest_dotnet_filesystem|$ROOT/dotnet/out/wisp_guest_dotnet_filesystem.wasm|$ROOT/dotnet|-p:WispFilesystemProbe=true"
  "wisp_guest_aspnet|$ROOT/dotnet/out/wisp_guest_aspnet.wasm|$ROOT/aspnet|"
  "wisp_guest_dotnet_request|$ROOT/dotnet/out/wisp_guest_dotnet_request.wasm|$ROOT/dotnet|-p:WispRequestProbe=true"
  "wisp_guest_dotnet|$ROOT/dotnet/out/wisp_guest_dotnet.wasm|$ROOT/dotnet|"
  "wisp_guest_dotnet_compat|$ROOT/dotnet/out/wisp_guest_dotnet_compat.wasm|$ROOT/dotnet|-p:WispCompatProbe=true"
  "wisp_guest_dotnet_entropy|$ROOT/dotnet/out/wisp_guest_dotnet_entropy.wasm|$ROOT/dotnet|-p:WispEntropyProbe=true"
  "wisp_guest_dotnet_stock|$ROOT/dotnet/out/wisp_guest_dotnet_stock.wasm|$ROOT/dotnet|-p:WispStock=true"
  "wisp_guest_dotnet_compat_stock|$ROOT/dotnet/out/wisp_guest_dotnet_compat_stock.wasm|$ROOT/dotnet|-p:WispCompatProbe=true -p:WispStock=true"
)

if [ -n "$SELECTED" ]; then
  selected=()
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra <<<"$spec"
    if [ "$name" = "$SELECTED" ]; then selected+=("$spec"); fi
  done
  [ "${#selected[@]}" -eq 1 ] || { echo "unknown C# fixture: $SELECTED" >&2; exit 2; }
  FIXTURES=("${selected[@]}")
fi

# Coarse on purpose: a per-fixture dependency graph would be a second copy of
# what MSBuild knows, and a stale fixture costs more than a spare rebuild.
newest_source() {
  python3 "$TOOLS" newest "$ROOT/aspnet" "$ROOT/dotnet" "$ROOT/../guest-sdks/dotnet" "$TOOLS" "$ROOT/build-dotnet.sh" "$ROOT/../nuget.config"
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
    IFS='|' read -r name out dir extra <<<"$spec"
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
  IFS='|' read -r name out dir extra <<<"$spec"
  if [ "$IF_STALE" = 1 ] && ! is_stale "$out" "$SRC_T"; then
    skipped=$((skipped + 1))
    continue
  fi
  log=$(mktemp)
  ok=0
  rm -f "$out"
  # shellcheck disable=SC2086
  if WISP_OUT_DIR="$(dirname "$out")" WISP_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1; then ok=1; fi
  built=$((built + 1))

  if [ "$ok" = 1 ]; then
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
