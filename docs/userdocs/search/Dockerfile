# The search server: Typesense with the index that build-index.sh snapshotted
# baked in as its data directory, behind an nginx that exposes only the search
# and health routes. Nothing writes to the index in production, so a deploy
# replaces the whole machine image and no volume is needed.
FROM typesense/typesense:30.2 AS typesense

FROM nginx:1.28
COPY --from=typesense /opt/typesense-server /opt/typesense-server
COPY snapshot/data /data
COPY nginx.template.conf /etc/nginx/templates/default.conf.template
COPY start.sh /usr/local/bin/start-search

# Typesense listens on loopback only; the machine's port is nginx's.
ENV TYPESENSE_DATA_DIR=/data \
    TYPESENSE_API_ADDRESS=127.0.0.1 \
    TYPESENSE_API_PORT=8108 \
    TYPESENSE_ENABLE_CORS=false \
    TYPESENSE_CACHE_NUM_ENTRIES=10000 \
    TYPESENSE_EMBEDDING_CACHE_NUM_ENTRIES=5000

# Remote embeddings call the leader's API through its peering address.
ENV TYPESENSE_PEERING_ADDRESS=${TYPESENSE_API_ADDRESS}

# TYPESENSE_API_KEY, the admin key, and DOCS_SEARCH_KEY, the search-only key
# nginx admits, come from the environment at run time (Fly secrets).
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/start-search"]
