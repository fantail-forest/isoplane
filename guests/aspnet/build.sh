#!/usr/bin/env bash
# Build the Mono fixture for ASP.NET scope, cancellation and streaming tests.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUESTS="$(cd "$HERE/.." && pwd)"
if [ "${ISOPLANE_FIXTURE_LOCK:-}" != "$GUESTS/build-dotnet.sh" ] && [ "${ISOPLANE_FIXTURE_LOCK:-}" != "$HERE/build.sh" ]; then
  exec python3 "$HERE/../../scripts/fixture-tools.py" lock "$GUESTS/.build-dotnet.lock" "$HERE/build.sh" "$@"
fi
exec "$HERE/../../guest-sdks/dotnet/build-guest.sh" "$HERE" "$@"
