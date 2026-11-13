#!/usr/bin/env python3
"""Measure how much of the desktop UI is torn down and rebuilt while it sits idle (demo mode, status updates every ~2 s).

A healthy UI changes text for live numbers but does not replace its elements: rebuilt buttons can't be clicked reliably, selected text
disappears and icons flicker. Developer/CI check, not part of the product. Needs: pip install playwright (and a Chrome/Chromium: CHROME=/path).

    python3 tools/ui-churn.py             # print the churn per view
    python3 tools/ui-churn.py --check     # exit 1 if a view that has no live traffic rebuilds anything (used in CI)
"""
import functools, http.server, os, shutil, sys, threading
from pathlib import Path
from playwright.sync_api import sync_playwright

UI = Path(__file__).resolve().parents[1] / "gui" / "ui"
SECONDS = 12
# views whose content does not depend on traffic counters: they must not rebuild at all while idle
QUIET = {"Share", "Trust", "Health", "Activity"}
JS = """
(() => {
  window.__m = {};
  const name = (n) => { let e = n.nodeType === 1 ? n : n.parentElement; const p = []; while (e && p.length < 3 && e.id !== 'views') { p.unshift((e.id ? '#'+e.id : '')+(e.className && typeof e.className==='string' ? '.'+e.className.split(' ').slice(0,2).join('.') : e.tagName.toLowerCase())); e = e.parentElement; } return p.join(' > '); };
  new MutationObserver((ms) => { for (const m of ms) { if (m.type !== 'childList') continue; const k = name(m.target); const v = window.__m[k] || (window.__m[k] = 0); window.__m[k] = v + m.addedNodes.length + m.removedNodes.length; } }).observe(document.body, {subtree:true, childList:true});
})()
"""


def browser_path():
    if os.environ.get("CHROME"):
        return os.environ["CHROME"]
    for n in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser"):
        if shutil.which(n):
            return shutil.which(n)
    return None


def main():
    check = "--check" in sys.argv

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *a, **k):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Quiet, directory=str(UI)))
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    bad = []
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=browser_path(), args=["--no-sandbox"])
        for view in ("Overview", "Share", "Connect", "Trust", "Health", "Activity"):
            pg = b.new_page(viewport={"width": 1280, "height": 820})
            pg.goto(base + "/?demo")
            pg.wait_for_selector("text=Studio YubiKey")
            if view != "Overview":
                pg.locator("#nav").get_by_text(view, exact=True).first.click()
            pg.wait_for_timeout(1500)
            pg.evaluate(JS)
            pg.wait_for_timeout(SECONDS * 1000)
            m = pg.evaluate("window.__m")
            total = sum(m.values())
            print(f"{view:9} {total:4d} nodes added+removed in {SECONDS} s" + ("" if not m else "   " + ", ".join(f"{k.split(' > ')[-1]}:{v}" for k, v in sorted(m.items(), key=lambda kv: -kv[1])[:3])))
            if view in QUIET and total:
                bad.append(view)
            pg.close()
        b.close()
    srv.shutdown()
    if check and bad:
        print("FAIL: these views rebuilt their content while idle:", ", ".join(bad))
        sys.exit(1)


if __name__ == "__main__":
    main()
