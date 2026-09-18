#!/usr/bin/env bash
# Build the reference C# guest. All the work is in the SDK's one build driver.
#
#   scripts/dev guests/dotnet/build.sh [-p:IsoplaneCompatProbe=true] ...
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$HERE/../../guest-sdks/dotnet/build-guest.sh" "$HERE" "$@"
