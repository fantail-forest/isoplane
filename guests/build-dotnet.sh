#!/usr/bin/env bash
# Build every C# guest fixture, one at a time.
#
#   scripts/dev guests/build-dotnet.sh            # build all six
#   scripts/dev guests/build-dotnet.sh --if-stale # only what is out of date
#
# Serial because five of the six are one .csproj under different properties,
# sharing `src/obj`, `src/bin`, `obj/wisp_pal.o` and the SDK project reference;
# building them concurrently hit three distinct MSBuild races (a duplicate
# AssemblyInfo from an intermediate directory inside `src/`, and two
# NETSDK1047 restores over one `obj/`).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IF_STALE=0
[ "${1:-}" = "--if-stale" ] && IF_STALE=1

# Test binaries call this concurrently; the second caller blocks, then finds
# everything fresh. The lock lives on fd 9, and each build below is started
# without it: an MSBuild node server outlives the build that started it, and
# one that inherited the lock held every later caller until it was killed.
exec 9>"$ROOT/.build-dotnet.lock"
command -v flock >/dev/null 2>&1 && flock 9

# name | output path | guest dir | extra dotnet publish args | expect-gate-fail
#
# `_stock` is built without the PAL shim object, so it carries the raw
# `wasi:*` imports NativeAOT-LLVM emits; `wisp-engine`'s `dotnet_pal` test
# reads it to check what the adapter composes them down to.
FIXTURES=(
  "wisp_guest_dotnet|$ROOT/dotnet/out/wisp_guest_dotnet.wasm|$ROOT/dotnet||"
  "wisp_guest_dotnet_compat|$ROOT/dotnet/out/wisp_guest_dotnet_compat.wasm|$ROOT/dotnet|-p:WispCompatProbe=true|"
  "wisp_guest_dotnet_entropy|$ROOT/dotnet/out/wisp_guest_dotnet_entropy.wasm|$ROOT/dotnet|-p:WispEntropyProbe=true|"
  "wisp_guest_dotnet_stock|$ROOT/dotnet/out/wisp_guest_dotnet_stock.wasm|$ROOT/dotnet|-p:WispPalObject=|gate-fails"
  "wisp_guest_dotnet_compat_stock|$ROOT/dotnet/out/wisp_guest_dotnet_compat_stock.wasm|$ROOT/dotnet|-p:WispCompatProbe=true -p:WispPalObject=|gate-fails"
)

# Coarse on purpose: a per-fixture dependency graph would be a second copy of
# what MSBuild knows, and a stale fixture costs more than a spare rebuild.
newest_source() {
  find "$ROOT/dotnet/src" "$(cd "$ROOT/.." && pwd)/guest-sdks/dotnet" \
       -type f \( -name '*.cs' -o -name '*.csproj' -o -name '*.props' -o -name '*.c' -o -name '*.sh' \) \
       -not -path '*/obj*' -not -path '*/bin*' -printf '%T@\n' 2>/dev/null | sort -rn | head -1
}

is_stale() {
  local out="$1" src="$2"
  [ -f "$out" ] || return 0
  local t; t=$(stat -c %Y "$out" 2>/dev/null || echo 0)
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
    echo "Run this in the sandbox, where the SDK lives:"
    echo "    scripts/dev guests/build-dotnet.sh"
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
  # shellcheck disable=SC2086
  if WISP_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1 9>&-; then ok=1; fi
  built=$((built + 1))

  # `build-guest.sh`'s import gate refuses the `_stock` fixtures after the
  # module is on disk, so for them a clean build is the failure.
  if [ "$gate" = "gate-fails" ]; then
    if [ "$ok" = 1 ]; then
      echo "  !! $name: passed the import gate, and it exists to fail it"; rc=1
    elif [ -s "$out" ]; then
      echo "  ok      $name  (import gate refused it, as this fixture requires)"
    else
      echo "  FAILED  $name  (no module produced)"; sed 's/^/      /' "$log" >&2; rc=1
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
