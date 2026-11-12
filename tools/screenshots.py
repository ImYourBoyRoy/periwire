#!/usr/bin/env python3
"""Regenerate the WebP screenshots in docs/img/ from the app's built-in demo mode (simulated devices, no daemon needed).

Developer tool, not part of the product. Needs:  pip install playwright pillow
and a Chrome or Chromium: set CHROME=/path/to/browser, or let it find google-chrome / chromium on PATH, or run `playwright install chromium`.

    python3 tools/screenshots.py            # write all images
    python3 tools/screenshots.py overview   # only images whose name contains "overview"

The UI is served from gui/ui on a throwaway local port; nothing else is touched. Re-run it after UI changes and commit the images.
"""
import functools, http.server, io, os, shutil, sys, threading
from pathlib import Path

from PIL import Image
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
UI = ROOT / "gui" / "ui"
OUT = ROOT / "docs" / "img"
THEMES = ["clay", "moss", "plum", "linen", "rose", "lagoon"]
DESKTOP = {"width": 1280, "height": 820}
SCALE = 1.5          # sharper images when the README shows them smaller than their pixel size
QUALITY = 86


def find_browser():
    if os.environ.get("CHROME"):
        return os.environ["CHROME"]
    for name in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser"):
        p = shutil.which(name)
        if p:
            return p
    return None  # playwright's own chromium


def serve():
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args, **kwargs):
            pass

    handler = functools.partial(Quiet, directory=str(UI))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}"


def to_webp(png: bytes, path: Path, size=None):
    im = Image.open(io.BytesIO(png)).convert("RGB")
    if size:
        im = im.resize(size, Image.LANCZOS)
    path.parent.mkdir(parents=True, exist_ok=True)
    im.save(path, "WEBP", quality=QUALITY, method=6)
    print(f"{path.relative_to(ROOT)}  {im.width}x{im.height}  {path.stat().st_size // 1024} KiB")
    return im


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else ""
    want = lambda n: only in n
    srv, base = serve()
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=find_browser(), args=["--no-sandbox"])

        def page(theme, viewport=DESKTOP, query="?demo", scale=SCALE):
            ctx = browser.new_context(viewport=viewport, device_scale_factor=scale)
            ctx.add_init_script(f"try{{localStorage.setItem('periwire.theme','{theme}')}}catch(e){{}}")
            pg = ctx.new_page()
            pg.goto(base + "/" + query)
            pg.wait_for_selector("#nav button, #nav a", timeout=10000)
            pg.wait_for_selector("text=Daemon running", timeout=10000)      # the demo daemon answered
            pg.wait_for_selector("text=Studio YubiKey", timeout=10000)      # and its connections are drawn
            pg.wait_for_timeout(900)
            return ctx, pg

        def goto(pg, label):
            pg.locator("#nav").get_by_text(label, exact=True).first.click()
            pg.wait_for_timeout(700)

        # one image per view, dark and light
        views = [("overview", "Overview", "clay", "linen"), ("share", "Share", "moss", "lagoon"), ("connect", "Connect", "clay", "rose"),
                 ("trust", "Trust", "plum", "linen"), ("health", "Health", "moss", "lagoon"), ("activity", "Activity", "plum", "rose")]
        for name, label, dark, light in views:
            for mode, theme in (("dark", dark), ("light", light)):
                n = f"{name}-{mode}"
                if not want(n):
                    continue
                ctx, pg = page(theme)
                if label != "Overview":
                    goto(pg, label)
                to_webp(pg.screenshot(), OUT / f"{n}.webp")
                ctx.close()

        if want("help"):
            ctx, pg = page("clay")
            pg.locator("#help-btn").click()
            pg.wait_for_timeout(900)
            to_webp(pg.screenshot(), OUT / "help-dark.webp")
            ctx.close()

        if want("windows"):
            ctx, pg = page("linen", query="?demo&win")
            goto(pg, "Connect")
            to_webp(pg.screenshot(), OUT / "windows-driver-light.webp")
            ctx.close()

        if want("mobile"):
            ctx, pg = page("clay", viewport={"width": 390, "height": 800}, scale=2)
            to_webp(pg.screenshot(), OUT / "mobile-dark.webp")
            ctx.close()

        if want("themes"):
            tiles = []
            for t in THEMES:
                ctx, pg = page(t, scale=1)
                tiles.append(Image.open(io.BytesIO(pg.screenshot())).convert("RGB").resize((640, 410), Image.LANCZOS))
                ctx.close()
            grid = Image.new("RGB", (640 * 3 + 8 * 2, 410 * 2 + 8), (128, 128, 128))
            for i, im in enumerate(tiles):
                grid.paste(im, ((i % 3) * 648, (i // 3) * 418))
            buf = io.BytesIO()
            grid.save(buf, "PNG")
            to_webp(buf.getvalue(), OUT / "themes.webp")

        browser.close()
    srv.shutdown()


if __name__ == "__main__":
    main()
