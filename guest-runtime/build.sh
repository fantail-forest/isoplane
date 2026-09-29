#!/usr/bin/env bash
# Build the Isoplane guest runtime from pinned sources: guest-runtime/build.sh [stage...]
#
# Stages, in order (default: all): acquire toolchain patch openssl icu runtime feed
# Everything under work/ and out/ is regenerable and git-ignored.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if command -v cygpath >/dev/null 2>&1; then
  HERE="$(cygpath -m "$HERE")"
fi
RUNTIME_SOURCE="${ISOPLANE_RUNTIME_SOURCE:-}"
if [ "${RUNTIME_SOURCE//\\//}" != "$HERE" ]; then
  exec python3 "$HERE/../scripts/build_storage.py" runtime "$HERE/.." bash "$HERE/build.sh" "$@"
fi
# shellcheck source=manifest.env
source "$HERE/manifest.env"
BUILD_HOST=linux
ICU_HOST=Linux
case "$(uname -s)" in
  MINGW*|MSYS*)
    BUILD_HOST=windows
    ICU_HOST=MinGW
    WASI_SDK_URL="$WASI_SDK_WINDOWS_URL"
    WASI_SDK_SHA256="$WASI_SDK_WINDOWS_SHA256"
    ;;
esac
WORK="${ISOPLANE_RUNTIME_WORK//\\//}"
echo "Runtime build work: $WORK"
OUT="$HERE/out"
RUNTIME="$WORK/runtime"
OPENSSL="$WORK/openssl"
OPENSSL_BUILD="$WORK/openssl-build"
WASI_SDK="$WORK/wasi-sdk"
ICU="$WORK/icu"
ICU_BUILD="$WORK/icu-build"
ICU_MONO_NATIVE="$OUT/icu/mono/runtimes/wasi-wasm/native"
DOWNLOADS="$WORK/downloads"
LOGS="$WORK/logs"
mkdir -p "$DOWNLOADS" "$LOGS" "$OUT"

fetch() {
  python3 "$HERE/../scripts/build_storage.py" "$1" "$3" "$2"
}

verify_archive() {
  local sha="$1" file="$2"
  printf '%s  %s\n' "$sha" "$file" | sha256sum --check --status || {
    echo "archive does not match its pinned SHA256: $file; run guest-runtime/build.sh acquire" >&2
    return 1
  }
}

require_ninja() {
  local ninja
  ninja="$(command -v ninja || command -v ninja-build)" || { echo "Ninja is missing from PATH; install a working Ninja before building runtime packs" >&2; return 1; }
  "$ninja" --version >/dev/null 2>&1 || { echo "Ninja cannot run at $ninja; select a working Ninja on PATH" >&2; return 1; }
}

stage_acquire() {
  echo "==> acquire"
  fetch "$RUNTIME_TARBALL_URL" "$RUNTIME_TARBALL_SHA256" "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz"
  fetch "$OPENSSL_TARBALL_URL" "$OPENSSL_TARBALL_SHA256" "$DOWNLOADS/$OPENSSL_TAG.tar.gz"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
  fetch "$ICU_SOURCE_URL" "$ICU_SOURCE_SHA256" "$DOWNLOADS/icu-$ICU_SOURCE_COMMIT.tar.gz"
  fetch "$ICU_MONO_TRANSPORT_URL" "$ICU_MONO_TRANSPORT_SHA256" "$DOWNLOADS/icu-mono-$ICU_MONO_TRANSPORT_VERSION.nupkg"
}

stage_toolchain() {
  echo "==> toolchain"
  # The marker is what the runtime build checks before provisioning its own copy.
  if [ -f "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION" ] && [ -f "$WASI_SDK/.isoplane-archive-sha256" ] && [ "$(cat "$WASI_SDK/.isoplane-archive-sha256")" = "$WASI_SDK_SHA256" ]; then
    echo "    wasi-sdk $WASI_SDK_VERSION already unpacked"
    return
  fi
  verify_archive "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
  rm -rf "$WASI_SDK"
  mkdir -p "$WASI_SDK"
  tar --strip-components=1 -xzf "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz" -C "$WASI_SDK"
  echo "$WASI_SDK_VERSION" > "$WASI_SDK/wasi-sdk-version.txt"
  touch "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION"
  echo "$WASI_SDK_SHA256" > "$WASI_SDK/.isoplane-archive-sha256"
}

source_tree() {
  local action="$1" tree="$2" archive="$3" sha="$4" patches="$5"
  shift 5
  python3 "$HERE/source-tree.py" "$action" "$tree" "$archive" "$sha" "$patches" "$@"
}

runtime_source() {
  source_tree "$1" "$RUNTIME" "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz" "$RUNTIME_TARBALL_SHA256" "$HERE/patches/runtime" --build-layout dotnet
}
openssl_source() {
  source_tree "$1" "$OPENSSL" "$DOWNLOADS/$OPENSSL_TAG.tar.gz" "$OPENSSL_TARBALL_SHA256" "$HERE/patches/openssl"
}

icu_source() {
  source_tree "$1" "$ICU" "$DOWNLOADS/icu-$ICU_SOURCE_COMMIT.tar.gz" "$ICU_SOURCE_SHA256" "$HERE/patches/icu"
}

stage_patch() {
  echo "==> patch"
  runtime_source materialize
  openssl_source materialize
  icu_source materialize
}

openssl_inputs_digest() {
  find "$HERE/patches/openssl" -type f | LC_ALL=C sort | xargs cat "$HERE/manifest.env" "$HERE/build.sh" "$HERE/source-tree.py" | sha256sum | cut -d' ' -f1
}

stage_openssl() {
  echo "==> openssl (log: $LOGS/openssl-build.log)"
  openssl_source verify
  local build_inputs="$(openssl_inputs_digest)"
  [ -x "$WASI_SDK/bin/clang" ] && [ -f "$WASI_SDK/.isoplane-archive-sha256" ] && [ "$(cat "$WASI_SDK/.isoplane-archive-sha256")" = "$WASI_SDK_SHA256" ] || { echo "the toolchain stage unpacks the pinned wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
  # Out of tree, so the source tree stays a clean base for patches. The
  # OPENSSLDIR is a guest path: the compiled-in default certificate file and
  # directory are then /etc/ssl/cert.pem and /etc/ssl/certs inside the
  # guest, not the builder's output directory.
  mkdir -p "$OPENSSL_BUILD"
  (
    cd "$OPENSSL_BUILD"
    if [ ! -f Makefile ] || [ ! -f "$OUT/openssl/.isoplane-build-identity" ] || [ "$(cat "$OUT/openssl/.isoplane-build-identity")" != "$(openssl_inputs_digest)" ]; then
      if [ -f Makefile ]; then make clean; fi
      CC="$WASI_SDK/bin/clang" AR="$WASI_SDK/bin/llvm-ar" RANLIB="$WASI_SDK/bin/llvm-ranlib" \
      perl "$OPENSSL/Configure" linux-generic32 no-shared no-threads no-asm no-dso no-module no-engine \
        no-sock no-async no-apps no-tests no-secure-memory no-ui-console \
        --with-rand-seed=getrandom --prefix="$OUT/openssl" --openssldir=/etc/ssl
    fi
    make -j"$(nproc)" build_libs
    # Headers and archives under out/openssl, where the runtime build finds them.
    make install_dev
    openssl_source verify
    [ "$build_inputs" = "$(openssl_inputs_digest)" ] || { echo "OpenSSL inputs changed during compilation; rerun the build" >&2; exit 1; }
    find "$OUT/openssl/include" "$OUT/openssl/lib" -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > "$WORK/openssl-build-artifacts"
    echo "$build_inputs" > "$OUT/openssl/.isoplane-build-identity"
  ) > "$LOGS/openssl-build.log" 2>&1
}

icu_inputs_digest() {
  find "$HERE/patches/icu" -type f | LC_ALL=C sort | xargs cat "$HERE/manifest.env" "$HERE/build.sh" "$HERE/source-tree.py" | sha256sum | cut -d' ' -f1
}

stage_icu() {
  echo "==> icu (log: $LOGS/icu-build.log)"
  icu_source verify
  local build_inputs="$(icu_inputs_digest)"
  local mono_archive="$DOWNLOADS/icu-mono-$ICU_MONO_TRANSPORT_VERSION.nupkg"
  verify_archive "$ICU_MONO_TRANSPORT_SHA256" "$mono_archive"
  (
    if [ ! -f "$WORK/icu-build-inputs" ] || [ "$(cat "$WORK/icu-build-inputs")" != "$build_inputs" ]; then
      rm -rf "$ICU_BUILD"
    fi
    mkdir -p "$ICU_BUILD"
    cd "$ICU_BUILD"
    unset ICU_DATA_FILTER_FILE
    if [ ! -f Makefile ]; then
      configure_environment=()
      if [ "$BUILD_HOST" = windows ]; then
        configure_environment+=("CXXFLAGS=${CXXFLAGS:--O2} -std=gnu++11")
      fi
      env "${configure_environment[@]}" "$ICU/icu/icu4c/source/runConfigureICU" "$ICU_HOST" --disable-tests --disable-samples --disable-shared --enable-static --with-data-packaging=archive
    fi
    make_arguments=()
    if [ "$BUILD_HOST" = windows ]; then
      # ICU's MinGW linker rules omit the static-library prefix.
      make_arguments+=(STATIC_PREFIX= STATIC_PREFIX_WHEN_USED=)
    fi
    make -j"$(nproc)" "${make_arguments[@]}"
    [ -f "data/out/$ICU_DATA_FILE" ] || { echo "the ICU build produced no data/out/$ICU_DATA_FILE" >&2; exit 1; }
    verify_archive "$ICU_MONO_TRANSPORT_SHA256" "$mono_archive"
    rm -rf "$OUT/icu"
    mkdir -p "$OUT/icu/mono"
    unzip -q "$mono_archive" 'runtimes/wasi-wasm/native/**' -d "$OUT/icu/mono"
    cp "data/out/$ICU_DATA_FILE" "$ICU_MONO_NATIVE/lib/icudt.dat"
    icu_source verify
    [ "$build_inputs" = "$(icu_inputs_digest)" ] || { echo "ICU inputs changed during compilation; rerun the build" >&2; exit 1; }
    find "$OUT/icu" -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > "$WORK/icu-build-artifacts"
    echo "$build_inputs" > "$WORK/icu-build-inputs"
  ) > "$LOGS/icu-build.log" 2>&1
}

# A pack version is never reused: its label carries a digest of the inputs
# (the manifest and every patch) and the second the build started, so two
# builds never publish different bytes under one version, and a shared NuGet
# cache never shadows a rebuilt pack. Arcade allows the build id only a
# two-digit revision, so the time lives in the label. The exact version is
# recorded under work/ for the feed stage, one file per pack.
inputs_digest() {
  python3 "$HERE/feed-identity.py" digest "$HERE"
}
runtime_version_file="$WORK/pack-version"
# stamp_pack_version <version file>
stamp_pack_version() {
  local build_id label stamp
  build_id="$(date -u +%Y%m%d).1"
  label="$PACK_LABEL-$(inputs_digest | cut -c1-8)-$(date -u +%H%M%S)"
  # Arcade's date stamp: yy * 1000 + mm * 50 + dd.
  stamp=$((10#$(date -u +%y) * 1000 + 10#$(date -u +%m) * 50 + 10#$(date -u +%d)))
  echo "10.0.$PACK_PATCH-$label.$PACK_ITERATION.$stamp.1" > "$1"
  PACK_BUILD_ID="$build_id"
  PACK_VERSION_LABEL="$label"
  echo "    pack version $(cat "$1")"
}
require_build_inputs() {
  require_ninja
  icu_source verify
  [ -f "$WORK/icu-build-inputs" ] && [ "$(cat "$WORK/icu-build-inputs")" = "$(icu_inputs_digest)" ] || { echo "the ICU data does not match current build inputs; run guest-runtime/build.sh icu" >&2; exit 1; }
  [ -f "$WORK/icu-build-artifacts" ] && sha256sum --check --status "$WORK/icu-build-artifacts" || { echo "staged ICU native libraries or data changed; run guest-runtime/build.sh icu" >&2; exit 1; }
  openssl_source verify
  [ -f "$OUT/openssl/.isoplane-build-identity" ] && [ "$(cat "$OUT/openssl/.isoplane-build-identity")" = "$(openssl_inputs_digest)" ] || { echo "the OpenSSL archives do not match current build inputs; run guest-runtime/build.sh openssl" >&2; exit 1; }
  [ -f "$WORK/openssl-build-artifacts" ] && sha256sum --check --status "$WORK/openssl-build-artifacts" || { echo "OpenSSL headers or archives changed; run guest-runtime/build.sh openssl" >&2; exit 1; }
  [ -f "$OUT/openssl/lib/libcrypto.a" ] || { echo "the openssl stage installs OpenSSL under $OUT/openssl; run it first" >&2; exit 1; }
  [ -x "$WASI_SDK/bin/clang" ] && [ -f "$WASI_SDK/.isoplane-archive-sha256" ] && [ "$(cat "$WASI_SDK/.isoplane-archive-sha256")" = "$WASI_SDK_SHA256" ] || { echo "the toolchain stage unpacks the pinned wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
}
# A managed P/Invoke whose wasm signature disagrees with its C prototype
# traps on the first call; the audit refuses to build such a pack.
audit_pinvokes() {
  python3 "$HERE/audit-pinvokes.py" "$1"
}
# Source timestamps cannot establish whether generated native/managed outputs
# match a different patch set or build configuration. Invalidate those outputs
# whenever their input receipt changes; package caches remain reusable.
prepare_pack_outputs() {
  local tree="$1" receipt="$2"
  if [ ! -f "$receipt" ] || [ "$(cat "$receipt")" != "$(inputs_digest)" ]; then
    rm -rf "$tree/artifacts"
    rm -f "$receipt"
  fi
}

complete_pack_build() {
  local expected="$1" receipt="$2" source_check="$3"
  "$source_check" verify
  [ "$expected" = "$(inputs_digest)" ] || { echo "runtime inputs changed during compilation; rerun the build" >&2; exit 1; }
  echo "$expected" > "$receipt"
}

openssl_cmake_args() {
  local root="$OUT/openssl" cache="$WORK/$(basename "$PWD")-openssl.cmake"
  if [ "$BUILD_HOST" = windows ]; then root="$(cygpath -m "$root")"; fi
  cat > "$cache" <<EOF
set(OPENSSL_ROOT_DIR "$root" CACHE PATH "" FORCE)
set(OPENSSL_INCLUDE_DIR "$root/include" CACHE PATH "" FORCE)
set(OPENSSL_CRYPTO_LIBRARY "$root/lib/libcrypto.a" CACHE FILEPATH "" FORCE)
set(OPENSSL_SSL_LIBRARY "$root/lib/libssl.a" CACHE FILEPATH "" FORCE)
set(OPENSSL_USE_STATIC_LIBS TRUE CACHE BOOL "" FORCE)
EOF
  printf '%s' "-C$cache"
}

build_runtime() {
  if [ "$BUILD_HOST" = windows ]; then
    local arguments=() argument
    while [ "$#" -gt 0 ]; do
      argument="$1"
      shift
      if [ "$argument" = -cmakeargs ]; then
        argument="-cmakeargs:$1"
        shift
      fi
      arguments+=("${argument//$HERE/$(cygpath -m "$HERE")}")
    done
    WASI_SDK_PATH="$(cygpath -m "$WASI_SDK")/" MSYS2_ARG_CONV_EXCL='*' \
      powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$PWD/eng/build.ps1")" "${arguments[@]}"
  else
    WASI_SDK_PATH="$WASI_SDK/" ./build.sh "$@"
  fi
}

stage_runtime() {
  echo "==> runtime (log: $LOGS/runtime-build.log)"
  require_build_inputs
  runtime_source verify
  local build_inputs="$(inputs_digest)"
  audit_pinvokes "$RUNTIME"
  prepare_pack_outputs "$RUNTIME" "$WORK/runtime-build-inputs"
  stamp_pack_version "$runtime_version_file"
  # An official build is the one Arcade versions from the label and build id
  # rather than stamping "dev" or "ci"; nothing else about it applies here.
  (
    cd "$RUNTIME"
    DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    build_runtime -os wasi -subset mono+libs+packs -c Release -bl \
      /nodeReuse:false /p:UseSharedCompilation=false \
      /p:OfficialBuild=true "/p:OfficialBuildId=$PACK_BUILD_ID" \
      "/p:PatchVersion=$PACK_PATCH" "/p:PreReleaseVersionLabel=$PACK_VERSION_LABEL" \
      "/p:PreReleaseVersionIteration=$PACK_ITERATION" /p:WasmNativeStrip=false \
      /p:EnableSourceLink=false \
      "/p:WasiOpenSslLibDir=$OUT/openssl/lib" \
      "/p:_IcuDir=$ICU_MONO_NATIVE" "/p:ICULibDir=$ICU_MONO_NATIVE/lib" \
      -cmakeargs "$(openssl_cmake_args)"
  ) > "$LOGS/runtime-build.log" 2>&1
  complete_pack_build "$build_inputs" "$WORK/runtime-build-inputs" runtime_source
  local shipping="$RUNTIME/artifacts/packages/Release/Shipping" version
  version="$(cat "$runtime_version_file")"
  sha256sum "$runtime_version_file" "$shipping/Microsoft.NETCore.App.Runtime.Mono.wasi-wasm.$version.nupkg" "$shipping/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk.$version.nupkg" > "$WORK/runtime-pack-artifacts"
}

# The version a build stage recorded, or the message naming that stage.
recorded_version() {
  local file="$1" stage="$2"
  [ -f "$file" ] || { echo "the $stage stage records its pack version at $file; run it first" >&2; exit 1; }
  cat "$file"
}
# The pack's identity is its version plus the digest NuGet records for the
# package, the form the SDK checks against the pack a build resolves.
pack_digest() {
  openssl dgst -sha512 -binary "$1" | base64 -w0
}
# Unpacked copies, for projects that import a pack explicitly instead of
# through the installed workload.
unpack() {
  mkdir -p "$2"
  unzip -q "$1" -d "$2"
}
stage_feed() {
  echo "==> feed"
  runtime_source verify
  require_build_inputs
  local inputs="$WORK/runtime-build-inputs" artifacts="$WORK/runtime-pack-artifacts"
  [ -f "$inputs" ] && [ "$(cat "$inputs")" = "$(inputs_digest)" ] || { echo "the runtime packs do not match current build inputs; run guest-runtime/build.sh runtime" >&2; exit 1; }
  [ -f "$artifacts" ] && sha256sum --check --status "$artifacts" || { echo "runtime package bytes or version identity changed after compilation; rebuild runtime" >&2; exit 1; }
  local feed="$OUT/feed"
  mkdir -p "$feed"
  local version
  version="$(recorded_version "$runtime_version_file" runtime)"
  local shipping="$RUNTIME/artifacts/packages/Release/Shipping"
  local pack="$shipping/Microsoft.NETCore.App.Runtime.Mono.wasi-wasm.$version.nupkg"
  [ -f "$pack" ] || { echo "the runtime stage produced no $pack:" >&2; ls "$shipping" >&2; exit 1; }
  local sdkpack="$shipping/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk.$version.nupkg"
  [ -f "$sdkpack" ] || { echo "the runtime stage produced no Wasi.Sdk pack at $sdkpack" >&2; exit 1; }
  rm -f "$feed"/*.nupkg
  cp "$pack" "$sdkpack" "$feed/"
  rm -rf "$OUT/packs"
  unpack "$pack" "$OUT/packs/runtime"
  unpack "$sdkpack" "$OUT/packs/wasi-sdk"
  # The platform runtime build selects its packs by version and checks
  # each resolved pack against the feed's digest.
  cat > "$feed/Isoplane.GuestRuntime.props" <<EOF
<Project>
  <PropertyGroup>
    <IsoplaneGuestRuntimeInputsSha256>$(inputs_digest)</IsoplaneGuestRuntimeInputsSha256>
    <IsoplaneGuestWasiSdkPackVersion>$version</IsoplaneGuestWasiSdkPackVersion>
    <IsoplaneGuestWasiSdkPackSha512>$(pack_digest "$sdkpack")</IsoplaneGuestWasiSdkPackSha512>
    <IsoplaneGuestRuntimePackVersion>$version</IsoplaneGuestRuntimePackVersion>
    <IsoplaneGuestRuntimePackSha512>$(pack_digest "$pack")</IsoplaneGuestRuntimePackSha512>
    <IsoplaneGuestRuntimePackDirectory>\$(MSBuildThisFileDirectory)../packs/runtime/</IsoplaneGuestRuntimePackDirectory>
    <IsoplaneGuestWasiSdkPackDirectory>\$(MSBuildThisFileDirectory)../packs/wasi-sdk/</IsoplaneGuestWasiSdkPackDirectory>
  </PropertyGroup>
</Project>
EOF
  echo "    $(basename "$pack")"
  echo "    $(basename "$sdkpack")"
}

stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain patch openssl icu runtime feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
