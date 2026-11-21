// Version, "what is new" and the update check. app.js hands over its helpers (no import cycle); everything here talks to the daemon through
// the same `call` the rest of the page uses. Nothing is installed from here: it tells you a newer release exists and opens its page.
import { call, on, demo } from "./api.js";

const SIX_HOURS = 6 * 3600 * 1000;
const T = window.__TAURI__;

export function initAbout({ S, h, $, esc, modal, toast, setText }) {
  const st = { app: null, update: null, checking: false, dismissed: null };
  const daemonVersion = () => S.status?.version || null;

  // The version, under the daemon chip: it opens the About dialog.
  const ver = h("button", { class: "ver", id: "ver", title: "About PeriWire", "aria-label": "About PeriWire", onclick: () => about() }, "");
  $(".side").append(ver);
  // The banner when a newer release exists.
  const banner = h("div", { class: "update-banner", id: "update-banner", role: "status", hidden: "" });
  $(".main").insertBefore(banner, $(".main .top").nextSibling);

  const appVersion = async () => {
    if (st.app) return st.app;
    try { st.app = demo ? "0.1.1" : await T.core.invoke("app_version"); } catch { st.app = null; }
    return st.app;
  };
  const openRelease = async (url) => {
    if (demo) { window.open(url, "_blank", "noopener"); return; }
    try { await T.core.invoke("open_url", { url }); } catch (e) { toast(`Could not open the browser: ${e.message || e}. ${url}`, "bad", 9000); }
  };
  let drawn = "";
  const paint = () => {
    const v = daemonVersion() || st.app;
    setText(ver, v ? `PeriWire v${v}` : "PeriWire");
    const u = st.update;
    const show = !!(u && u.newer && st.dismissed !== u.latest);
    const key = show ? `${u.current}>${u.latest}` : "";
    if (key === drawn) return; // the status arrives every couple of seconds: leave the banner (and a click on it) alone unless it changed
    drawn = key;
    banner.hidden = !show;
    if (!show) { banner.replaceChildren(); return; }
    banner.replaceChildren(
      h("span", {}, `PeriWire ${u.latest} is available (you have ${u.current}). `),
      h("button", { class: "btn", onclick: () => openRelease(u.url) }, "What's new and download"),
      h("button", { class: "btn ghost", onclick: () => { st.dismissed = u.latest; paint(); }, "aria-label": "Dismiss" }, "Not now"));
  };

  /** Ask the daemon (which asks GitHub). `quiet` hides failures: the automatic check must never nag. */
  const check = async (quiet) => {
    if (st.checking) return st.update;
    st.checking = true;
    try { st.update = await call("update.check"); paint(); return st.update; }
    catch (e) { if (!quiet) throw e; return null; }
    finally { st.checking = false; }
  };

  async function terminalSetup(btn, undo) {
    btn.disabled = true;
    try {
      const text = demo ? "(demo) nothing changed" : await T.core.invoke("terminal_setup", { undo });
      toast(text.split("\n").slice(0, 4).join(" · "), "ok", 9000);
    } catch (e) { toast(`${e.message || e}`, "bad", 9000); }
    btn.disabled = false;
  }

  async function about() {
    const app = await appVersion(), d = daemonVersion();
    const body = h("div", { class: "about" });
    const render = () => {
      const u = st.update, mismatch = app && d && app !== d;
      body.replaceChildren(
        h("p", { html: `<b>PeriWire ${esc(d || app || "")}</b>` }),
        h("p", { class: "muted", html: `Desktop app ${esc(app || "?")} · daemon ${esc(d || "not running")}` }),
        mismatch ? h("p", { class: "warn-text" }, "The app and the daemon are different versions. Restart the daemon (periwire stop, then start it again) to finish updating.") : "",
        h("p", {}, u ? (u.newer ? `Version ${u.latest} is available.` : "You have the latest release.") : "Not checked yet."),
        h("label", { class: "check" },
          h("input", { type: "checkbox", id: "auto-update", checked: S.status?.update_check !== false ? "" : null, onchange: async (e) => {
            try { await call("update.set", { enabled: e.target.checked }); } catch (err) { e.target.checked = !e.target.checked; toast(err.message, "bad"); }
          } }), " Check for updates automatically (one request to github.com now and then)"),
        h("p", { class: "muted", html: "The <code>periwire</code> command in your terminals: a link in <code>~/.local/bin</code> and a PATH block in your shell profiles (Linux, macOS), or your user PATH (Windows). Reversible." }),
        h("div", { class: "row" },
          h("button", { class: "btn", id: "terminal-setup", onclick: async (e) => terminalSetup(e.target, false) }, "Set up the periwire command"),
          h("button", { class: "btn ghost", id: "terminal-undo", onclick: async (e) => terminalSetup(e.target, true) }, "Undo")),
        h("div", { class: "row" },
          h("button", { class: "btn", id: "check-now", onclick: async (e) => {
            e.target.disabled = true; e.target.textContent = "Checking…";
            try { await check(false); } catch (err) { toast(`Could not check: ${err.message}`, "bad", 7000); }
            render();
          } }, "Check now"),
          u?.newer ? h("button", { class: "btn primary", onclick: () => openRelease(u.url) }, "What's new and download") : ""));
    };
    render();
    await modal({ title: "About PeriWire", body, actions: [{ label: "Close", value: true, kind: "primary" }] });
  }

  let first = true;
  on("status", (s) => {
    paint();
    if (first && s) { first = false; if (s.update_check !== false) check(true); }
  });
  appVersion().then(paint);
  setInterval(() => { if (S.connected && S.status?.update_check !== false) check(true); }, SIX_HOURS);
  paint();
  return { check, about };
}
