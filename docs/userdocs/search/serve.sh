#!/usr/bin/env bash
# Run the production search image locally from search/snapshot, on the port
# build-index.sh uses, for a local site started with
#   DOCS_SEARCH_URL=http://127.0.0.1:8118 DOCS_SEARCH_KEY=<key> pnpm start
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi
: "${DOCS_SEARCH_KEY:?set DOCS_SEARCH_KEY to the key build-index.sh was given}"
port="${DOCS_SEARCH_PORT:-8118}"
docker build -q -t isoplane-docs-search . > /dev/null
exec docker run --rm -p "127.0.0.1:$port:8080" \
  -e TYPESENSE_API_KEY="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')" \
  -e DOCS_SEARCH_KEY="$DOCS_SEARCH_KEY" \
  isoplane-docs-search
