#!/usr/bin/env bash
# Download the compiler feed for committed dependency pins: fetch.sh OUTPUT_DIR
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "$#" != 1 ]; then
  echo "Usage: $0 OUTPUT_DIR" >&2
  exit 2
fi
source "$HERE/manifest.env"
revision=$(git -C "$HERE/.." rev-parse HEAD:vendor/dotnet-runtime)
url="https://github.com/fantail-forest/isoplane-dotnet-runtime/releases/download/isoplane-runtime-$revision"
mkdir -p "$1"
output=$(realpath "$1")
download=$(mktemp -d "$output/.download.XXXXXX")
trap 'rm -rf "$download"' EXIT
cd "$download"
echo "Downloading runtime feed for $revision"
if ! curl --fail --location --retry 3 "$url/runtime-feed.tar.gz" -o runtime-feed.tar.gz; then
  echo "The pinned runtime feed must be published by the fork build first: $url" >&2
  exit 1
fi
curl --fail --location --retry 3 "$url/SHA256SUMS" -o SHA256SUMS
sha256sum --check SHA256SUMS
tar -xzf runtime-feed.tar.gz --no-same-owner
"${ISOPLANE_PYTHON:-python3}" "$HERE/feed-identity.py" verify-release "$download/feed" "$WASI_SDK_VERSION"
rm -rf "$output/feed" "$output/packs"
mv feed packs "$output/"
