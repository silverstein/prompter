#!/bin/bash
# Package Prompter.app as a drag-to-Applications disk image.
# Usage: scripts/make-dmg.sh <path/to/Prompter.app> <output.dmg>
set -euo pipefail
app="$1"; out="$2"
test -d "$app" || { echo "no app bundle at $app" >&2; exit 1; }
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
mkdir -p "$(dirname "$out")"
rm -f "$out"
hdiutil create -volname "Prompter" -srcfolder "$stage" -ov -format UDZO "$out" >/dev/null
echo "  Created $out"
