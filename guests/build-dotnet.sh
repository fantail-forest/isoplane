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
if [ "${WISP_FIXTURE_LOCK:-}" != "$ROOT/build-dotnet.sh" ]; then
  exec python3 "$TOOLS" lock "$ROOT/.build-dotnet.lock" "$ROOT/build-dotnet.sh" "${ARGS[@]}"
fi

# name | output path | guest dir | extra dotnet publish args
#
# `_stock` is built without the PAL shim object, so it carries the raw
# `wasi:*` imports NativeAOT-LLVM emits; `wisp-engine`'s `dotnet_pal` test
# reads it to check what the adapter composes them down to.
FIXTURES=(
  "wisp_guest_dotnet_activation|$ROOT/dotnet/out/wisp_guest_dotnet_activation.wasm|$ROOT/dotnet|-p:WispActivationProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_certificates|$ROOT/dotnet/out/wisp_guest_dotnet_certificates.wasm|$ROOT/dotnet|-p:WispCertificateProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_streaming|$ROOT/dotnet/out/wisp_guest_dotnet_streaming.wasm|$ROOT/dotnet|-p:WispStreamingProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_hashing|$ROOT/dotnet/out/wisp_guest_dotnet_hashing.wasm|$ROOT/dotnet|-p:WispHashingProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_trust|$ROOT/dotnet/out/wisp_guest_dotnet_trust.wasm|$ROOT/dotnet|-p:WispTrustProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_randomness|$ROOT/dotnet/out/wisp_guest_dotnet_randomness.wasm|$ROOT/dotnet|-p:WispRandomnessProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_config|$ROOT/dotnet/out/wisp_guest_dotnet_config.wasm|$ROOT/dotnet|-p:WispConfigProbe=true"
  "wisp_guest_dotnet_failing_boot|$ROOT/dotnet/out/wisp_guest_dotnet_failing_boot.wasm|$ROOT/dotnet|-p:WispFailingBoot=true"
  "wisp_guest_dotnet_network_refusal|$ROOT/dotnet/out/wisp_guest_dotnet_network_refusal.wasm|$ROOT/dotnet|-p:WispNetworkRefusalProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_dns|$ROOT/dotnet/out/wisp_guest_dotnet_dns.wasm|$ROOT/dotnet|-p:WispDnsProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_stream|$ROOT/dotnet/out/wisp_guest_dotnet_stream.wasm|$ROOT/dotnet|-p:WispStreamProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_connect|$ROOT/dotnet/out/wisp_guest_dotnet_connect.wasm|$ROOT/dotnet|-p:WispConnectProbe=true -p:IlcResilient=false"
  "wisp_guest_dotnet_cleanup|$ROOT/dotnet/out/wisp_guest_dotnet_cleanup.wasm|$ROOT/dotnet|-p:WispCleanupProbe=true -p:IlcResilient=false"
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

# The fixtures that also publish under the Mono interpreter: the behaviour
# probes the engine and host tests drive in both runtimes. The stock, Durable
# Instance and ASP.NET flavours are NativeAOT-only for now.
MONO_FIXTURES=(
  wisp_guest_dotnet_activation
  wisp_guest_dotnet_certificates
  wisp_guest_dotnet_streaming
  wisp_guest_dotnet_hashing
  wisp_guest_dotnet_trust
  wisp_guest_dotnet_randomness
  wisp_guest_dotnet_network_refusal
  wisp_guest_dotnet_dns
  wisp_guest_dotnet_stream
  wisp_guest_dotnet_connect
  wisp_guest_dotnet_cleanup
  wisp_guest_dotnet_timer
  wisp_guest_dotnet_async
  wisp_guest_dotnet_filesystem
  wisp_guest_dotnet_compat
  wisp_guest_dotnet_entropy
)

if [ "$RUNTIME" = mono ]; then
  mono=()
  for spec in "${FIXTURES[@]}"; do
    IFS='|' read -r name out dir extra <<<"$spec"
    for candidate in "${MONO_FIXTURES[@]}"; do
      if [ "$name" = "$candidate" ]; then
        mono+=("$name|$ROOT/dotnet/out/mono/$name.wasm|$dir|$extra -p:WispGuestRuntime=Mono --artifacts-path $ROOT/dotnet/obj-mono")
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

FEED="${WISP_TEST_RUNTIME_FEED:-$ROOT/../guest-runtime/out/feed}"
if [ -n "${WISP_TEST_RUNTIME_FEED:-}" ]; then
  python3 "$ROOT/../guest-runtime/feed-identity.py" verify-artifact "$FEED"
  echo "==> explicitly selected runtime artifacts: $FEED"
else
  python3 "$ROOT/../guest-runtime/feed-identity.py" verify-current "$ROOT/../guest-runtime" "$FEED"
fi
export WISP_GUEST_RUNTIME_FEED="$FEED"
FEED_IDENTITY=$(python3 "$TOOLS" runtime-identity "$FEED/Wisp.GuestRuntime.props" "$RUNTIME")
INPUTS=$(python3 "$TOOLS" fingerprint "$ROOT/aspnet" "$ROOT/dotnet" "$ROOT/../guest-sdks/dotnet" "$TOOLS" "$ROOT/build-dotnet.sh" "$ROOT/../nuget.config" "$ROOT/../global.json" "$FEED")

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
  if WISP_OUT_DIR="$(dirname "$out")" WISP_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1; then ok=1; fi
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
