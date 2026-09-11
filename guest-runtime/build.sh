#!/usr/bin/env bash
# Build the WISP guest runtime from pinned sources: guest-runtime/build.sh [stage...]
#
# Stages, in order (default: all): acquire toolchain patch openssl packs feed
#   acquire    download and verify the archives in manifest.env, extract the source
#              trees under work/ and commit them pristine so every change is a diff
#   toolchain  unpack the pinned wasi-sdk under work/wasi-sdk
#   patch      apply the patch series to each source tree, one commit each:
#              patches/runtime and patches/openssl are directories applied in
#              order; patches/runtimelab/series names the fork's own patches and
#              the runtime patches that apply to it, Mono files excluded
#   openssl    build OpenSSL to wasm32-wasip2 archives, installed under out/openssl
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
DOWNLOADS="$WORK/downloads"
LOGS="$WORK/logs"
mkdir -p "$DOWNLOADS" "$LOGS" "$OUT"

fetch() {
  local url="$1" sha="$2" file="$3"
  if [ ! -f "$file" ] || [ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ]; then
    curl --silent --show-error --fail --location --retry 3 -o "$file" "$url"
  fi
  echo "$sha  $file" | sha256sum --check --quiet
}

extract_pristine() {
  local tarball="$1" target="$2"
  if [ -d "$target/.git" ]; then
    echo "    $target already extracted"
    return
  fi
  rm -rf "$target"
  mkdir -p "$target"
  tar --strip-components=1 -xzf "$tarball" -C "$target"
  git -C "$target" init -q
  # The applied-patch list is build state, never part of a patch.
  echo ".wisp-applied" >> "$target/.git/info/exclude"
  git -C "$target" -c user.name=wisp -c user.email=wisp@localhost add -A
  git -C "$target" -c user.name=wisp -c user.email=wisp@localhost commit -q -m "pristine $(basename "$tarball")"
}

stage_acquire() {
  echo "==> acquire"
  fetch "$RUNTIME_TARBALL_URL" "$RUNTIME_TARBALL_SHA256" "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz"
  fetch "$RUNTIMELAB_TARBALL_URL" "$RUNTIMELAB_TARBALL_SHA256" "$DOWNLOADS/runtimelab-$RUNTIMELAB_COMMIT.tar.gz"
  fetch "$OPENSSL_TARBALL_URL" "$OPENSSL_TARBALL_SHA256" "$DOWNLOADS/$OPENSSL_TAG.tar.gz"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz"
  extract_pristine "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz" "$RUNTIME"
  extract_pristine "$DOWNLOADS/runtimelab-$RUNTIMELAB_COMMIT.tar.gz" "$RUNTIMELAB"
  extract_pristine "$DOWNLOADS/$OPENSSL_TAG.tar.gz" "$OPENSSL"
}

stage_toolchain() {
  echo "==> toolchain"
  # The marker is what the runtime build checks before provisioning its own copy.
  if [ -f "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION" ]; then
    echo "    wasi-sdk $WASI_SDK_VERSION already unpacked"
    return
  fi
  rm -rf "$WASI_SDK"
  mkdir -p "$WASI_SDK"
  tar --strip-components=1 -xzf "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz" -C "$WASI_SDK"
  cp "$RUNTIME/src/mono/wasi/wasi-sdk-version.txt" "$WASI_SDK/wasi-sdk-version.txt" 2>/dev/null || echo "$WASI_SDK_VERSION" > "$WASI_SDK/wasi-sdk-version.txt"
  touch "$WASI_SDK/WASI-SDK-VERSION-$WASI_SDK_VERSION"
}

# The patches of a directory in name order, or the entries of its series
# file (one path per line, relative to the directory, `#` comments).
patch_series() {
  local directory="$1"
  if [ -f "$directory/series" ]; then
    grep -v '^\s*\(#\|$\)' "$directory/series" | sed "s|^|$directory/|"
  else
    ls "$directory"/*.patch 2>/dev/null || true
  fi
}

# apply_patches <tree> <patch directory> [git apply option...]
apply_patches() {
  local tree="$1" directory="$2"
  shift 2
  local applied="$tree/.wisp-applied"
  touch "$applied"
  local patch name
  while IFS= read -r patch; do
    [ -n "$patch" ] || continue
    name="$(basename "$patch")"
    if grep -qx "$name" "$applied"; then
      continue
    fi
    echo "    applying $name"
    git -C "$tree" apply --index "$@" "$patch"
    git -C "$tree" -c user.name=wisp -c user.email=wisp@localhost commit -q -m "$name"
    echo "$name" >> "$applied"
  done < <(patch_series "$directory")
}

stage_patch() {
  echo "==> patch"
  apply_patches "$RUNTIME" "$HERE/patches/runtime"
  # The fork has no Mono; a shared patch's Mono hunks do not apply to it.
  apply_patches "$RUNTIMELAB" "$HERE/patches/runtimelab" --exclude='src/mono/*'
  apply_patches "$OPENSSL" "$HERE/patches/openssl"
  # The runtime tree's own copy of the version file feeds the toolchain marker.
  cp "$RUNTIME/src/mono/wasi/wasi-sdk-version.txt" "$WASI_SDK/wasi-sdk-version.txt" 2>/dev/null || true
}

stage_openssl() {
  echo "==> openssl (log: $LOGS/openssl-build.log)"
  [ -x "$WASI_SDK/bin/clang" ] || { echo "the toolchain stage unpacks wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
  # Out of tree, so the source tree stays a clean base for patches. The
  # OPENSSLDIR is a guest path: the compiled-in default certificate file and
  # directory are then /etc/ssl/cert.pem and /etc/ssl/certs inside the
  # guest, not the builder's output directory.
  mkdir -p "$OPENSSL_BUILD"
  (
    cd "$OPENSSL_BUILD"
    if [ ! -f Makefile ]; then
      CC="$WASI_SDK/bin/clang" AR="$WASI_SDK/bin/llvm-ar" RANLIB="$WASI_SDK/bin/llvm-ranlib" \
      perl "$OPENSSL/Configure" linux-generic32 no-shared no-threads no-asm no-dso no-module no-engine \
        no-sock no-async no-apps no-tests no-secure-memory no-ui-console \
        --with-rand-seed=getrandom --prefix="$OUT/openssl" --openssldir=/etc/ssl
    fi
    make -j"$(nproc)" build_libs
    # Headers and archives under out/openssl, where the runtime build finds them.
    make install_dev
  ) > "$LOGS/openssl-build.log" 2>&1
}

# A pack version is never reused: its label carries a digest of the inputs
# (the manifest and every patch) and the second the build started, so two
# builds never publish different bytes under one version, and a shared NuGet
# cache never shadows a rebuilt pack. Arcade allows the build id only a
# two-digit revision, so the time lives in the label. The exact version is
# recorded under work/ for the feed stage, one file per pack.
inputs_digest() {
  find "$HERE/patches" -type f | LC_ALL=C sort | xargs cat "$HERE/manifest.env" | sha256sum | cut -c1-8
}
runtime_version_file="$WORK/pack-version"
nativeaot_version_file="$WORK/nativeaot-pack-version"
# stamp_pack_version <version file>
stamp_pack_version() {
  local build_id label stamp
  build_id="$(date -u +%Y%m%d).1"
  label="$PACK_LABEL-$(inputs_digest)-$(date -u +%H%M%S)"
  # Arcade's date stamp: yy * 1000 + mm * 50 + dd.
  stamp=$((10#$(date -u +%y) * 1000 + 10#$(date -u +%m) * 50 + 10#$(date -u +%d)))
  echo "10.0.$PACK_PATCH-$label.$PACK_ITERATION.$stamp.1" > "$1"
  PACK_BUILD_ID="$build_id"
  PACK_VERSION_LABEL="$label"
  echo "    pack version $(cat "$1")"
}
require_build_inputs() {
  [ -f "$OUT/openssl/lib/libcrypto.a" ] || { echo "the openssl stage installs OpenSSL under $OUT/openssl; run it first" >&2; exit 1; }
  [ -x "$WASI_SDK/bin/clang" ] || { echo "the toolchain stage unpacks wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
}
# A managed P/Invoke whose wasm signature disagrees with its C prototype
# traps on the first call; the audit refuses to build such a pack.
audit_pinvokes() {
  python3 "$HERE/audit-pinvokes.py" "$1"
}
stage_runtime() {
  echo "==> runtime (log: $LOGS/runtime-build.log)"
  require_build_inputs
  audit_pinvokes "$RUNTIME"
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
      -cmakeargs "-DOPENSSL_ROOT_DIR=$OUT/openssl -DOPENSSL_USE_STATIC_LIBS=TRUE"
  ) > "$LOGS/runtime-build.log" 2>&1
}

# The NativeAOT framework: the fork's CoreLib and libraries under the same
# patch series, the native shims, the OpenSSL backend and the AOT runtime
# archives, packaged as the target pack the pinned ILCompiler.LLVM compiles
# against.
stage_nativeaot() {
  echo "==> nativeaot (log: $LOGS/nativeaot-build.log)"
  require_build_inputs
  audit_pinvokes "$RUNTIMELAB"
  stamp_pack_version "$nativeaot_version_file"
  (
    cd "$RUNTIMELAB"
    WASI_SDK_PATH="$WASI_SDK/" DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    ./build.sh -os wasi -arch wasm -subset clr.aot+libs+nativeaot.packages -c Release -bl \
      /nodeReuse:false /p:UseSharedCompilation=false \
      /p:OfficialBuild=true "/p:OfficialBuildId=$PACK_BUILD_ID" \
      "/p:PatchVersion=$PACK_PATCH" "/p:PreReleaseVersionLabel=$PACK_VERSION_LABEL" \
      "/p:PreReleaseVersionIteration=$PACK_ITERATION" \
      /p:EnableSourceLink=false \
      "/p:WasiOpenSslLibDir=$OUT/openssl/lib" \
      -cmakeargs "-DOPENSSL_ROOT_DIR=$OUT/openssl -DOPENSSL_USE_STATIC_LIBS=TRUE"
  ) > "$LOGS/nativeaot-build.log" 2>&1
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
  rm -f "$feed"/*.nupkg
  cp "$pack" "$sdkpack" "$nativeaot_pack" "$feed/"
  rm -rf "$OUT/packs"
  unpack "$pack" "$OUT/packs/runtime"
  unpack "$sdkpack" "$OUT/packs/wasi-sdk"
  unpack "$nativeaot_pack" "$OUT/packs/nativeaot"
  # The SDK's profiles import this to select their packs by version and to
  # check the pack a build resolves against the feed's digest.
  cat > "$feed/Wisp.GuestRuntime.props" <<EOF
<Project>
  <PropertyGroup>
    <WispGuestRuntimePackVersion>$version</WispGuestRuntimePackVersion>
    <WispGuestRuntimePackSha512>$(pack_digest "$pack")</WispGuestRuntimePackSha512>
    <WispGuestRuntimePackDirectory>$OUT/packs/runtime/</WispGuestRuntimePackDirectory>
    <WispGuestWasiSdkPackDirectory>$OUT/packs/wasi-sdk/</WispGuestWasiSdkPackDirectory>
    <WispGuestNativeAotPackVersion>$nativeaot_version</WispGuestNativeAotPackVersion>
    <WispGuestNativeAotPackSha512>$(pack_digest "$nativeaot_pack")</WispGuestNativeAotPackSha512>
    <WispGuestNativeAotPackDirectory>$OUT/packs/nativeaot/</WispGuestNativeAotPackDirectory>
  </PropertyGroup>
</Project>
EOF
  echo "    $(basename "$pack")"
  echo "    $(basename "$sdkpack")"
  echo "    $(basename "$nativeaot_pack")"
}

stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain patch openssl packs feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
