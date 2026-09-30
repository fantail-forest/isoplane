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
  if [ "$#" = 0 ]; then exec task --dir "$HERE/.." runtime; fi
  tasks=()
  for stage in "$@"; do tasks+=("runtime:$stage"); done
  exec task --dir "$HERE/.." "${tasks[@]}"
fi
shift
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
OUT="$HERE/../.build/wasm32/runtime"
RUNTIME="$HERE/../vendor/dotnet-runtime"
OPENSSL="$HERE/../vendor/openssl"
OPENSSL_BUILD="$WORK/openssl-build"
WASI_SDK="$WORK/wasi-sdk"
ICU="$HERE/../vendor/icu"
ICU_BUILD="$WORK/icu-build"
ICU_MONO_NATIVE="$OUT/icu/mono/runtimes/wasi-wasm/native"
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

require_ninja() {
  local ninja
  ninja="$(command -v ninja || command -v ninja-build)" || { echo "Ninja is missing from PATH; install a working Ninja before building runtime packs" >&2; return 1; }
  "$ninja" --version >/dev/null 2>&1 || { echo "Ninja cannot run at $ninja; select a working Ninja on PATH" >&2; return 1; }
}

stage_acquire() {
  echo "==> acquire"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
  fetch "$ICU_MONO_TRANSPORT_URL" "$ICU_MONO_TRANSPORT_SHA256" "$DOWNLOADS/icu-mono-$ICU_MONO_TRANSPORT_VERSION.nupkg"
}

stage_toolchain() {
  echo "==> toolchain"
  # The marker is what the runtime build checks before provisioning its own copy.
  if [ -f "$WASI_SDK/VERSION" ] && [ "$(head -n 1 "$WASI_SDK/VERSION" | tr -d '\r')" = "$WASI_SDK_VERSION" ]; then
    echo "    wasi-sdk $WASI_SDK_VERSION already unpacked"
    return
  fi
  verify_archive "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
  rm -rf "$WASI_SDK"
  mkdir -p "$WASI_SDK"
  tar --strip-components=1 -xzf - -C "$WASI_SDK" < "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-$BUILD_HOST.tar.gz"
  echo "$WASI_SDK_VERSION" > "$WASI_SDK/wasi-sdk-version.txt"
  touch "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION"
}

stage_openssl() {
  echo "==> openssl (log: $LOGS/openssl-build.log)"
  [ -x "$WASI_SDK/bin/clang" ] || { echo "the toolchain stage unpacks the pinned wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
  # OPENSSLDIR is a guest path: the compiled-in default certificate file and
  # directory are then /etc/ssl/cert.pem and /etc/ssl/certs inside the
  # guest, not the builder's output directory.
  mkdir -p "$OPENSSL_BUILD"
  (
    cd "$OPENSSL_BUILD"
      CC="$WASI_SDK/bin/clang" AR="$WASI_SDK/bin/llvm-ar" RANLIB="$WASI_SDK/bin/llvm-ranlib" \
      perl "$OPENSSL/Configure" linux-generic32 no-shared no-threads no-asm no-dso no-module no-engine \
        no-sock no-async no-apps no-tests no-secure-memory no-ui-console \
        --with-rand-seed=getrandom --prefix="$OUT/openssl" --openssldir=/etc/ssl
    make -j"$(nproc)" build_libs
    # Headers and archives under out/openssl, where the runtime build finds them.
    make install_dev
  ) > "$LOGS/openssl-build.log" 2>&1
}

stage_icu() {
  echo "==> icu (log: $LOGS/icu-build.log)"
  local mono_archive="$DOWNLOADS/icu-mono-$ICU_MONO_TRANSPORT_VERSION.nupkg"
  verify_archive "$ICU_MONO_TRANSPORT_SHA256" "$mono_archive"
  (
    mkdir -p "$ICU_BUILD"
    cd "$ICU_BUILD"
    unset ICU_DATA_FILTER_FILE
    configure_environment=()
    if [ "$BUILD_HOST" = windows ]; then
      configure_environment+=("CXXFLAGS=${CXXFLAGS:--O2} -std=gnu++11")
    fi
    env "${configure_environment[@]}" "$ICU/icu/icu4c/source/runConfigureICU" "$ICU_HOST" --disable-tests --disable-samples --disable-shared --enable-static --with-data-packaging=archive
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
  ) > "$LOGS/icu-build.log" 2>&1
}

runtime_version_file="$WORK/pack-version"
stamp_pack_version() {
  PACK_VERSION_SUFFIX="$PACK_LABEL-${ISOPLANE_RUNTIME_INPUTS:?Task must supply the runtime source checksum}.$PACK_ITERATION"
  # Arcade needs a calendar build number for assembly/file versions.
  PACK_BUILD_ID="$(git -C "$RUNTIME" log -1 --format=%cs | tr -d '-').1"
  printf '%s\n' "10.0.$PACK_PATCH-$PACK_VERSION_SUFFIX" > "$1"
  echo "    pack version $(cat "$1")"
}
require_build_inputs() {
  require_ninja
  [ -f "$OUT/openssl/lib/libcrypto.a" ]
  [ -f "$ICU_MONO_NATIVE/lib/icudt.dat" ]
  [ -x "$WASI_SDK/bin/clang" ]
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
  stamp_pack_version "$runtime_version_file"
  # Official versioning keeps the upstream assembly versions instead of dev versions.
  (
    mkdir -p "$WORK/runtime-artifacts"
    if [ ! -e "$RUNTIME/artifacts" ]; then
      ln -s "$(realpath --relative-to="$RUNTIME" "$WORK/runtime-artifacts")" "$RUNTIME/artifacts"
    fi
    cd "$RUNTIME"
    DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    build_runtime -os wasi -subset mono+libs+packs -c Release -bl \
      /nodeReuse:false /p:UseSharedCompilation=false \
      /p:OfficialBuild=true "/p:OfficialBuildId=$PACK_BUILD_ID" \
      "/p:PatchVersion=$PACK_PATCH" "/p:VersionSuffix=$PACK_VERSION_SUFFIX" \
      /p:WasmNativeStrip=false \
      /p:EnableSourceLink=false \
      "/p:WasiOpenSslLibDir=$OUT/openssl/lib" \
      "/p:_IcuDir=$ICU_MONO_NATIVE" "/p:ICULibDir=$ICU_MONO_NATIVE/lib" \
      -cmakeargs "$(openssl_cmake_args)"
  ) > "$LOGS/runtime-build.log" 2>&1
  local shipping="$RUNTIME/artifacts/packages/Release/Shipping" version
  version="$(cat "$runtime_version_file")"
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
  require_build_inputs
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
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain openssl icu runtime feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
