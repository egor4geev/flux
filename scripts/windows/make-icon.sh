#!/bin/zsh
# Flux.exe's icon (part 10.9): crates/flux-app/resources/windows/flux.ico, from the same picture as
# the Mac's (scripts/app-icon.swift: the glass tile and the logo), cropped to the tile — a Windows
# icon fills its square, macOS keeps a margin — at the sizes Explorer, the taskbar and the title bar
# ask for, as PNG images. Committed: the Windows build (CI) can't draw it. Run again after the logo
# changes; needs macOS (swift, sips) and python3.
#
#   scripts/windows/make-icon.sh
set -euo pipefail
cd "${0:A:h}/../.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
swift scripts/app-icon.swift crates/flux-app/assets/brand/logo.svg "$work/flux.iconset"
# The tile is 824 of 1024 pixels with its shadow below: 912 keep the shadow's fade.
sips -c 912 912 "$work/flux.iconset/icon_512x512@2x.png" --out "$work/tile.png" >/dev/null
sizes=(16 20 24 32 40 48 64 96 128 256)
for size in $sizes; do
  sips -z $size $size "$work/tile.png" --out "$work/$size.png" >/dev/null
done
python3 -I - "$work" crates/flux-app/resources/windows/flux.ico $sizes <<'PY'
# An .ico: a directory of PNG images (Windows reads them since Vista), the smallest first.
import struct, sys
work, out, sizes = sys.argv[1], sys.argv[2], [int(size) for size in sys.argv[3:]]
images = [open(f"{work}/{size}.png", "rb").read() for size in sizes]
data = struct.pack("<HHH", 0, 1, len(images))
offset = len(data) + 16 * len(images)
for size, image in zip(sizes, images):
    # 256 is written as 0; one plane, 32 bits per pixel.
    data += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset)
    offset += len(image)
open(out, "wb").write(data + b"".join(images))
PY
echo crates/flux-app/resources/windows/flux.ico
