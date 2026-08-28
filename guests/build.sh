#!/usr/bin/env bash
# Build Rust guest fixtures to guests/out/*.wasm.
# Usage: scripts/dev guests/build.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$ROOT/out"
TARGET=wasm32-unknown-unknown
PROFILE=release
# `scripts/dev` points CARGO_TARGET_DIR at a volume outside the repo so
# container builds cannot leave root-owned artifacts in the working tree.
TDIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$OUT"
cd "$ROOT"

CARGO_FLAGS=(--target "$TARGET" --profile "$PROFILE")
[ -n "${WISP_OFFLINE:-}" ] && CARGO_FLAGS+=(--offline)

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

if ! command -v wasm-tools >/dev/null 2>&1; then
  echo >&2
  echo "FATAL: wasm-tools is required to validate the guest feature profile." >&2
  echo "Install wasm-tools, then rerun this command:" >&2
  echo "    scripts/dev guests/build.sh" >&2
  exit 1
fi

echo
echo "==> validation (threads/atomics OFF, as rule V2 requires)"
# Every module must pass V2, the two V1 fixtures included: an undeclared import
# is a capability question, not a well-formedness one, and both fixtures are
# otherwise perfectly ordinary wasm.
v2_failed=()
for f in "$OUT"/*.wasm; do
  # Validate the guest feature profile with threads and blocking atomics disabled.
  if wasm-tools validate -f=-threads,-shared-everything-threads,-relaxed-simd,-exceptions,-legacy-exceptions "$f" 2>/dev/null; then
    echo "  ok        $(basename "$f")"
  else
    echo "  INVALID   $(basename "$f")"
    v2_failed+=("$(basename "$f")")
  fi
done

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


echo
echo "==> wrote $(ls -1 "$OUT"/*.wasm | wc -l) modules to $OUT"

if [ "${#v2_failed[@]}" -ne 0 ]; then
  echo >&2
  echo "BUILD FAILED -- the modules are on disk and at least one is not a legal" >&2
  echo "WISP guest, so nothing here is measuring the platform tenants get:" >&2
  for m in ${v2_failed[@]+"${v2_failed[@]}"}; do
    echo "  V2 (wasm-tools validate): $m" >&2
  done
  exit 1
fi
