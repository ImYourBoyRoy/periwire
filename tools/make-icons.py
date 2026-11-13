#!/usr/bin/env python3
"""Regenerate every app and tray icon from the artwork in images/.

Developer tool, not part of the product. Needs:  pip install playwright pillow
and a Chrome or Chromium (CHROME=/path/to/browser, else google-chrome / chromium on PATH, else playwright's own).

    python3 tools/make-icons.py

Sources (the design lives here, nothing is drawn in code):
    images/PeriWire-1024.png   the finished app icon (used as designed for 128 px and up)
    images/PeriWire-icon.svg   the same icon as vector (re-rendered with a sturdier wire for the smallest sizes)
    images/PeriWire-mark.svg   the P-shaped wire alone, one colour (tray icons, in three colours)

Output: gui/src-tauri/icons/*.png|ico|icns and gui/ui/logo.png.

Why variants: the thin wire, the ring and the block disappear below about 64 px, so 16-64 px icons use heavier strokes, bigger
terminals and a larger mark; macOS icons sit inside the standard 824 px safe area with a soft shadow; tray glyphs are the bare mark.
"""
import io, os, re, shutil, struct
from pathlib import Path

from PIL import Image, ImageFilter
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "images"
OUT = ROOT / "gui" / "src-tauri" / "icons"
UI = ROOT / "gui" / "ui"

TRAY_COLORS = {"tray.png": "#F0EBE4", "tray-active.png": "#4ADE80", "tray-warn.png": "#FACC15"}


def find_browser():
    if os.environ.get("CHROME"):
        return os.environ["CHROME"]
    for name in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser"):
        p = shutil.which(name)
        if p:
            return p
    return None


def sturdy_icon(svg: str) -> str:
    """The icon with the wire drawn heavier, the terminals bigger and the whole mark larger inside the tile (for 16-64 px)."""
    s = svg.replace('stroke-width="53.5"', 'stroke-width="84"')
    s = re.sub(r'<path fill-rule="evenodd" d="M440 254[^"]*"/>', '<path fill-rule="evenodd" d="M440 244 A66 66 0 1 1 440 376 A66 66 0 1 1 440 244 Z M440 278 A32 32 0 1 0 440 342 A32 32 0 1 0 440 278 Z"/>', s)
    s = s.replace('<rect x="652" y="458" width="81" height="84" rx="19"/>', '<rect x="636" y="444" width="108" height="112" rx="24"/>')
    s = s.replace('<circle cx="425" cy="935" r="52"/>', '<circle cx="425" cy="935" r="68"/>')
    s = s.replace(' filter="url(#markShadow)"', "")  # a shadow is just mush at this size
    s = s.replace('<g>\n  <g fill="none"', '<g transform="translate(620 622) scale(1.2) translate(-620 -622)">\n  <g fill="none"', 1)
    return s


def tray_mark(svg: str, color: str) -> str:
    """The bare mark, heavier, centred in a square, in one colour."""
    s = svg.replace('stroke-width="53.5"', 'stroke-width="98"')
    s = re.sub(r'<path fill-rule="evenodd" d="M440 254[^"]*"/>', '<path fill-rule="evenodd" d="M440 238 A72 72 0 1 1 440 382 A72 72 0 1 1 440 238 Z M440 276 A34 34 0 1 0 440 344 A34 34 0 1 0 440 276 Z"/>', s)
    s = s.replace('<rect x="652" y="458" width="81" height="84" rx="19"/>', '<rect x="628" y="436" width="120" height="124" rx="26"/>')
    s = s.replace('<circle cx="425" cy="935" r="52"/>', '<circle cx="425" cy="935" r="72"/>')
    s = s.replace('viewBox="300 205 640 825"', 'viewBox="150 150 940 940"').replace('currentColor', color)
    # A thin dark halo under the glyph keeps it readable on a light panel (the ivory idle glyph would vanish there).
    halo = ('<defs><filter id="halo" x="-10%" y="-10%" width="120%" height="120%"><feMorphology in="SourceAlpha" operator="dilate" radius="30" result="d"/>'
            '<feFlood flood-color="#1c1c20" flood-opacity="0.7"/><feComposite in2="d" operator="in" result="h"/>'
            '<feMerge><feMergeNode in="h"/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs>')
    return s.replace('<g fill="none"', halo + '<g filter="url(#halo)"><g fill="none"', 1).replace('</svg>', '</g></svg>')


def write_ico(path: Path, imgs):
    blobs = []
    for im in imgs:
        b = io.BytesIO()
        im.save(b, "PNG")
        blobs.append(b.getvalue())
    out = struct.pack("<HHH", 0, 1, len(imgs))
    offset = 6 + 16 * len(imgs)
    for im, blob in zip(imgs, blobs):
        out += struct.pack("<BBBBHHII", im.width % 256, im.height % 256, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    path.write_bytes(out + b"".join(blobs))


def mac_tile(big: Image.Image, size: int) -> Image.Image:
    """The icon inside macOS's safe area (824 of 1024) with the usual soft shadow underneath."""
    canvas = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    tile = big.resize((824, 824), Image.LANCZOS)
    alpha = tile.getchannel("A")
    shadow = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 90), (100, 112), alpha)
    canvas = Image.alpha_composite(canvas, shadow.filter(ImageFilter.GaussianBlur(14)))
    canvas.alpha_composite(tile, (100, 100))
    return canvas.resize((size, size), Image.LANCZOS)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    big = Image.open(SRC / "PeriWire-1024.png").convert("RGBA")
    icon_svg = (SRC / "PeriWire-icon.svg").read_text()
    mark_svg = (SRC / "PeriWire-mark.svg").read_text()

    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=find_browser(), args=["--no-sandbox"])

        def render(svg: str, size: int) -> Image.Image:
            svg = re.sub(r'width="1024" height="1024"', f'width="{size}" height="{size}"', svg, count=1)
            page = browser.new_page(viewport={"width": size, "height": size})
            page.set_content(f'<html><body style="margin:0;background:transparent">{svg}</body></html>')
            png = page.screenshot(omit_background=True)
            page.close()
            return Image.open(io.BytesIO(png)).convert("RGBA")

        sturdy = sturdy_icon(icon_svg)
        small = {n: render(sturdy, n) for n in (16, 24, 32, 48, 64)}
        trays = {name: render(tray_mark(mark_svg, color), 64) for name, color in TRAY_COLORS.items()}
        browser.close()

    def large(n):
        return big.resize((n, n), Image.LANCZOS)

    files = {"32x32.png": small[32], "48x48.png": small[48], "64x64.png": small[64], "128x128.png": large(128), "128x128@2x.png": large(256),
             "256x256.png": large(256), "512x512.png": large(512), "icon.png": large(512)}
    for name, im in files.items():
        im.save(OUT / name)
    for name, im in trays.items():
        im.save(OUT / name)
    write_ico(OUT / "icon.ico", [small[16], small[24], small[32], small[48], small[64], large(128), large(256)])
    sizes = [16, 32, 64, 128, 256, 512, 1024]
    mac = {n: mac_tile(big, n) for n in sizes}
    mac[1024].save(OUT / "icon.icns", format="ICNS", append_images=[mac[n] for n in sizes[:-1]])
    large(256).save(UI / "logo.png")
    for f in sorted(OUT.iterdir()):
        print(f"{f.relative_to(ROOT)}  {f.stat().st_size // 1024 or 1} KiB")
    print(f"{(UI / 'logo.png').relative_to(ROOT)}")


if __name__ == "__main__":
    main()
