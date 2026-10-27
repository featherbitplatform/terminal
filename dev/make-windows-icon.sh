#!/usr/bin/env bash
# Regenerates crates/fbterm/windows/fbterm.ico (16, 24, 32, 48, 64, 128 and 256 px) from the
# featherbit brand mark: the same graphite tile as the macOS and Linux icons
# (dev/make-icns.sh, dev/make-linux-icons.sh). build.rs embeds it in fbterm.exe and the
# installer (crates/fbterm/wix/main.wxs) uses it for the Start menu shortcut and Apps list.
# Needs python3 with Pillow; runs on any system.
# Usage: dev/make-windows-icon.sh [path/to/featherbit-mark.png]
set -euo pipefail

python3 -c "import PIL" 2>/dev/null || { echo "make-windows-icon.sh needs python3 with Pillow (pip install pillow)" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
mark="${1:-$root/../design-system-v2/assets/featherbit-mark.png}"
[[ -f "$mark" ]] || { echo "mark not found: $mark" >&2; exit 1; }
out="$root/crates/fbterm/windows/fbterm.ico"
mkdir -p "$(dirname "$out")"

python3 - "$mark" "$out" <<'PY'
import sys
from PIL import Image, ImageDraw
mark = Image.open(sys.argv[1]).convert("RGBA")
out = sys.argv[2]
# 1024 master: graphite rounded-square tile (824px in a 1024 canvas) with the mark centred,
# aspect ratio preserved (as in make-linux-icons.sh).
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
# Each size resampled from the master (not from the next larger one), largest first.
sizes = (256, 128, 64, 48, 32, 24, 16)
frames = [master.resize((s, s), Image.LANCZOS) for s in sizes]
frames[0].save(out, format="ICO", sizes=[(s, s) for s in sizes], append_images=frames[1:])
print(out)
PY
