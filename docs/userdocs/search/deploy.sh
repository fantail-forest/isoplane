#!/usr/bin/env bash
# Deploy the search server to Fly and the site, with the matching index
# version, to Cloudflare. Run after `pnpm run build` and `pnpm run search:index`.
# Creates the Fly app on first use and sets both platforms' secrets from
# search/.env without passing one on a command line. Authenticate the
# Fly and Wrangler CLIs before running this script.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi
: "${DOCS_SEARCH_KEY:?set DOCS_SEARCH_KEY in search/.env to the value build-index.sh was given}"
app="$(sed -n 's/^app = "\(.*\)"$/\1/p' fly.toml)"
version="$(cat snapshot/VERSION)" || { echo "no snapshot; run pnpm run search:index first" >&2; exit 1; }

fly apps list --quiet | grep -qx "$app" || fly apps create "$app" --yes

# The admin key is generated once and never read by anything in this repo;
# Typesense needs one to start. The search-only key may change with .env.
fly_secrets="DOCS_SEARCH_KEY=$DOCS_SEARCH_KEY"
if ! fly secrets list --app "$app" | awk 'NR > 1 {print $1}' | grep -qx TYPESENSE_API_KEY; then
  fly_secrets="$fly_secrets
TYPESENSE_API_KEY=$(openssl rand -hex 24)"
fi
printf '%s\n' "$fly_secrets" | fly secrets import --app "$app" --stage
fly deploy --ha=false

cd ..
# One deploy carries the index version, a new edge cache key for every search,
# and the Worker's secret, which Wrangler reads from a file only it opens. The
# Cloudflare authentication comes from Wrangler configuration or environment.
worker_secrets="$(mktemp)"
trap 'rm -f "$worker_secrets"' EXIT
printf 'DOCS_SEARCH_KEY=%s\n' "$DOCS_SEARCH_KEY" > "$worker_secrets"
pnpm exec wrangler deploy --secrets-file "$worker_secrets" --var "SEARCH_INDEX_VERSION:$version"
