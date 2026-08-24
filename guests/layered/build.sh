#!/usr/bin/env bash
# Build the §4.6 layered-runtime fixtures.
#
# Compiled on the HOST (docs/AGENT-RULES.md rule 10: compile on the host,
# measure in the sandbox). Nothing here is timed.
#
# Three artifact families land in guests/layered/out:
#
#   base.wasm             the shared runtime layer (§4.6). Linked with
#                         --initial-memory=8388608, which reserves the window
#                         [4 MiB, 8 MiB) for an app layer's static data and
#                         shadow stack and puts the runtime heap at 8 MiB.
#   app-<t>.wasm          tenant <t>'s APP LAYER: --import-memory, so it has no
#                         linear memory of its own and links against the base's
#                         at instantiation, and --global-base=4194304 so its
#                         data lands in the reserved window.
#   selfcontained-<t>.wasm the same tenant source with the identical runtime
#                         code statically linked and fat-LTO trimmed (§4.2).
#
# §4.3 V2 forbids threads/shared memory/atomics, and none of this uses them:
# the base's memory is an ordinary unshared memory, imported by one app layer
# per instance. Cross-tenant sharing is copy-on-write of a common template,
# never a shared mutable heap. See docs/layered-runtime.md §2.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$ROOT/out"
TARGET=wasm32-unknown-unknown
TDIR="${CARGO_TARGET_DIR:-$ROOT/../target}"
REL="$TDIR/$TARGET/release"
APPS=(quotes chat etl hello audit search)

# The app window. Both numbers are load-bearing and both are asserted at
# measurement time by `wisp-dotnet-sharing layered`.
APP_BASE=4194304          # 4 MiB — where an app layer's static data starts
APP_STACK=262144          # 256 KiB shadow stack, inside the window
BASE_MEM=8388608          # 8 MiB initial — window + heap start

mkdir -p "$OUT"
rm -f "$OUT"/*.wasm
cd "$ROOT/.."

echo "==> app layers (§4.6: imports the runtime, owns no memory)"
for a in "${APPS[@]}"; do
  cargo rustc -p "layered-$a" --target "$TARGET" --release --features layered -- \
    -C link-arg=--import-memory \
    -C link-arg=--global-base=$APP_BASE \
    -C link-arg=-zstack-size=$APP_STACK \
    -C link-arg=--export=__stack_pointer
  cp "$REL/layered_$a.wasm" "$OUT/app-$a.wasm"
done

echo "==> base layer (the shared runtime, un-trimmed)"
cargo rustc -p wisp-layer-base --target "$TARGET" --release -- \
  -C link-arg=--initial-memory=$BASE_MEM \
  -C link-arg=--export=__stack_pointer
cp "$REL/wisp_layer_base.wasm" "$OUT/base.wasm"

echo "==> self-contained equivalents (§4.2: same code, statically linked, LTO-trimmed)"
for a in "${APPS[@]}"; do
  cargo rustc -p "layered-$a" --target "$TARGET" --release --features selfcontained -- \
    -C link-arg=--export=__stack_pointer
  cp "$REL/layered_$a.wasm" "$OUT/selfcontained-$a.wasm"
done

echo
printf '%-28s %10s %10s\n' ARTIFACT BYTES ZSTD19
for f in "$OUT"/base.wasm "$OUT"/app-*.wasm "$OUT"/selfcontained-*.wasm; do
  raw=$(stat -c %s "$f")
  if command -v zstd >/dev/null 2>&1; then z=$(zstd -19 -c "$f" 2>/dev/null | wc -c); else z=-; fi
  printf '%-28s %10s %10s\n' "$(basename "$f")" "$raw" "$z"
done
