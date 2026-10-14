#!/usr/bin/env bash
# Build SQLite for the shared runtime from the release pinned in sqlite.env:
# e_sqlite3.a, the bindings generated from its C API, and native-imports.txt
# naming the functions it provides, in DIRECTORY.
# Usage: guest-sdks/dotnet/MonoRuntime/sqlite/build.sh --output DIRECTORY
# WASI_SDK_PATH names the wasi-sdk the runtime links with.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=sqlite.env
source "$HERE/sqlite.env"

OUTPUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUTPUT="${2:?--output needs a directory}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUTPUT" ] || { echo "missing --output DIRECTORY" >&2; exit 2; }
WASI_SDK="${WASI_SDK_PATH:?set WASI_SDK_PATH to the runtime wasi-sdk}"
[ -x "$WASI_SDK/bin/clang" ] || { echo "no clang in WASI_SDK_PATH=$WASI_SDK" >&2; exit 1; }
PYTHON="${ISOPLANE_PYTHON:-python3}"

mkdir -p "$OUTPUT/downloads"
OUTPUT="$(cd "$OUTPUT" && pwd)"
ARCHIVE="$OUTPUT/downloads/sqlite-amalgamation-$SQLITE_VERSION.zip"

matches_pin() {
  [ "$("$PYTHON" -I -c 'import hashlib, sys; print(hashlib.sha3_256(open(sys.argv[1], "rb").read()).hexdigest())' "$1")" \
    = "$SQLITE_AMALGAMATION_SHA3_256" ]
}

if ! { [ -f "$ARCHIVE" ] && matches_pin "$ARCHIVE"; }; then
  curl --fail --silent --show-error --location --retry 3 --output "$ARCHIVE.part" "$SQLITE_AMALGAMATION_URL"
  matches_pin "$ARCHIVE.part" || { echo "$SQLITE_AMALGAMATION_URL does not match its pinned SHA3-256" >&2; exit 1; }
  mv "$ARCHIVE.part" "$ARCHIVE"
fi

WORK="$(mktemp -d "$OUTPUT/build.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
unzip -q -j "$ARCHIVE" '*/sqlite3.c' '*/sqlite3.h' -d "$WORK"

COMPILE=("$WASI_SDK/bin/clang" --target=wasm32-wasip2 "--sysroot=$WASI_SDK/share/wasi-sysroot" -O2 -I "$WORK" -I "$HERE")
"${COMPILE[@]}" -c "$HERE/isoplane_sqlite.c" -o "$WORK/isoplane_sqlite.o"
"$WASI_SDK/bin/llvm-nm" --defined-only --extern-only --format=just-symbols "$WORK/isoplane_sqlite.o" > "$WORK/exports.txt"
"${COMPILE[@]}" -fsyntax-only -Xclang -ast-dump=json -x c "$WORK/sqlite3.h" > "$WORK/sqlite3.ast.json"
"$PYTHON" -I "$HERE/bindings.py" "$WORK/sqlite3.ast.json" "$WORK/exports.txt" "$WORK/SqliteImports.g.cs" "$WORK/sqlite_callback_pool.c" "$WORK/native-imports.txt"
"${COMPILE[@]}" -c "$WORK/sqlite_callback_pool.c" -o "$WORK/sqlite_callback_pool.o"
"$WASI_SDK/bin/llvm-ar" rcs "$WORK/e_sqlite3.a" "$WORK/isoplane_sqlite.o" "$WORK/sqlite_callback_pool.o"

mv -f "$WORK/SqliteImports.g.cs" "$OUTPUT/SqliteImports.g.cs"
mv -f "$WORK/native-imports.txt" "$OUTPUT/native-imports.txt"
mv -f "$WORK/e_sqlite3.a" "$OUTPUT/e_sqlite3.a"
echo "==> SQLite $SQLITE_VERSION: $OUTPUT/e_sqlite3.a"
