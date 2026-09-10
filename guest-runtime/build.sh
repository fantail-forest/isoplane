#!/usr/bin/env bash
# Build the WISP guest runtime from pinned sources: guest-runtime/build.sh [stage...]
#
# Stages, in order (default: all): acquire toolchain patch openssl runtime feed
#   acquire    download and verify the archives in manifest.env, extract the source
#              trees under work/ and commit them pristine so every change is a diff
#   toolchain  unpack the pinned wasi-sdk under work/wasi-sdk
#   patch      apply patches/runtime/*.patch and patches/openssl/*.patch, one commit each
#   openssl    build OpenSSL to wasm32-wasip2 archives, installed under out/openssl
#   runtime    build the Mono WASI runtime pack (mono+libs+packs) for wasi-wasm
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
  fetch "$OPENSSL_TARBALL_URL" "$OPENSSL_TARBALL_SHA256" "$DOWNLOADS/$OPENSSL_TAG.tar.gz"
  fetch "$WASI_SDK_URL" "$WASI_SDK_SHA256" "$DOWNLOADS/wasi-sdk-$WASI_SDK_VERSION-x86_64-linux.tar.gz"
  extract_pristine "$DOWNLOADS/runtime-$RUNTIME_TAG.tar.gz" "$RUNTIME"
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

apply_patches() {
  local tree="$1" directory="$2"
  local applied="$tree/.wisp-applied"
  touch "$applied"
  for patch in "$directory"/*.patch; do
    [ -e "$patch" ] || continue
    local name; name="$(basename "$patch")"
    if grep -qx "$name" "$applied"; then
      continue
    fi
    echo "    applying $name"
    git -C "$tree" apply --index "$patch"
    git -C "$tree" -c user.name=wisp -c user.email=wisp@localhost commit -q -m "$name"
    echo "$name" >> "$applied"
  done
}

stage_patch() {
  echo "==> patch"
  apply_patches "$RUNTIME" "$HERE/patches/runtime"
  apply_patches "$OPENSSL" "$HERE/patches/openssl"
  # The runtime tree's own copy of the version file feeds the toolchain marker.
  cp "$RUNTIME/src/mono/wasi/wasi-sdk-version.txt" "$WASI_SDK/wasi-sdk-version.txt" 2>/dev/null || true
}

stage_openssl() {
  echo "==> openssl (log: $LOGS/openssl-build.log)"
  [ -x "$WASI_SDK/bin/clang" ] || { echo "the toolchain stage unpacks wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
  # Out of tree, so the source tree stays a clean base for patches.
  mkdir -p "$OPENSSL_BUILD"
  (
    cd "$OPENSSL_BUILD"
    if [ ! -f Makefile ]; then
      CC="$WASI_SDK/bin/clang" AR="$WASI_SDK/bin/llvm-ar" RANLIB="$WASI_SDK/bin/llvm-ranlib" \
      perl "$OPENSSL/Configure" linux-generic32 no-shared no-threads no-asm no-dso no-module no-engine \
        no-sock no-async no-apps no-tests no-secure-memory no-ui-console \
        --with-rand-seed=getrandom --prefix="$OUT/openssl"
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
# recorded under work/ for the feed stage.
inputs_digest() {
  cat "$HERE/manifest.env" "$HERE"/patches/*/*.patch | sha256sum | cut -c1-8
}
pack_version_file="$WORK/pack-version"
stamp_pack_version() {
  local build_id label stamp
  build_id="$(date -u +%Y%m%d).1"
  label="$PACK_LABEL-$(inputs_digest)-$(date -u +%H%M%S)"
  # Arcade's date stamp: yy * 1000 + mm * 50 + dd.
  stamp=$((10#$(date -u +%y) * 1000 + 10#$(date -u +%m) * 50 + 10#$(date -u +%d)))
  echo "10.0.$PACK_PATCH-$label.$PACK_ITERATION.$stamp.1" > "$pack_version_file"
  PACK_BUILD_ID="$build_id"
  PACK_VERSION_LABEL="$label"
}
stage_runtime() {
  echo "==> runtime (log: $LOGS/runtime-build.log)"
  [ -f "$OUT/openssl/lib/libcrypto.a" ] || { echo "the openssl stage installs OpenSSL under $OUT/openssl; run it first" >&2; exit 1; }
  [ -x "$WASI_SDK/bin/clang" ] || { echo "the toolchain stage unpacks wasi-sdk at $WASI_SDK; run it first" >&2; exit 1; }
  stamp_pack_version
  echo "    pack version $(cat "$pack_version_file")"
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

stage_feed() {
  echo "==> feed"
  local feed="$OUT/feed"
  mkdir -p "$feed"
  local prefix="Microsoft.NETCore.App.Runtime.Mono.wasi-wasm"
  [ -f "$pack_version_file" ] || { echo "the runtime stage records the pack version at $pack_version_file; run it first" >&2; exit 1; }
  local version
  version="$(cat "$pack_version_file")"
  local pack="$RUNTIME/artifacts/packages/Release/Shipping/$prefix.$version.nupkg"
  [ -f "$pack" ] || { echo "the runtime stage produced no $pack:" >&2; ls "$RUNTIME"/artifacts/packages/Release/Shipping/ >&2; exit 1; }
  local sdkpack="$RUNTIME/artifacts/packages/Release/Shipping/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk.$version.nupkg"
  [ -f "$sdkpack" ] || { echo "the runtime stage produced no Wasi.Sdk pack at $sdkpack" >&2; exit 1; }
  rm -f "$feed"/*.nupkg
  cp "$pack" "$sdkpack" "$feed/"
  # The pack's identity is its version plus the digest NuGet records for the
  # package, the form the SDK checks against the pack a build resolves.
  local digest
  digest="$(openssl dgst -sha512 -binary "$pack" | base64 -w0)"
  # Unpacked copies, for projects that import the packs explicitly instead of
  # through the installed workload.
  rm -rf "$OUT/packs"
  mkdir -p "$OUT/packs/runtime" "$OUT/packs/wasi-sdk"
  unzip -q "$pack" -d "$OUT/packs/runtime"
  unzip -q "$sdkpack" -d "$OUT/packs/wasi-sdk"
  # The SDK's Mono profile imports this to select the packs by version.
  cat > "$feed/Wisp.GuestRuntime.props" <<EOF
<Project>
  <PropertyGroup>
    <WispGuestRuntimePackVersion>$version</WispGuestRuntimePackVersion>
    <WispGuestRuntimePackSha512>$digest</WispGuestRuntimePackSha512>
    <WispGuestRuntimePackDirectory>$OUT/packs/runtime/</WispGuestRuntimePackDirectory>
    <WispGuestWasiSdkPackDirectory>$OUT/packs/wasi-sdk/</WispGuestWasiSdkPackDirectory>
  </PropertyGroup>
</Project>
EOF
  echo "    $(basename "$pack")"
  echo "    $(basename "$sdkpack")"
}

stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(acquire toolchain patch openssl runtime feed)
for stage in "${stages[@]}"; do
  "stage_$stage"
done
