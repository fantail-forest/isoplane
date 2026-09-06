#!/usr/bin/env bash
# Validate the linked core module and reject imports outside Wisp.
set -euo pipefail
wasm-tools validate "$1"
module=$(wasm-tools print "$1")
stray=$(printf '%s\n' "$module" | grep -E '^\s*\(import' | grep -v '(import "wisp"' || true)
if [ -n "$stray" ]; then
  printf 'wisp: undeclared imports survived the PAL link:\n%s\n' "$stray" >&2
  exit 1
fi
