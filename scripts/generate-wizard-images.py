"""Generate Inno Setup wizard images from the master logo.

Renders the shield (recolored white) centered on a near-black canvas
matching the shadcn dark-mode neutral palette. Outputs:

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
BG = (10, 10, 10)
FG = (250, 250, 250)


def load_shield():
    icon = Image.open(SRC).convert("RGBA")
    bbox = icon.getbbox()
    return icon.crop(bbox)


def recolor_white(rgba_img):
    out = Image.new("RGBA", rgba_img.size, (0, 0, 0, 0))
    pixels_in = rgba_img.load()
    pixels_out = out.load()
    w, h = rgba_img.size
    for x in range(w):
        for y in range(h):
            r, g, b, a = pixels_in[x, y]
            if a > 0:
                pixels_out[x, y] = (FG[0], FG[1], FG[2], a)
    return out


def render(width, height, shield_h_ratio):
    canvas = Image.new("RGB", (width, height), BG)
    shield = load_shield()
    sw, sh = shield.size
    target_h = int(height * shield_h_ratio)
    ratio = target_h / sh
    nw = max(1, int(sw * ratio))
    nh = max(1, int(sh * ratio))
    scaled = shield.resize((nw, nh), Image.LANCZOS)
    white = recolor_white(scaled)
    cx = (width - nw) // 2
    cy = int(height * 0.20)
    canvas.paste(white, (cx, cy), white)
    return canvas


def main():
    render(164, 314, shield_h_ratio=0.55).save(LARGE, "BMP")
    render(55, 58, shield_h_ratio=0.85).save(SMALL, "BMP")
    for path in (LARGE, SMALL):
        size = os.path.getsize(path)
        print(f"{path.relative_to(ROOT)}: {size} bytes")


if __name__ == "__main__":
    main()
