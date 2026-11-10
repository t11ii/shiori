#!/usr/bin/env python3
"""Render the app icon files from shiori.svg.

    pip install cairosvg pillow
    python3 assets/icon/build_icons.py

Outputs (next to this script):
  shiori.icns       macOS app bundle icon (cargo-bundle)
  shiori.ico        Windows exe icon (embedded by build.rs)
  shiori-512.png    window / Dock icon at runtime (macOS style, with margin)
  shiori-win-256.png window icon at runtime on Windows (full bleed)
"""
import io
import re
from pathlib import Path

import cairosvg
from PIL import Image

HERE = Path(__file__).resolve().parent
MAC_SVG = (HERE / "shiori.svg").read_text(encoding="utf-8")

# Windows icons have no standard margin, so crop the macOS grid margin away and
# drop the tile shadow; small sizes (16/32px) stay legible this way.
WIN_SVG = re.sub(r'viewBox="[^"]*"', 'viewBox="100 100 824 824"', MAC_SVG, count=1)
WIN_SVG = WIN_SVG.replace(' filter="url(#tileShadow)"', "")


def render(svg: str, size: int) -> Image.Image:
    png = cairosvg.svg2png(bytestring=svg.encode(), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def main() -> None:
    ico_sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    ico = [render(WIN_SVG, s) for s in ico_sizes]
    ico[-1].save(HERE / "shiori.ico", sizes=[(s, s) for s in ico_sizes], append_images=ico[:-1])

    icns_sizes = [16, 32, 64, 128, 256, 512, 1024]
    icns = [render(MAC_SVG, s) for s in icns_sizes]
    icns[-1].save(HERE / "shiori.icns", append_images=icns[:-1])

    render(MAC_SVG, 512).save(HERE / "shiori-512.png", optimize=True)
    render(WIN_SVG, 256).save(HERE / "shiori-win-256.png", optimize=True)


if __name__ == "__main__":
    main()
