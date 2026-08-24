#!/usr/bin/env bash
# Check the managed HTTP fixture response and completed-state memory copy.
# Runs INSIDE the wisp-dev container:  scripts/dev guests/dotnet/check.sh
#
#   --wasi=pal    (default) implement only the measured runtime-PAL set;
#                 the other 29 wasi imports trap if called
#   --wasi=trap   every non-`wisp` import traps — for this fixture execution
#   --wasi=stub   nothing traps; enumerate which imports are reachable
#   --density N   activate N instances and report marginal RSS (§6.2)
#   --compat      expect the compatibility probe response; build with the probe enabled
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec cargo run --quiet --release --bin wisp-dotnet-hostcheck --manifest-path "$HERE/hostcheck/Cargo.toml" -- \
  "$HERE/out/wisp_guest_dotnet.wasm" "$@"
