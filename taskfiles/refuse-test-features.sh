#!/usr/bin/env bash
# Run a Cargo build and fail when a package it compiled enables a test-util feature:
# refuse-test-features.sh cargo build ... --message-format=json-render-diagnostics
set -euo pipefail
"$@" | jq -nRr -f "$(dirname "${BASH_SOURCE[0]}")/refuse-test-features.jq"
