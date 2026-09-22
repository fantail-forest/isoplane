#!/usr/bin/env bash
# Build the C# test fixtures; samples have their own build scripts.
# Usage: guests/build-dotnet.sh [--if-stale] [--fixture name] [--runtime nativeaot|mono]
# Serial because the variants share one project's intermediate files.
#
# `--runtime mono` publishes the interpreted flavour of the fixtures listed in
# MONO_FIXTURES to dotnet/out/mono, with its own intermediate directory so the
# two runtimes never share a restore or a link.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IF_STALE=0
SELECTED=""
RUNTIME=nativeaot
ARGS=("$@")
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) IF_STALE=1; shift ;;
    --fixture)
      [ $# -ge 2 ] || { echo "missing fixture name" >&2; exit 2; }
      SELECTED="$2"; shift 2 ;;
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

TOOLS="$ROOT/../scripts/fixture-tools.py"
if [ "${ISOPLANE_FIXTURE_LOCK:-}" != "$ROOT/build-dotnet.sh" ]; then
  exec python3 "$TOOLS" lock "$ROOT/.build-dotnet.lock" "$ROOT/build-dotnet.sh" "${ARGS[@]}"
fi

# name | output path | guest dir | extra dotnet publish args
#
# `_stock` is built without the PAL shim object, so it carries the raw
# `wasi:*` imports NativeAOT-LLVM emits; `isoplane-engine`'s `dotnet_pal` test
# reads it to check what the adapter composes them down to.
FIXTURES=(
  "isoplane_guest_dotnet_activation|$ROOT/dotnet/out/isoplane_guest_dotnet_activation.wasm|$ROOT/dotnet|-p:IsoplaneActivationProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_certificates|$ROOT/dotnet/out/isoplane_guest_dotnet_certificates.wasm|$ROOT/dotnet|-p:IsoplaneCertificateProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_streaming|$ROOT/dotnet/out/isoplane_guest_dotnet_streaming.wasm|$ROOT/dotnet|-p:IsoplaneStreamingProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_hashing|$ROOT/dotnet/out/isoplane_guest_dotnet_hashing.wasm|$ROOT/dotnet|-p:IsoplaneHashingProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_trust|$ROOT/dotnet/out/isoplane_guest_dotnet_trust.wasm|$ROOT/dotnet|-p:IsoplaneTrustProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_randomness|$ROOT/dotnet/out/isoplane_guest_dotnet_randomness.wasm|$ROOT/dotnet|-p:IsoplaneRandomnessProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_memory|$ROOT/dotnet/out/isoplane_guest_dotnet_memory.wasm|$ROOT/dotnet|-p:IsoplaneMemoryProbe=true -p:IsoplaneMemoryMaxBytes=4294967296 -p:IsoplaneGuestMemoryBytes=4294967296"
  "isoplane_guest_dotnet_config|$ROOT/dotnet/out/isoplane_guest_dotnet_config.wasm|$ROOT/dotnet|-p:IsoplaneConfigProbe=true"
  "isoplane_guest_dotnet_failing_boot|$ROOT/dotnet/out/isoplane_guest_dotnet_failing_boot.wasm|$ROOT/dotnet|-p:IsoplaneFailingBoot=true"
  "isoplane_guest_dotnet_network_refusal|$ROOT/dotnet/out/isoplane_guest_dotnet_network_refusal.wasm|$ROOT/dotnet|-p:IsoplaneNetworkRefusalProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_dns|$ROOT/dotnet/out/isoplane_guest_dotnet_dns.wasm|$ROOT/dotnet|-p:IsoplaneDnsProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_stream|$ROOT/dotnet/out/isoplane_guest_dotnet_stream.wasm|$ROOT/dotnet|-p:IsoplaneStreamProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_connect|$ROOT/dotnet/out/isoplane_guest_dotnet_connect.wasm|$ROOT/dotnet|-p:IsoplaneConnectProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_cleanup|$ROOT/dotnet/out/isoplane_guest_dotnet_cleanup.wasm|$ROOT/dotnet|-p:IsoplaneCleanupProbe=true -p:IlcResilient=false"
  "isoplane_guest_dotnet_timer|$ROOT/dotnet/out/isoplane_guest_dotnet_timer.wasm|$ROOT/dotnet|-p:IsoplaneTimerProbe=true"
  "isoplane_guest_dotnet_logs|$ROOT/dotnet/out/isoplane_guest_dotnet_logs.wasm|$ROOT/dotnet|-p:IsoplaneLogProbe=true"
  "isoplane_guest_dotnet_async|$ROOT/dotnet/out/isoplane_guest_dotnet_async.wasm|$ROOT/dotnet|-p:IsoplaneAsyncProbe=true"
  "isoplane_guest_dotnet_filesystem|$ROOT/dotnet/out/isoplane_guest_dotnet_filesystem.wasm|$ROOT/dotnet|-p:IsoplaneFilesystemProbe=true"
  "isoplane_guest_aspnet|$ROOT/dotnet/out/isoplane_guest_aspnet.wasm|$ROOT/aspnet|"
  "isoplane_guest_dotnet_request|$ROOT/dotnet/out/isoplane_guest_dotnet_request.wasm|$ROOT/dotnet|-p:IsoplaneRequestProbe=true"
  "isoplane_guest_dotnet|$ROOT/dotnet/out/isoplane_guest_dotnet.wasm|$ROOT/dotnet|"
  "isoplane_guest_dotnet_compat|$ROOT/dotnet/out/isoplane_guest_dotnet_compat.wasm|$ROOT/dotnet|-p:IsoplaneCompatProbe=true"
  "isoplane_guest_dotnet_entropy|$ROOT/dotnet/out/isoplane_guest_dotnet_entropy.wasm|$ROOT/dotnet|-p:IsoplaneEntropyProbe=true"
  "isoplane_guest_dotnet_stock|$ROOT/dotnet/out/isoplane_guest_dotnet_stock.wasm|$ROOT/dotnet|-p:IsoplaneStock=true"
  "isoplane_guest_dotnet_compat_stock|$ROOT/dotnet/out/isoplane_guest_dotnet_compat_stock.wasm|$ROOT/dotnet|-p:IsoplaneCompatProbe=true -p:IsoplaneStock=true"
)

# The fixtures that also publish under the Mono interpreter: the behaviour
# probes the engine and host tests drive in both runtimes. The stock, Durable
# Instance and ASP.NET flavours are NativeAOT-only for now.
MONO_FIXTURES=(
  isoplane_guest_dotnet_memory
  isoplane_guest_dotnet_activation
  isoplane_guest_dotnet_certificates
  isoplane_guest_dotnet_streaming
  isoplane_guest_dotnet_hashing
  isoplane_guest_dotnet_trust
  isoplane_guest_dotnet_randomness
  isoplane_guest_dotnet_network_refusal
  isoplane_guest_dotnet_dns
  isoplane_guest_dotnet_stream
  isoplane_guest_dotnet_connect
  isoplane_guest_dotnet_cleanup
  isoplane_guest_dotnet_timer
  isoplane_guest_dotnet_logs
  isoplane_guest_dotnet_async
  isoplane_guest_dotnet_filesystem
  isoplane_guest_dotnet_compat
  isoplane_guest_dotnet_entropy
)

if [ "$RUNTIME" = mono ]; then
  mono=()
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra <<<"$spec"
    for candidate in "${MONO_FIXTURES[@]}"; do
      if [ "$name" = "$candidate" ]; then
        mono+=("$name|$ROOT/dotnet/out/mono/$name.wasm|$dir|$extra -p:IsoplaneGuestRuntime=Mono --artifacts-path $ROOT/dotnet/obj-mono")
      fi
    done
  done
  FIXTURES=("${mono[@]}")
fi

if [ -n "$SELECTED" ]; then
  selected=()
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra <<<"$spec"
    if [ "$name" = "$SELECTED" ]; then selected+=("$spec"); fi
  done
  [ "${#selected[@]}" -eq 1 ] || { echo "unknown C# fixture for runtime $RUNTIME: $SELECTED" >&2; exit 2; }
  FIXTURES=("${selected[@]}")
fi

FEED="${ISOPLANE_TEST_RUNTIME_FEED:-$ROOT/../guest-runtime/out/feed}"
if [ -n "${ISOPLANE_TEST_RUNTIME_FEED:-}" ]; then
  python3 "$ROOT/../guest-runtime/feed-identity.py" verify-artifact "$FEED"
  echo "==> explicitly selected runtime artifacts: $FEED"
else
  python3 "$ROOT/../guest-runtime/feed-identity.py" verify-current "$ROOT/../guest-runtime" "$FEED"
fi
COMPILER_DIRECTORY="$(dirname "$FEED")/compiler"
COMPILER_INPUTS=()
if [ "$RUNTIME" != mono ]; then
  compiler_verification=verify-current
  [ -z "${ISOPLANE_TEST_RUNTIME_FEED:-}" ] || compiler_verification=verify-artifact
  python3 "$ROOT/../guest-runtime/compiler/build.py" "$compiler_verification" --output "$COMPILER_DIRECTORY"
  COMPILER_INPUTS+=("$COMPILER_DIRECTORY/compiler.json")
fi
export ISOPLANE_GUEST_RUNTIME_FEED="$FEED"
FEED_IDENTITY=$(python3 "$TOOLS" runtime-identity "$FEED/Isoplane.GuestRuntime.props" "$RUNTIME")
INPUTS=$(python3 "$TOOLS" fingerprint "$ROOT/aspnet" "$ROOT/dotnet" "$ROOT/../guest-sdks/dotnet" "$TOOLS" "$ROOT/build-dotnet.sh" "$ROOT/../nuget.config" "$ROOT/../global.json" "$FEED" "${COMPILER_INPUTS[@]}")

is_stale() {
  local out="$1"
  [ -f "$out.runtime" ] || return 0
  [ "$(cat "$out.runtime")" = "$FEED_IDENTITY" ] || return 0
  ! python3 "$TOOLS" fresh "$out.fixture.json" "$INPUTS" "$out" "$out.runtime"
}

if [ "$IF_STALE" = 1 ]; then
  fresh=1
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra <<<"$spec"
    if is_stale "$out"; then fresh=0; break; fi
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
  if [ "$IF_STALE" = 1 ] && ! is_stale "$out"; then
    skipped=$((skipped + 1))
    continue
  fi
  log=$(mktemp)
  ok=0
  rm -f "$out"
  # shellcheck disable=SC2086
  if ISOPLANE_OUT_DIR="$(dirname "$out")" ISOPLANE_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1; then ok=1; fi
  built=$((built + 1))

  if [ "$ok" = 1 ]; then
    python3 "$TOOLS" record "$out.fixture.json" "$INPUTS" "$out" "$out.runtime"
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
