#!/usr/bin/env bash
# Check the managed HTTP fixture response and completed-state memory copy.
# Runs INSIDE the wisp-dev container:  scripts/dev guests/dotnet/check.sh
#   --compat      expect the compatibility probe response; build with the probe enabled
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cargo build --release --manifest-path "$HERE/hostcheck/Cargo.toml"
exec "$HERE/hostcheck/target/release/wisp-dotnet-hostcheck" "$HERE/out/wisp_guest_dotnet.wasm" "$@"
