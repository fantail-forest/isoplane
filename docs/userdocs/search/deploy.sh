#!/usr/bin/env bash
# Deploy the search server to Fly and the site, with the matching index
# version, to Cloudflare. Run after `pnpm run build` and `pnpm run search:index`.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

version="$(cat snapshot/VERSION)" || { echo "no snapshot; run pnpm run search:index first" >&2; exit 1; }

fly deploy --ha=false
cd ..
# A new index version is a new edge cache key for every search.
wrangler deploy --var "SEARCH_INDEX_VERSION:$version"
