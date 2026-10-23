#!/usr/bin/env bash
# Regenerates crates/fbterm/macos/fbterm.icns from the featherbit brand mark.
# Needs macOS (sips, iconutil) and python3 with Pillow (master composition only).
# Usage: dev/make-icns.sh [path/to/featherbit-mark.png]
set -euo pipefail
[[ "$(uname -s)" == "Darwin" ]] || { echo "make-icns.sh runs on macOS only" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
mark="${1:-$root/../design-system-v2/assets/featherbit-mark.png}"
out="$root/crates/fbterm/macos/fbterm.icns"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
iconset="$work/fbterm.iconset"
mkdir -p "$iconset"

# 1024 master: graphite rounded-square tile (macOS icon grid: 824px tile in a
# 1024 canvas) with the mark centred, aspect ratio preserved.
python3 - "$mark" "$work/master.png" <<'PY'
import sys
from PIL import Image, ImageDraw
mark = Image.open(sys.argv[1]).convert("RGBA")
S, TILE = 1024, 824
off = (S - TILE) // 2
scale = 4
mask = Image.new("L", (S * scale, S * scale), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    [off * scale, off * scale, (off + TILE) * scale - 1, (off + TILE) * scale - 1],
    radius=185 * scale, fill=255)
mask = mask.resize((S, S), Image.LANCZOS)
canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
canvas.paste(Image.new("RGBA", (S, S), (22, 23, 27, 255)), (0, 0), mask)
h = int(TILE * 0.68)
w = round(mark.width * h / mark.height)
m = mark.resize((w, h), Image.LANCZOS)
canvas.alpha_composite(m, ((S - w) // 2, (S - h) // 2))
canvas.save(sys.argv[2])
PY

for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$work/master.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" "$work/master.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$out"
echo "wrote $out"
