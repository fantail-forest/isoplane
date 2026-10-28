#!/bin/bash
# Run Typesense on loopback and nginx on the machine's port; end the machine
# when either stops, so Fly restarts it whole.
set -eu
: "${TYPESENSE_API_KEY:?}" "${DOCS_SEARCH_KEY:?}"

/opt/typesense-server &
typesense=$!
/docker-entrypoint.sh nginx -g 'daemon off;' &
nginx=$!
trap 'kill -TERM $typesense $nginx 2>/dev/null' TERM INT

wait -n "$typesense" "$nginx"
status=$?
kill -TERM "$typesense" "$nginx" 2>/dev/null
exit "$status"
