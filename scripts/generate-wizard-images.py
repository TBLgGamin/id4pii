"""Generate Inno Setup wizard images from the master logo.

Renders the shield (preserving its original dark color) centered on a clean
white canvas. Outputs:

  installer/wizard-image.bmp  164x314  welcome and finish pages
  installer/wizard-small.bmp   55x58   header strip

Run after changing assets/icon-256.png. The BMPs are checked in.
"""
import os
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "assets" / "icon-256.png"
LARGE = ROOT / "installer" / "wizard-image.bmp"
SMALL = ROOT / "installer" / "wizard-small.bmp"
BG = (255, 255, 255)


def load_shield():
    icon = Image.open(SRC).convert("RGBA")
    bbox = icon.getbbox()
    return icon.crop(bbox)


def render(width, height, fill_ratio):
    canvas = Image.new("RGB", (width, height), BG)
    shield = load_shield()
    sw, sh = shield.size
    box_w = int(width * fill_ratio)
    box_h = int(height * fill_ratio)
    ratio = min(box_w / sw, box_h / sh)
    nw = max(1, int(sw * ratio))
    nh = max(1, int(sh * ratio))
    scaled = shield.resize((nw, nh), Image.LANCZOS)
    on_white = Image.new("RGB", (nw, nh), BG)
    on_white.paste(scaled, (0, 0), scaled)
    cx = (width - nw) // 2
    cy = (height - nh) // 2
    canvas.paste(on_white, (cx, cy))
    return canvas


def main():
    render(164, 314, fill_ratio=0.55).save(LARGE, "BMP")
    render(55, 58, fill_ratio=0.85).save(SMALL, "BMP")
    for path in (LARGE, SMALL):
        size = os.path.getsize(path)
        print(f"{path.relative_to(ROOT)}: {size} bytes")


if __name__ == "__main__":
    main()
