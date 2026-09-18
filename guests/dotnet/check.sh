#!/usr/bin/env bash
# Run the C# guest under the hostcheck harness in guests/dotnet/hostcheck.
#
#   scripts/dev guests/dotnet/check.sh [--wasi=pal|trap|stub] [--density N]
#
#   --wasi=pal    (default) bind the runtime PAL set; every other wasi import
#                 traps if called
#   --wasi=trap   every non-`isoplane` import traps
#   --wasi=stub   nothing traps; enumerate which imports are reachable
#   --density N   activate N instances and report marginal RSS
#   --compat      expect the compatibility probe response; build with the probe enabled
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec cargo run --quiet --release --bin isoplane-dotnet-hostcheck --manifest-path "$HERE/hostcheck/Cargo.toml" -- \
  "$HERE/out/isoplane_guest_dotnet.wasm" "$@"
