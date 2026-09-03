#!/usr/bin/env bash
# Build every Rust guest to guests/out/*.wasm and list them in guests/out/manifest.
#
#   scripts/dev guests/build.sh            # build all
#   scripts/dev guests/build.sh --if-stale # only when a source is newer than an output
#
# Guests are built for size: the release profile in guests/Cargo.toml is
# opt-level="z" + lto + panic="abort" + strip.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$ROOT/out"
MANIFEST="$OUT/manifest"
IF_STALE=0
[ "${1:-}" = "--if-stale" ] && IF_STALE=1

# Tests call this concurrently from several test binaries; the second caller
# blocks, then finds everything fresh.
LOCK="$ROOT/.build.lock"
if [ -z "${WISP_BUILD_LOCKED:-}" ] && command -v flock >/dev/null 2>&1; then
  export WISP_BUILD_LOCKED=1
  exec flock "$LOCK" "$0" "$@"
fi

# The guests depend on the Rust SDK and the contract crate by path, so a
# change to either is a change to every module.
newest_source() {
  find "$ROOT" "$ROOT/../guest-sdks/rust" "$ROOT/../crates/wisp-abi" \
       \( -name out -o -name target -o -name dotnet \) -prune -o \
       -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \
                  -o -name 'config.toml' -o -name 'build.sh' \) \
       -printf '%T@\n' 2>/dev/null | sort -rn | head -1
}

is_fresh() {
  [ -f "$MANIFEST" ] || return 1
  local src; src=$(newest_source); : "${src:=0}"
  local g f t
  while read -r g; do
    f="$OUT/$g.wasm"
    [ -f "$f" ] || return 1
    t=$(stat -c %Y "$f")
    awk -v a="$t" -v b="$src" 'BEGIN { exit !(a >= b) }' || return 1
  done < "$MANIFEST"
}

if [ "$IF_STALE" = 1 ] && is_fresh; then
  echo "==> $OUT is up to date"
  exit 0
fi
TARGET=wasm32-unknown-unknown
PROFILE=release
# Separate guest build outputs avoid locking the parent host build directory.
TDIR="${CARGO_TARGET_DIR:+${CARGO_TARGET_DIR%/}/guests}"
TDIR="${TDIR:-$ROOT/target}"
export CARGO_TARGET_DIR="$TDIR"

# WISP_BUILD_QUIET skips informational size and import/export reports.
QUIET="${WISP_BUILD_QUIET:-}"
mkdir -p "$OUT"
cd "$ROOT"

CARGO_FLAGS=(--target "$TARGET" --profile "$PROFILE")
[ -n "${WISP_OFFLINE:-}" ] && CARGO_FLAGS+=(--offline)

# Check tools before replacing existing outputs.
preflight_missing=()
command -v cargo >/dev/null 2>&1 || preflight_missing+=(
  "cargo is not on PATH -- the guests are a cargo workspace of their own")
# rustup prints a target-libdir for any triple it can name, installed or not,
# and only creates the directory when the target is really there.
if ! libdir=$(rustc --print target-libdir --target "$TARGET" 2>/dev/null) || [ ! -d "$libdir" ]; then
  preflight_missing+=("the $TARGET std is not installed (rustup target add $TARGET)")
fi
# V2 is checked with this, and an unverified guest is not known to be a legal
# WISP module. V1 -- the capability surface -- needs nothing here: it is
# checked by `wisp_control::validate` from a Rust test, not by this script.
command -v wasm-tools >/dev/null 2>&1 || preflight_missing+=(
  "wasm-tools is not on PATH, so V2 could not be checked")
if [ "${#preflight_missing[@]}" -ne 0 ]; then
  echo "FATAL: refusing to build. Nothing in $OUT has been touched." >&2
  for m in "${preflight_missing[@]}"; do
    echo "  - $m" >&2
  done
  echo "Install the missing tools, then rerun this command:" >&2
  echo "    scripts/dev guests/build.sh" >&2
  exit 1
fi

# `undeclared-import` is built twice, once per variant.
GUESTS=(quotes-api misbehaver bigheap impure-cctor undeclared-import)

# Remove only outputs owned by this script; other toolchains share the directory.
echo "==> building guests for $TARGET ($PROFILE)"
for g in "${GUESTS[@]}"; do
  rm -f "$OUT/$g.wasm"
  cargo build -p "$g" "${CARGO_FLAGS[@]}"
  cp "$TDIR/$TARGET/$PROFILE/${g//-/_}.wasm" "$OUT/$g.wasm"
done

# V1 fixture, variant 2: an undeclared MODULE rather than an undeclared field.
rm -f "$OUT/foreign-module.wasm"
cargo build -p undeclared-import --features foreign-module "${CARGO_FLAGS[@]}"
cp "$TDIR/$TARGET/$PROFILE/undeclared_import.wasm" "$OUT/foreign-module.wasm"

if [ -z "$QUIET" ]; then
  echo
  echo "==> sizes"
  printf '%-24s %10s %10s %10s\n' MODULE BYTES GZIP ZSTD
  for f in "$OUT"/*.wasm; do
    raw=$(stat -c %s "$f")
    gz=$(gzip -9 -c "$f" | wc -c)
    if command -v zstd >/dev/null 2>&1; then
      zs=$(zstd -19 -c "$f" 2>/dev/null | wc -c)
    else
      zs=-
    fi
    printf '%-24s %10s %10s %10s\n' "$(basename "$f")" "$raw" "$gz" "$zs"
  done
fi


echo
echo "==> validation (threads/atomics OFF, as rule V2 requires)"
# Every module must pass V2, the two V1 fixtures included: an undeclared import
# is a capability question, not a well-formedness one, and both fixtures are
# otherwise perfectly ordinary wasm.
v2_failed=()
for f in "$OUT"/*.wasm; do
  # V2: threads / shared memory / atomics OFF, relaxed SIMD OFF, exceptions
  # beyond the profile OFF. With those disabled the blocking instructions
  # (`memory.atomic.wait*`/`notify`) fail bytecode validation outright, which
  # is the layer that makes blocking inexpressible in-sandbox.
  if wasm-tools validate -f=-threads,-shared-everything-threads,-relaxed-simd,-exceptions,-legacy-exceptions "$f" 2>/dev/null; then
    echo "  ok        $(basename "$f")"
  else
    echo "  INVALID   $(basename "$f")"
    v2_failed+=("$(basename "$f")")
  fi
done

if [ -z "$QUIET" ]; then
  echo
  echo "==> import/export surface"
  for f in "$OUT"/*.wasm; do
    echo "--- $(basename "$f")"
    echo -n "    imports: "
    wasm-tools print "$f" | grep -oE '\(import "[^"]+" "[^"]+"' \
      | sed -E 's/\(import "([^"]+)" "([^"]+)"/\1::\2/' | sort -u | tr '\n' ' '
    echo
    echo -n "    exports: "
    wasm-tools print "$f" | grep -oE '\(export "[^"]+"' \
      | sed -E 's/\(export "([^"]+)"/\1/' | sort -u | tr '\n' ' '
    echo
  done
fi

echo
echo "==> V1 (capability surface) is checked by the validator, not by this script"
# Import names and signatures are checked by the host validator tests.

if [ "${#v2_failed[@]}" -ne 0 ]; then
  rm -f "$MANIFEST"
  echo >&2
  echo "BUILD FAILED -- the modules are on disk and at least one is not a legal" >&2
  echo "WISP guest, so nothing here is measuring the platform tenants get:" >&2
  for m in ${v2_failed[@]+"${v2_failed[@]}"}; do
    echo "  V2 (wasm-tools validate): $m" >&2
  done
  exit 1
fi

# The manifest is what tests enumerate, so it is written only once every
# module in it has been built and verified.
printf '%s\n' "${GUESTS[@]}" foreign-module > "$MANIFEST"
echo
echo "==> wrote $(wc -l < "$MANIFEST") modules to $OUT"
