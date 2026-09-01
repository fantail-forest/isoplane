#!/usr/bin/env bash
# Build every C# guest fixture, in parallel.
#
#   scripts/dev guests/build-dotnet.sh            # build all six
#   scripts/dev guests/build-dotnet.sh --if-stale # only what is out of date
#
# ## Why this exists
#
# These six modules used to be **committed**, on the argument that building
# them needs a .NET SDK most checkouts do not have. That argument does not
# survive the Dockerfile, which installs the SDK, or `scripts/dev`, which is
# the enforced environment for every build and test in this repo: a checkout
# that cannot build them cannot run the tests that read them either. Meanwhile
# `guests/out/*.wasm` — the Rust guests — has always been gitignored and built
# on demand, so the tree had two rules for one kind of artifact, and the .NET
# half went stale the moment anyone edited the shared PAL.
#
# So: one command, same contract as `guests/build.sh`, and the tests invoke it
# themselves rather than asserting against a binary nobody rebuilt.
#
# ## Serial, deliberately, and it was measured before it was decided
#
# Five of the six are the SAME .csproj with different properties, sharing
# `src/obj`, `src/bin`, `obj/wisp_pal.o` and a ProjectReference to
# `Wisp.Sdk.csproj`. Building them concurrently was tried and abandoned after
# three distinct MSBuild races, each uncovered by fixing the last:
#
#   1. an isolated intermediate dir inside `src/` gets picked up by the default
#      compile glob -> CS0579 duplicate AssemblyInfo attributes;
#   2. global properties propagate into ProjectReferences, so the SDK project
#      wrote its `project.assets.json` over the guest's -> NETSDK1047;
#   3. with that fixed, all six restore the shared SDK into one `obj/` at once
#      and race there instead -> NETSDK1047 again, intermittently.
#
# The cost of serial is 47 s on a cold checkout and nothing at all when the
# fixtures are fresh, since `--if-stale` is a 39 ms no-op. A flaky build script
# invoked from `cargo test` is far more expensive than 47 seconds.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IF_STALE=0
[ "${1:-}" = "--if-stale" ] && IF_STALE=1

# Serialise concurrent invocations. `cargo test` runs one binary per test target
# in parallel, and three of them call this before reading a fixture — without
# the lock they would race on the same obj/ and out/ directories, which is the
# failure this script's whole isolation scheme exists to avoid. The second
# caller blocks, then finds everything fresh and does nothing.
LOCK="$ROOT/.build-dotnet.lock"
if [ -z "${WISP_BUILD_DOTNET_LOCKED:-}" ] && command -v flock >/dev/null 2>&1; then
  export WISP_BUILD_DOTNET_LOCKED=1
  exec flock "$LOCK" "$0" "$@"
fi

if ! command -v dotnet >/dev/null 2>&1; then
  {
    echo "FATAL: no .NET SDK on PATH, so the C# guest fixtures cannot be built."
    echo "They are no longer committed. Run this in the sandbox, where the SDK lives:"
    echo "    scripts/dev guests/build-dotnet.sh"
  } >&2
  exit 1
fi

# name | output path | guest dir | extra dotnet publish args | expect-gate-fail
#
# `_stock` means "without the PAL shim object", i.e. the raw NativeAOT-LLVM
# import surface — 32 `wasi:*` imports that V1 rejects. `dotnet_pal.rs` reads
# it to assert the adapter composes them down to exactly two capabilities, so
# it is a fixture whose whole value is that it is NOT import-gate-clean.
FIXTURES=(
  "wisp_guest_dotnet|$ROOT/dotnet/out/wisp_guest_dotnet.wasm|$ROOT/dotnet||"
  "wisp_guest_dotnet_compat|$ROOT/dotnet/out/wisp_guest_dotnet_compat.wasm|$ROOT/dotnet|-p:WispCompatProbe=true|"
  "wisp_guest_dotnet_entropy|$ROOT/dotnet/out/wisp_guest_dotnet_entropy.wasm|$ROOT/dotnet|-p:WispEntropyProbe=true|"
  "wisp_guest_dotnet_stock|$ROOT/dotnet/out/wisp_guest_dotnet_stock.wasm|$ROOT/dotnet|-p:WispPalObject=|gate-fails"
  "wisp_guest_dotnet_compat_stock|$ROOT/dotnet/out/wisp_guest_dotnet_compat_stock.wasm|$ROOT/dotnet|-p:WispCompatProbe=true -p:WispPalObject=|gate-fails"
)

# Sources any of them depends on. Deliberately coarse: the SDK, the PAL and the
# guest sources all invalidate everything, because a per-fixture dependency
# graph would be a second, subtly different copy of what MSBuild already knows.
# Being coarse costs a rebuild that was not needed; being clever costs a stale
# fixture that a test then asserts against, which is how this file came to
# exist.
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
  if WISP_OUT="$name" "$dir/build.sh" $extra >"$log" 2>&1; then ok=1; fi
  built=$((built + 1))

  # The two `_stock` fixtures are built WITHOUT the PAL shim object, so they
  # carry the ~32 raw `wasi:*` imports NativeAOT-LLVM emits — and
  # `build-guest.sh`'s import gate rejects exactly that, by design (invariant 4,
  # rule V1). Their whole value to `dotnet_pal.rs` is being the thing the gate
  # refuses: it asserts the adapter composes those 32 declarations down to two
  # capabilities. So for them a non-zero exit is expected and a *clean* build is
  # the failure. The module is on disk either way; the gate runs after the copy.
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
