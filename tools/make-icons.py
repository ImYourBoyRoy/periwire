#!/usr/bin/env python3
"""Render the app/tray icons (no external assets). Output: gui/src-tauri/icons/*.png|ico, gui/ui/logo.svg."""
import math, os
from PIL import Image, ImageDraw, ImageFilter

OUT = os.path.join(os.path.dirname(__file__), "..", "gui", "src-tauri", "icons")
S = 1024

def glyph(d, cx, color, scale=1.0, ox=0, oy=0, w=46):
    """USB trident symbol centred at x=cx."""
    def P(x, y): return (cx + (x - 512) * scale + ox, (y - 512) * scale + 512 + oy)
    # stem
    d.line([P(512, 250), P(512, 790)], fill=color, width=int(w * scale))
    # bottom circle
    r = 70 * scale; x, y = P(512, 810)
    d.ellipse([x - r, y - r, x + r, y + r], fill=color)
    # arrow head on top
    d.polygon([P(512, 170), P(452, 262), P(572, 262)], fill=color)
    # left branch -> circle
    d.line([P(512, 600), P(352, 600), P(352, 470)], fill=color, width=int(w * scale), joint="curve")
    x, y = P(352, 440); r = 52 * scale
    d.ellipse([x - r, y - r, x + r, y + r], outline=color, width=int(30 * scale))
    # right branch -> square
    d.line([P(512, 520), P(672, 520), P(672, 400)], fill=color, width=int(w * scale), joint="curve")
    x, y = P(672, 372); r = 50 * scale
    d.rectangle([x - r, y - r, x + r, y + r], fill=color)

def app_icon():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # gradient squircle
    grad = Image.new("RGBA", (S, S))
    px = grad.load()
    for y in range(S):
        for x in range(S):
            t = (x * 0.35 + y * 0.65) / S
            r = int(201 + (224 - 201) * t); g = int(119 + (160 - 119) * t); b = int(75 + (99 - 75) * t)
            px[x, y] = (r, g, b, 255)
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([40, 40, S - 40, S - 40], radius=230, fill=255)
    img.paste(grad, (0, 0), mask)
    # soft highlight
    hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(hl).ellipse([-200, -420, S + 200, 420], fill=(255, 255, 255, 38))
    hl = hl.filter(ImageFilter.GaussianBlur(40))
    img = Image.alpha_composite(img, Image.composite(hl, Image.new("RGBA", (S, S), (0, 0, 0, 0)), mask))
    g = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    glyph(ImageDraw.Draw(g), 512, (255, 255, 255, 255), scale=0.82, oy=-10)
    shadow = g.filter(ImageFilter.GaussianBlur(18)); shadow = Image.eval(shadow, lambda v: v // 3)
    img = Image.alpha_composite(img, Image.new("RGBA", (S, S), (0, 0, 0, 0)))
    img = Image.alpha_composite(img, shadow.point(lambda v: v))
    img = Image.alpha_composite(img, g)
    return img

def tray_icon(size=64, color=(255, 255, 255, 255)):
    g = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    glyph(ImageDraw.Draw(g), 512, color, scale=1.05, w=70)
    return g.resize((size, size), Image.LANCZOS)

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    a = app_icon()
    for name, sz in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
        a.resize((sz, sz), Image.LANCZOS).save(os.path.join(OUT, name))
    a.save(os.path.join(OUT, "icon.ico"), sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    for name, col in [("tray.png", (240, 235, 228, 255)), ("tray-active.png", (74, 222, 128, 255)), ("tray-warn.png", (250, 204, 21, 255))]:
        tray_icon(64, col).save(os.path.join(OUT, name))
    print("icons written to", os.path.abspath(OUT))
