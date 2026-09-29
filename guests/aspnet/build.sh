#!/usr/bin/env bash
# Build the Mono fixture for ASP.NET scope, cancellation and streaming tests.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$HERE/../../guest-sdks/dotnet/build-guest.sh" "$HERE" "$@"
