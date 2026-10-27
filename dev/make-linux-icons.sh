#!/usr/bin/env bash
# Regenerates the hicolor PNG icons under crates/fbterm/linux/icons/ from the featherbit
# brand mark: the same graphite tile as the macOS icon (dev/make-icns.sh).
# Needs python3 with Pillow; runs on any system.
# Usage: dev/make-linux-icons.sh [path/to/featherbit-mark.png]
set -euo pipefail

python3 -c "import PIL" 2>/dev/null || { echo "make-linux-icons.sh needs python3 with Pillow (pip install pillow)" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
mark="${1:-$root/../design-system-v2/assets/featherbit-mark.png}"
[[ -f "$mark" ]] || { echo "mark not found: $mark" >&2; exit 1; }
out="$root/crates/fbterm/linux/icons/hicolor"

python3 - "$mark" "$out" <<'PY'
import os, sys
from PIL import Image, ImageDraw
mark = Image.open(sys.argv[1]).convert("RGBA")
out = sys.argv[2]
# 1024 master: graphite rounded-square tile (824px in a 1024 canvas) with the mark centred,
# aspect ratio preserved.
S, TILE = 1024, 824
off = (S - TILE) // 2
scale = 4
mask = Image.new("L", (S * scale, S * scale), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    [off * scale, off * scale, (off + TILE) * scale - 1, (off + TILE) * scale - 1],
    radius=185 * scale, fill=255)
mask = mask.resize((S, S), Image.LANCZOS)
master = Image.new("RGBA", (S, S), (0, 0, 0, 0))
master.paste(Image.new("RGBA", (S, S), (22, 23, 27, 255)), (0, 0), mask)
h = int(TILE * 0.68)
w = round(mark.width * h / mark.height)
m = mark.resize((w, h), Image.LANCZOS)
master.alpha_composite(m, ((S - w) // 2, (S - h) // 2))
for s in (32, 64, 128, 256, 512):
    d = os.path.join(out, f"{s}x{s}", "apps")
    os.makedirs(d, exist_ok=True)
    # No timestamps or text chunks, so a rerun from the same mark gives the same bytes.
    master.resize((s, s), Image.LANCZOS).save(os.path.join(d, "fbterm.png"), optimize=True)
    print(os.path.join(d, "fbterm.png"))
PY
