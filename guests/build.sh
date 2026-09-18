#!/usr/bin/env bash
# Build every Rust guest to guests/out/*.wasm and list them in guests/out/manifest.
#
#   scripts/dev guests/build.sh            # build all
#   scripts/dev guests/build.sh --if-stale # only when inputs or outputs differ from the build receipt
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$ROOT/out"
MANIFEST="$OUT/manifest"
IF_STALE=0
[ "${1:-}" = "--if-stale" ] && IF_STALE=1

TOOLS="$ROOT/../scripts/fixture-tools.py"
if [ "${ISOPLANE_FIXTURE_LOCK:-}" != "$ROOT/build.sh" ]; then
  exec python3 "$TOOLS" lock "$ROOT/.build.lock" "$ROOT/build.sh" "$@"
fi

INPUTS=$(python3 "$TOOLS" fingerprint "$ROOT" "$ROOT/../guest-sdks/rust" "$ROOT/../crates/isoplane-abi" "$TOOLS" "$ROOT/build.sh")

is_fresh() {
  [ -f "$MANIFEST" ] || return 1
  local outputs=("$MANIFEST") g
  while read -r g; do outputs+=("$OUT/$g.wasm"); done < "$MANIFEST"
  python3 "$TOOLS" fresh "$OUT/.fixture-receipt.json" "$INPUTS" "${outputs[@]}"
}

if [ "$IF_STALE" = 1 ] && is_fresh; then
  echo "==> $OUT is up to date"
  exit 0
fi
TARGET=wasm32-unknown-unknown
PROFILE=release
# A target directory of the guests' own: a host `cargo test` that runs this
# holds the lock on its build directory, and a nested cargo sharing it would
# wait on its parent forever.
TDIR="${CARGO_TARGET_DIR:+${CARGO_TARGET_DIR%/}/guests}"
TDIR="${TDIR:-$ROOT/target}"
export CARGO_TARGET_DIR="$TDIR"
export CARGO_BUILD_BUILD_DIR="$TDIR/build"

# ISOPLANE_BUILD_QUIET drops the size and import/export reports, which a test
# running this never reads, and keeps every check.
QUIET="${ISOPLANE_BUILD_QUIET:-}"
mkdir -p "$OUT"
cd "$ROOT"

CARGO_FLAGS=(--target "$TARGET" --profile "$PROFILE")
[ -n "${ISOPLANE_OFFLINE:-}" ] && CARGO_FLAGS+=(--offline)

# Everything this script needs is checked before anything is deleted.
preflight_missing=()
command -v cargo >/dev/null 2>&1 || preflight_missing+=(
  "cargo is not on PATH -- the guests are a cargo workspace of their own")
# rustup prints a target-libdir for any triple it can name and creates the
# directory only when the target is installed.
if ! libdir=$(rustc --print target-libdir --target "$TARGET" 2>/dev/null) || [ ! -d "$libdir" ]; then
  preflight_missing+=("the $TARGET std is not installed (rustup target add $TARGET)")
fi
command -v wasm-tools >/dev/null 2>&1 || preflight_missing+=(
  "wasm-tools is not on PATH, so V2 could not be checked")
if [ "${#preflight_missing[@]}" -ne 0 ]; then
  echo "FATAL: refusing to build. Nothing in $OUT has been touched." >&2
  for m in "${preflight_missing[@]}"; do
    echo "  - $m" >&2
  done

  exit 1
fi

# `undeclared-import` is built twice, once per variant.
GUESTS=(hello quotes-api misbehaver bigheap impure-cctor undeclared-import)

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
    raw=$(python3 "$TOOLS" size "$f")
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
# The two V1 fixtures included: an undeclared import is a capability question,
# not a well-formedness one.
v2_failed=()
for f in "$OUT"/*.wasm; do
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

# V1, the capability surface, is checked by `isoplane_control::validate` in the
# `real_guests` test, which holds every module in the manifest to a recorded
# verdict; a shell copy of the import table would drift and could not check
# signatures.

if [ "${#v2_failed[@]}" -ne 0 ]; then
  rm -f "$MANIFEST"
  echo >&2
  echo "BUILD FAILED -- the modules are on disk and at least one is not a legal" >&2
  echo "Isoplane guest, so nothing here is measuring the platform tenants get:" >&2
  for m in ${v2_failed[@]+"${v2_failed[@]}"}; do
    echo "  V2 (wasm-tools validate): $m" >&2
  done
  exit 1
fi

# Written only once every module in it has been built and verified.
printf '%s\n' "${GUESTS[@]}" foreign-module > "$MANIFEST"
echo
echo "==> wrote $(wc -l < "$MANIFEST") modules to $OUT"

outputs=("$MANIFEST")
while read -r g; do outputs+=("$OUT/$g.wasm"); done < "$MANIFEST"
python3 "$TOOLS" record "$OUT/.fixture-receipt.json" "$INPUTS" "${outputs[@]}"
