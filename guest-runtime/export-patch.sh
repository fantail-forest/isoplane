#!/usr/bin/env bash
# Turn the uncommitted changes in a work tree into the next numbered patch:
#   guest-runtime/export-patch.sh runtime|runtimelab|openssl <name>
# Writes patches/<tree>/NNN-<name>.patch (and names it in the tree's series
# file, where one exists), commits the change in the work tree and records it
# as applied, so build.sh patch does not apply it twice.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tree="$1"
name="$2"
work="$HERE/work/$tree"
directory="$HERE/patches/$tree"
mkdir -p "$directory"
count=$(find "$directory" -name '*.patch' | wc -l)
file="$directory/$(printf '%03d' $((count + 1)))-$name.patch"
git -C "$work" add -A
git -C "$work" diff --cached --no-color > "$file"
[ -s "$file" ] || { rm -f "$file"; echo "no changes to export" >&2; exit 1; }
git -C "$work" -c user.name=wisp -c user.email=wisp@localhost commit -q -m "$(basename "$file")"
[ ! -f "$directory/series" ] || echo "$(basename "$file")" >> "$directory/series"
echo "$(basename "$file")" >> "$work/.wisp-applied"
echo "$file"
