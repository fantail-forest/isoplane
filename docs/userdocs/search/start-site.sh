#!/usr/bin/env bash
# Start the local site against the local search server that serve.sh runs,
# with the key from search/.env; nothing is typed into a shell.
#   pnpm run search:start
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi
: "${DOCS_SEARCH_KEY:?set DOCS_SEARCH_KEY in search/.env to the value build-index.sh was given}"
export DOCS_SEARCH_KEY
export DOCS_SEARCH_URL="http://127.0.0.1:${DOCS_SEARCH_PORT:-8118}"
cd ..
exec pnpm start
