#!/usr/bin/env bash
# Turn the uncommitted changes in a work tree into the next numbered patch:
#   guest-runtime/export-patch.sh runtime|runtimelab|openssl <name>
# Writes patches/<tree>/NNN-<name>.patch (and names it in the tree's series
# file, where one exists) and commits the change in the work tree; build.sh
# patch then re-materializes the tree from the declared series.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tree="$1"
name="$2"
work="$HERE/work/$tree"
directory="$HERE/patches/$tree"
mkdir -p "$directory"
# Number after the highest existing patch: the series is not contiguous.
last=$(find "$directory" -maxdepth 1 -name '[0-9][0-9][0-9]-*.patch' -printf '%f\n' | sort | tail -1 | cut -c1-3)
file="$directory/$(printf '%03d' $((10#${last:-0} + 1)))-$name.patch"
git -C "$work" add -A
git -C "$work" diff --cached --no-color > "$file"
[ -s "$file" ] || { rm -f "$file"; echo "no changes to export" >&2; exit 1; }
git -C "$work" -c user.name=isoplane -c user.email=isoplane@localhost commit -q -m "$(basename "$file")"
[ ! -f "$directory/series" ] || echo "$(basename "$file")" >> "$directory/series"
echo "$file"
