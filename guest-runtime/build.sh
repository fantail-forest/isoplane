#!/usr/bin/env bash
# Build the Isoplane guest runtime from pinned sources: guest-runtime/build.sh [stage...]
#
# Stages, in order (default: all): acquire toolchain patch openssl icu packs feed
#   acquire    download and verify the archives in manifest.env
#   toolchain  unpack the pinned wasi-sdk under work/wasi-sdk
#   patch      reconcile pinned source trees and patches, preserving local commits:
#              patches/runtime and patches/openssl are directories applied in
#              order; patches/runtimelab/series names the fork's own patches and
#              the runtime patches that apply to it, Mono files excluded
#   openssl    build OpenSSL to wasm32-wasip2 archives, installed under out/openssl
#   icu        build complete ICU data and stage each pinned WASI native transport
#   packs      the runtime and nativeaot stages concurrently (separate trees)
#   runtime    build the Mono WASI runtime pack (mono+libs+packs) for wasi-wasm
#   nativeaot  build the NativeAOT-LLVM framework pack (clr.aot+libs) for wasi-wasm
#   feed       write the packs into out/feed as a local NuGet source, unpacked under out/packs
#
# Everything under work/ and out/ is regenerable and git-ignored.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=manifest.env
source "$HERE/manifest.env"
WORK="$HERE/work"
OUT="$HERE/out"
RUNTIME="$WORK/runtime"
RUNTIMELAB="$WORK/runtimelab"
OPENSSL="$WORK/openssl"
OPENSSL_BUILD="$WORK/openssl-build"
WASI_SDK="$WORK/wasi-sdk"
ICU="$WORK/icu"
ICU_BUILD="$WORK/icu-build"
ICU_MONO_NATIVE="$OUT/icu/mono/runtimes/wasi-wasm/native"
ICU_NATIVEAOT_NATIVE="$OUT/icu/nativeaot/runtimes/wasi-wasm/native"
DOWNLOADS="$WORK/downloads"
LOGS="$WORK/logs"
mkdir -p "$DOWNLOADS" "$LOGS" "$OUT"
exec 9>"$WORK/.build.lock"
flock 9

fetch() {
  local url="$1" sha="$2" file="$3"
  if [ ! -f "$file" ] || [ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ]; then
    curl --silent --show-error --fail --location --retry 3 -o "$file" "$url"
  fi
  echo "$sha  $file" | sha256sum --check --quiet
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
  fetch "$RUNTIMELAB_TARBALL_URL" "$RUNTIMELAB_TARBALL_SHA256" "$DOWNLOADS/runtimelab-$RUNTIMELAB_COMMIT.tar.gz"
  fetch "$OPENSSL_TARBALL_URL" "$OPENSSL_TARBALL_SHA256" "$DOWNLOADS/$OPENSSL_TAG.tar.gz"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz"
  fetch "$ICU_SOURCE_URL" "$ICU_SOURCE_SHA256" "$DOWNLOADS/icu-$ICU_SOURCE_COMMIT.tar.gz"
  fetch "$ICU_MONO_TRANSPORT_URL" "$ICU_MONO_TRANSPORT_SHA256" "$DOWNLOADS/icu-mono-$ICU_MONO_TRANSPORT_VERSION.nupkg"
  fetch "$ICU_NATIVEAOT_TRANSPORT_URL" "$ICU_NATIVEAOT_TRANSPORT_SHA256" "$DOWNLOADS/icu-nativeaot-$ICU_NATIVEAOT_TRANSPORT_VERSION.nupkg"
}

stage_toolchain() {
  echo "==> toolchain"
  # The marker is what the runtime build checks before provisioning its own copy.
  if [ -f "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION" ] && [ -f "$WASI_SDK/.isoplane-archive-sha256" ] && [ "$(cat "$WASI_SDK/.isoplane-archive-sha256")" = "$WASI_SDK_SHA256" ]; then
    echo "    wasi-sdk $WASI_SDK_VERSION already unpacked"
    return
  fi
  verify_archive "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz"
  rm -rf "$WASI_SDK"
  mkdir -p "$WASI_SDK"
  tar --strip-components=1 -xzf "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz" -C "$WASI_SDK"
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
  source_tree "$1" "$RUNTIME" "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz" "$RUNTIME_TARBALL_SHA256" "$HERE/patches/runtime"
}
nativeaot_source() {
  source_tree "$1" "$RUNTIMELAB" "$DOWNLOADS/runtimelab-$RUNTIMELAB_COMMIT.tar.gz" "$RUNTIMELAB_TARBALL_SHA256" "$HERE/patches/runtimelab" --exclude='src/mono/*'
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
  nativeaot_source materialize
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
    if [ ! -f "$OUT/openssl/.isoplane-build-identity" ] || [ "$(cat "$OUT/openssl/.isoplane-build-identity")" != "$(openssl_inputs_digest)" ]; then
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
  local nativeaot_archive="$DOWNLOADS/icu-nativeaot-$ICU_NATIVEAOT_TRANSPORT_VERSION.nupkg"
  verify_archive "$ICU_MONO_TRANSPORT_SHA256" "$mono_archive"
  verify_archive "$ICU_NATIVEAOT_TRANSPORT_SHA256" "$nativeaot_archive"
  (
    if [ ! -f "$WORK/icu-build-inputs" ] || [ "$(cat "$WORK/icu-build-inputs")" != "$build_inputs" ]; then
      rm -rf "$ICU_BUILD"
    fi
    mkdir -p "$ICU_BUILD"
    cd "$ICU_BUILD"
    unset ICU_DATA_FILTER_FILE
    if [ ! -f Makefile ]; then
      "$ICU/icu/icu4c/source/runConfigureICU" Linux --disable-tests --disable-samples --disable-shared --enable-static --with-data-packaging=archive
    fi
    make -j"$(nproc)"
    [ -f "data/out/$ICU_DATA_FILE" ] || { echo "the ICU build produced no data/out/$ICU_DATA_FILE" >&2; exit 1; }
    verify_archive "$ICU_MONO_TRANSPORT_SHA256" "$mono_archive"
    verify_archive "$ICU_NATIVEAOT_TRANSPORT_SHA256" "$nativeaot_archive"
    rm -rf "$OUT/icu"
    mkdir -p "$OUT/icu/mono" "$OUT/icu/nativeaot"
    unzip -q "$mono_archive" 'runtimes/wasi-wasm/native/*' -d "$OUT/icu/mono"
    unzip -q "$nativeaot_archive" 'runtimes/wasi-wasm/native/*' -d "$OUT/icu/nativeaot"
    cp "data/out/$ICU_DATA_FILE" "$ICU_MONO_NATIVE/lib/icudt.dat"
    cp "data/out/$ICU_DATA_FILE" "$ICU_NATIVEAOT_NATIVE/lib/icudt.dat"
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
nativeaot_version_file="$WORK/nativeaot-pack-version"
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
    WASI_SDK_PATH="$WASI_SDK/" DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    ./build.sh -os wasi -subset mono+libs+packs -c Release -bl \
      /nodeReuse:false /p:UseSharedCompilation=false \
      /p:OfficialBuild=true "/p:OfficialBuildId=$PACK_BUILD_ID" \
      "/p:PatchVersion=$PACK_PATCH" "/p:PreReleaseVersionLabel=$PACK_VERSION_LABEL" \
      "/p:PreReleaseVersionIteration=$PACK_ITERATION" /p:WasmNativeStrip=false \
      /p:EnableSourceLink=false \
      "/p:WasiOpenSslLibDir=$OUT/openssl/lib" \
      "/p:_IcuDir=$ICU_MONO_NATIVE" "/p:ICULibDir=$ICU_MONO_NATIVE/lib" \
      -cmakeargs "-DOPENSSL_ROOT_DIR=$OUT/openssl -DOPENSSL_USE_STATIC_LIBS=TRUE"
  ) > "$LOGS/runtime-build.log" 2>&1
  complete_pack_build "$build_inputs" "$WORK/runtime-build-inputs" runtime_source
  local shipping="$RUNTIME/artifacts/packages/Release/Shipping" version
  version="$(cat "$runtime_version_file")"
  sha256sum "$runtime_version_file" "$shipping/Microsoft.NETCore.App.Runtime.Mono.wasi-wasm.$version.nupkg" "$shipping/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk.$version.nupkg" > "$WORK/runtime-pack-artifacts"
}

# The NativeAOT framework: the fork's CoreLib and libraries under the same
# patch series, the native shims, the OpenSSL backend and the AOT runtime
# archives, packaged as the target pack the pinned ILCompiler.LLVM compiles
# against.
stage_nativeaot() {
  echo "==> nativeaot (log: $LOGS/nativeaot-build.log)"
  require_build_inputs
  nativeaot_source verify
  local build_inputs="$(inputs_digest)"
  audit_pinvokes "$RUNTIMELAB"
  prepare_pack_outputs "$RUNTIMELAB" "$WORK/nativeaot-build-inputs"
  stamp_pack_version "$nativeaot_version_file"
  (
    cd "$RUNTIMELAB"
    WASI_SDK_PATH="$WASI_SDK/" DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    ./build.sh -os wasi -arch wasm -subset clr.aot+libs+nativeaot.packages -c Release -bl \
      "/p:_IcuDir=$ICU_NATIVEAOT_NATIVE" \
      /nodeReuse:false /p:UseSharedCompilation=false \
      /p:OfficialBuild=true "/p:OfficialBuildId=$PACK_BUILD_ID" \
      "/p:PatchVersion=$PACK_PATCH" "/p:PreReleaseVersionLabel=$PACK_VERSION_LABEL" \
      "/p:PreReleaseVersionIteration=$PACK_ITERATION" \
      /p:EnableSourceLink=false \
      "/p:WasiOpenSslLibDir=$OUT/openssl/lib" \
      -cmakeargs "-DOPENSSL_ROOT_DIR=$OUT/openssl -DOPENSSL_USE_STATIC_LIBS=TRUE"
  ) > "$LOGS/nativeaot-build.log" 2>&1
  complete_pack_build "$build_inputs" "$WORK/nativeaot-build-inputs" nativeaot_source
  local version
  version="$(cat "$nativeaot_version_file")"
  sha256sum "$nativeaot_version_file" "$RUNTIMELAB/artifacts/packages/Release/Shipping/runtime.wasi-wasm.Microsoft.DotNet.ILCompiler.LLVM.$version.nupkg" "$RUNTIMELAB/artifacts/packages/Release/Shipping/Microsoft.DotNet.ILCompiler.LLVM.$version.nupkg" > "$WORK/nativeaot-pack-artifacts"
}

# Both packs at once: the two trees, logs and version files are disjoint, and
# the builds share only read-only inputs (OpenSSL, the wasi-sdk, the NuGet
# cache). A failure in either fails the stage after both have stopped.
stage_packs() {
  local runtime_status=0 nativeaot_status=0
  stage_runtime & local runtime_job=$!
  stage_nativeaot & local nativeaot_job=$!
  wait "$runtime_job" || runtime_status=$?
  wait "$nativeaot_job" || nativeaot_status=$?
  [ "$runtime_status" -eq 0 ] || { echo "the runtime stage failed ($runtime_status); see $LOGS/runtime-build.log" >&2; }
  [ "$nativeaot_status" -eq 0 ] || { echo "the nativeaot stage failed ($nativeaot_status); see $LOGS/nativeaot-build.log" >&2; }
  [ "$runtime_status" -eq 0 ] && [ "$nativeaot_status" -eq 0 ]
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
  nativeaot_source verify
  require_build_inputs
  for receipt in "$WORK/runtime-build-inputs" "$WORK/nativeaot-build-inputs"; do
    [ -f "$receipt" ] && [ "$(cat "$receipt")" = "$(inputs_digest)" ] || { echo "the runtime packs do not match current build inputs; run guest-runtime/build.sh packs" >&2; exit 1; }
  done
  for receipt in "$WORK/runtime-pack-artifacts" "$WORK/nativeaot-pack-artifacts"; do
    [ -f "$receipt" ] && sha256sum --check --status "$receipt" || { echo "runtime package bytes or version identity changed after compilation; rebuild packs" >&2; exit 1; }
  done
  local feed="$OUT/feed"
  mkdir -p "$feed"
  local version nativeaot_version
  version="$(recorded_version "$runtime_version_file" runtime)"
  nativeaot_version="$(recorded_version "$nativeaot_version_file" nativeaot)"
  local shipping="$RUNTIME/artifacts/packages/Release/Shipping"
  local pack="$shipping/Microsoft.NETCore.App.Runtime.Mono.wasi-wasm.$version.nupkg"
  [ -f "$pack" ] || { echo "the runtime stage produced no $pack:" >&2; ls "$shipping" >&2; exit 1; }
  local sdkpack="$shipping/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk.$version.nupkg"
  [ -f "$sdkpack" ] || { echo "the runtime stage produced no Wasi.Sdk pack at $sdkpack" >&2; exit 1; }
  local nativeaot_shipping="$RUNTIMELAB/artifacts/packages/Release/Shipping"
  local nativeaot_pack="$nativeaot_shipping/runtime.wasi-wasm.Microsoft.DotNet.ILCompiler.LLVM.$nativeaot_version.nupkg"
  [ -f "$nativeaot_pack" ] || { echo "the nativeaot stage produced no $nativeaot_pack:" >&2; ls "$nativeaot_shipping" >&2; exit 1; }
  local nativeaot_sdk="$nativeaot_shipping/Microsoft.DotNet.ILCompiler.LLVM.$nativeaot_version.nupkg"
  [ -f "$nativeaot_sdk" ] || { echo "the nativeaot stage produced no common targets pack at $nativeaot_sdk" >&2; exit 1; }
  rm -f "$feed"/*.nupkg
  cp "$pack" "$sdkpack" "$nativeaot_pack" "$nativeaot_sdk" "$feed/"
  rm -rf "$OUT/packs"
  unpack "$pack" "$OUT/packs/runtime"
  unpack "$sdkpack" "$OUT/packs/wasi-sdk"
  unpack "$nativeaot_pack" "$OUT/packs/nativeaot"
  unpack "$nativeaot_sdk" "$OUT/packs/nativeaot-sdk"
  # The SDK's profiles import this to select their packs by version and to
  # check the pack a build resolves against the feed's digest.
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
    <IsoplaneGuestNativeAotSdkPackVersion>$nativeaot_version</IsoplaneGuestNativeAotSdkPackVersion>
    <IsoplaneGuestNativeAotSdkPackSha512>$(pack_digest "$nativeaot_sdk")</IsoplaneGuestNativeAotSdkPackSha512>
    <IsoplaneGuestNativeAotSdkPackDirectory>\$(MSBuildThisFileDirectory)../packs/nativeaot-sdk/</IsoplaneGuestNativeAotSdkPackDirectory>
    <IsoplaneGuestNativeAotPackVersion>$nativeaot_version</IsoplaneGuestNativeAotPackVersion>
    <IsoplaneGuestNativeAotPackSha512>$(pack_digest "$nativeaot_pack")</IsoplaneGuestNativeAotPackSha512>
    <IsoplaneGuestNativeAotPackDirectory>\$(MSBuildThisFileDirectory)../packs/nativeaot/</IsoplaneGuestNativeAotPackDirectory>
  </PropertyGroup>
</Project>
EOF
  echo "    $(basename "$pack")"
  echo "    $(basename "$sdkpack")"
  echo "    $(basename "$nativeaot_pack")"
}

stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain patch openssl icu packs feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
