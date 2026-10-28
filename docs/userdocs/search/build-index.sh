#!/usr/bin/env bash
# Build the docs search index from the built site into search/snapshot/.
#
#   search/build-index.sh            after `pnpm run build`
#
# Reads search/.env if present. Variables:
#   DOCS_SEARCH_KEY                 search-only key value (required); the Worker
#                                   holds the same value as its DOCS_SEARCH_KEY secret
#   DOCS_SEARCH_EMBEDDING_API_KEY   key for the embedding provider named in
#                                   scraper.config.json (required unless off)
#   DOCS_SEARCH_EMBEDDINGS=off      keyword-only index, no provider needed
#   DOCS_SEARCH_EMBEDDING_MODEL     override the model, e.g. ts/all-MiniLM-L12-v2
#   DOCS_SEARCH_EMBEDDING_URL       base URL of an OpenAI-compatible provider
#   DOCS_SEARCH_PORT                loopback port for the build's Typesense (8118)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi

: "${DOCS_SEARCH_KEY:?set DOCS_SEARCH_KEY to the search-only key value}"
export DOCS_SEARCH_PORT="${DOCS_SEARCH_PORT:-8118}"
export DOCS_SEARCH_UID DOCS_SEARCH_GID DOCS_SEARCH_ADMIN_KEY DOCS_SEARCH_SCRAPER_CONFIG
DOCS_SEARCH_UID="$(id -u)"
DOCS_SEARCH_GID="$(id -g)"
DOCS_SEARCH_ADMIN_KEY="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
api="http://127.0.0.1:${DOCS_SEARCH_PORT}"

[ -f ../build/docs/sitemap.xml ] || { echo "build/docs is missing; run pnpm run build first" >&2; exit 1; }

# The scraper reads its whole config from one environment variable. The
# committed config names the production embedding model; the environment adds
# the provider key, or swaps the model, or drops embeddings altogether.
embeddings_filter='.'
if [ "${DOCS_SEARCH_EMBEDDINGS:-on}" = off ]; then
  embeddings_filter='.custom_settings.field_definitions |= map(select(.name != "embedding"))'
else
  model="${DOCS_SEARCH_EMBEDDING_MODEL:-}"
  if [ -z "$model" ]; then
    model="$(jq -r '.custom_settings.field_definitions[] | select(.name == "embedding") | .embed.model_config.model_name' scraper.config.json)"
  fi
  case "$model" in
    ts/*) ;;
    *) : "${DOCS_SEARCH_EMBEDDING_API_KEY:?set DOCS_SEARCH_EMBEDDING_API_KEY for $model, or DOCS_SEARCH_EMBEDDINGS=off}" ;;
  esac
  embeddings_filter='(.custom_settings.field_definitions[] | select(.name == "embedding") | .embed.model_config) |=
    ({model_name: $model}
     + (if $key == "" then {} else {api_key: $key} end)
     + (if $url == "" then {} else {url: $url} end))'
fi
DOCS_SEARCH_SCRAPER_CONFIG="$(jq -c \
  --arg model "${model:-}" --arg key "${DOCS_SEARCH_EMBEDDING_API_KEY:-}" --arg url "${DOCS_SEARCH_EMBEDDING_URL:-}" \
  "$embeddings_filter" scraper.config.json)"

rm -rf .work snapshot
mkdir -p .work/data .work/snapshot
sed 's#https://isoplane.dev/#http://site/#g' ../build/docs/sitemap.xml > .work/sitemap.xml

cleanup() { docker compose down --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker compose up -d typesense site
for _ in $(seq 1 60); do
  curl -sf "$api/health" | grep -q '"ok":true' && break
  sleep 1
done
curl -sf "$api/health" | grep -q '"ok":true' || { echo "Typesense did not become healthy" >&2; exit 1; }

docker compose run --rm scraper

collection="$(curl -sf -H "X-TYPESENSE-API-KEY: $DOCS_SEARCH_ADMIN_KEY" "$api/aliases/isoplane-docs" | jq -r .collection_name)"
records="$(curl -sf -H "X-TYPESENSE-API-KEY: $DOCS_SEARCH_ADMIN_KEY" "$api/collections/$collection" | jq -r .num_documents)"
[ "${records:-0}" -gt 0 ] || { echo "the crawl indexed no records" >&2; exit 1; }

# The only key the server will ever answer searches for. The Worker sends it;
# the site itself never sees it.
curl -sf -X POST -H "X-TYPESENSE-API-KEY: $DOCS_SEARCH_ADMIN_KEY" "$api/keys" \
  -d "$(jq -cn --arg value "$DOCS_SEARCH_KEY" \
        '{description: "isoplane.dev/docs/search", actions: ["documents:search"], collections: ["*"], value: $value}')" \
  > /dev/null

curl -sf -X POST -H "X-TYPESENSE-API-KEY: $DOCS_SEARCH_ADMIN_KEY" "$api/operations/snapshot?snapshot_path=/snapshot/data" \
  | grep -q '"success":true' || { echo "snapshot failed" >&2; exit 1; }
docker compose stop typesense >/dev/null

mv .work/snapshot snapshot
version="$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)"
printf '%s\n' "$version" > snapshot/VERSION
echo "indexed $records records from $collection into search/snapshot ($version)"
