#!/usr/bin/env bash
# Pack the adapter and build an isolated ASP.NET package consumer.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUESTS="$(cd "$HERE/.." && pwd)"
if [ "${WISP_FIXTURE_LOCK:-}" != "$GUESTS/build-dotnet.sh" ] && [ "${WISP_FIXTURE_LOCK:-}" != "$HERE/build.sh" ]; then
  exec python3 "$HERE/../../scripts/fixture-tools.py" lock "$GUESTS/.build-dotnet.lock" "$HERE/build.sh" "$@"
fi
exec python3 "$HERE/build.py" "$@"
