#!/usr/bin/env bash
# Regenerate crates/fbterm/assets/fonts/lucide-icons.ttf: the Lucide icon font (lucide-static,
# ISC License) subset to the icons the app draws. Needs npm and fonttools (`pip install
# fonttools`, for pyftsubset).
#
# To add an icon: add its Lucide name to ICONS, run this script, and add a constant with the
# codepoint it prints to crates/fbterm/src/ui.rs.
set -euo pipefail

VERSION=1.54.0
ICONS=(x plus users chevron-up chevron-down copy shield-alert lock)

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

(cd "$work" && npm pack --silent "lucide-static@$VERSION" >/dev/null && tar xzf "lucide-static-$VERSION.tgz")
font="$work/package/font"

unicodes=$(python3 - "$font/codepoints.json" "${ICONS[@]}" <<'EOF'
import json, sys
points = json.load(open(sys.argv[1]))
for name in sys.argv[2:]:
    print(f"{name} U+{points[name]:04X}", file=sys.stderr)
print(",".join(f"U+{points[name]:04X}" for name in sys.argv[2:]))
EOF
)

pyftsubset "$font/lucide.ttf" --unicodes="$unicodes" --layout-features='' --no-hinting \
    --output-file="$root/crates/fbterm/assets/fonts/lucide-icons.ttf"
cp "$work/package/LICENSE" "$root/crates/fbterm/assets/fonts/LICENSE-lucide"
echo "wrote crates/fbterm/assets/fonts/lucide-icons.ttf (lucide-static $VERSION)"
