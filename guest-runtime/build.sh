#!/usr/bin/env bash
# Build the Isoplane guest runtime from pinned sources: guest-runtime/build.sh [stage...]
#
# Task owns dependencies; --compile runs one build action.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if command -v cygpath >/dev/null 2>&1; then
  HERE="$(cygpath -m "$HERE")"
fi
if [ "${1:-}" != --compile ]; then
  if [ "$#" = 0 ]; then exec task --dir "$HERE/.." runtime:compiler; fi
  tasks=()
  for stage in "$@"; do
    case "$stage" in
      runtime|feed) tasks+=(runtime:compiler) ;;
      *) tasks+=("runtime:$stage") ;;
    esac
  done
  exec task --dir "$HERE/.." "${tasks[@]}"
fi
shift
# shellcheck source=manifest.env
source "$HERE/manifest.env"
BUILD_HOST=linux
case "$(uname -s)" in
  MINGW*|MSYS*)
    BUILD_HOST=windows
    WASI_SDK_URL="$WASI_SDK_WINDOWS_URL"
    WASI_SDK_SHA256="$WASI_SDK_WINDOWS_SHA256"
    ;;
esac
WORK="${ISOPLANE_RUNTIME_WORK//\\//}"
echo "Runtime build work: $WORK"
OUT="$HERE/../.build/wasm32/runtime"
RUNTIME="$HERE/../vendor/dotnet-runtime"
OPENSSL="$HERE/../vendor/openssl"
OPENSSL_BUILD="$WORK/openssl-build"
WASI_SDK="$WORK/wasi-sdk"
ICU="$HERE/../vendor/icu"
DOWNLOADS="$WORK/downloads"
LOGS="$WORK/logs"
mkdir -p "$DOWNLOADS" "$LOGS" "$OUT"

fetch() {
  local url="$1" checksum="$2" destination="$3"
  if [ -f "$destination" ] && printf '%s  %s\n' "$checksum" "$destination" | sha256sum --check --status; then
    return
  fi
  curl --fail --location --retry 3 --output "$destination.part" "$url"
  printf '%s  %s\n' "$checksum" "$destination.part" | sha256sum --check
  mv "$destination.part" "$destination"
}

verify_archive() {
  local sha="$1" file="$2"
  printf '%s  %s\n' "$sha" "$file" | sha256sum --check --status || {
    echo "archive does not match its pinned SHA256: $file; run guest-runtime/build.sh acquire" >&2
    return 1
  }
}

stage_acquire() {
  echo "==> acquire"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
}

# The toolchain a build directory is configured with. CMake and Configure record
# resolved tool paths, so its real location identifies it as well as its version.
toolchain_identity() {
  realpath "$WASI_SDK"
  cat "$WASI_SDK/VERSION"
}

# Discards build directories configured with another toolchain, which an
# incremental build would otherwise keep compiling and archiving with. The
# first directory holds the record of the toolchain they were configured with.
reset_for_toolchain() {
  local stamp="$1/.isoplane-toolchain" directory
  if [ "$(cat "$stamp" 2>/dev/null)" != "$(toolchain_identity)" ]; then
    for directory in "$@"; do rm -rf "$directory"; done
  fi
  mkdir -p "$1"
  toolchain_identity > "$stamp"
}

stage_toolchain() {
  echo "==> toolchain"
  # The marker is what the runtime build checks before provisioning its own copy.
  if [ -f "$WASI_SDK/VERSION" ] && [ "$(head -n 1 "$WASI_SDK/VERSION" | tr -d '\r')" = "$WASI_SDK_VERSION" ]; then
    echo "    wasi-sdk $WASI_SDK_VERSION already unpacked"
  else
    verify_archive "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
    rm -rf "$WASI_SDK"
    mkdir -p "$WASI_SDK"
    tar --strip-components=1 -xzf - -C "$WASI_SDK" < "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
    echo "$WASI_SDK_VERSION" > "$WASI_SDK/wasi-sdk-version.txt"
    touch "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION"
  fi
  # Task compares this file to decide whether builds made with the toolchain are current.
  toolchain_identity > "$WORK/toolchain-identity"
}

stage_openssl() {
  echo "==> openssl (log: $LOGS/openssl-build.log)"
  reset_for_toolchain "$OPENSSL_BUILD"
  WASI_SDK_PATH="$WASI_SDK" bash "$OPENSSL/util/isoplane-build-wasi.sh" \
    "$OPENSSL_BUILD" "$OUT/openssl" "${CMAKE_BUILD_PARALLEL_LEVEL:-$(nproc)}" > "$LOGS/openssl-build.log" 2>&1
}

stage_icu() {
  echo "==> icu (log: $LOGS/icu-build.log)"
  bash "$RUNTIME/eng/isoplane/build-icu.sh" "$ICU" "$WORK" "$OUT" > "$LOGS/icu-build.log" 2>&1
}

runtime_stage() {
  ISOPLANE_RUNTIME_OUTPUT="$OUT" WASI_SDK_PATH="$WASI_SDK" \
    ISOPLANE_OPENSSL_REVISION="$(git -C "$OPENSSL" rev-parse HEAD)" \
    ISOPLANE_ICU_REVISION="$(git -C "$ICU" rev-parse HEAD)" \
    bash "$RUNTIME/eng/isoplane/build-runtime.sh" "$1"
}

stage_runtime() { runtime_stage runtime; }
stage_feed() { runtime_stage feed; }

stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain openssl icu runtime feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
