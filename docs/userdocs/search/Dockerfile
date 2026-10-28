# The search server: Typesense with the index that build-index.sh snapshotted
# baked in as its data directory. Nothing writes to it in production, so a
# deploy replaces the whole machine image and no volume is needed.
FROM typesense/typesense:30.2

COPY snapshot/data /data

ENV TYPESENSE_DATA_DIR=/data \
    TYPESENSE_ENABLE_CORS=false \
    TYPESENSE_CACHE_NUM_ENTRIES=10000 \
    TYPESENSE_EMBEDDING_CACHE_NUM_ENTRIES=5000

# TYPESENSE_API_KEY, the admin key, comes from the environment at run time
# (a Fly secret). Searches use the search-only key baked into the snapshot.
