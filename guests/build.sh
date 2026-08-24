#!/usr/bin/env bash
# Build Rust guest fixtures to guests/out/*.wasm.
# Usage: scripts/dev guests/build.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$ROOT/out"
TARGET=wasm32-unknown-unknown
PROFILE=release
mkdir -p "$OUT"
cd "$ROOT"

CARGO_FLAGS=(--target "$TARGET" --profile "$PROFILE")
[ -n "${WISP_OFFLINE:-}" ] && CARGO_FLAGS+=(--offline)

# name:crate pairs. `undeclared-import` is built twice, once per variant.
GUESTS=(quotes-api misbehaver bigheap impure-cctor undeclared-import)

echo "==> building guests for $TARGET ($PROFILE)"
for g in "${GUESTS[@]}"; do
  cargo build -p "$g" "${CARGO_FLAGS[@]}"
  cp "target/$TARGET/$PROFILE/${g//-/_}.wasm" "$OUT/$g.wasm"
done

# V1 fixture, variant 2: an undeclared MODULE rather than an undeclared field.
cargo build -p undeclared-import --features foreign-module "${CARGO_FLAGS[@]}"
cp "target/$TARGET/$PROFILE/undeclared_import.wasm" "$OUT/foreign-module.wasm"

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

if command -v wasm-tools >/dev/null 2>&1; then
  echo
  echo "==> validation (threads/atomics OFF, as rule V2 requires)"
  for f in "$OUT"/*.wasm; do
    if wasm-tools validate --features=mvp,bulk-memory,sign-ext,multi-value,nontrapping-float-to-int,reference-types "$f" 2>/dev/null; then
      echo "  ok        $(basename "$f")"
    else
      echo "  INVALID   $(basename "$f")"
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


fi

echo
echo "==> wrote $(ls -1 "$OUT"/*.wasm | wc -l) modules to $OUT"
