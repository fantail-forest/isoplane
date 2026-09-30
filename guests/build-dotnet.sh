#!/usr/bin/env bash
# Build the C# test fixtures; samples have their own build scripts.
# Usage: guests/build-dotnet.sh [--if-stale] [--fixture name]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELECTED=""
while [ $# -gt 0 ]; do
  case "$1" in
    --if-stale) shift ;;
    --fixture)
      [ $# -ge 2 ] || { echo "missing fixture name" >&2; exit 2; }
      SELECTED="$2"; shift 2 ;;
    --runtime)
      [ $# -ge 2 ] || { echo "missing runtime name" >&2; exit 2; }
      case "$2" in
        mono) ;;
        *) echo "unknown runtime: $2 (expected mono)" >&2; exit 2 ;;
      esac
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -n "$SELECTED" ]; then
  exec task --dir "$ROOT/.." "fixtures:dotnet:$SELECTED"
fi
exec task --dir "$ROOT/.." fixtures:dotnet
