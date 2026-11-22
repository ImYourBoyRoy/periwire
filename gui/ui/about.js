// The About page: version, updates and the terminal command, plus the sidebar's version line and the "update available" banner.
// app.js hands over its helpers (no import cycle); updates go through the daemon (`update.check`) and the app shell (`self_update`, which runs
// `periwire self-update`: it checks the download's SHA-256 before replacing anything, then the app starts again from the new files).
import { call, on, demo } from "./api.js";

const SIX_HOURS = 6 * 3600 * 1000;
const T = window.__TAURI__;
const REPO = "https://github.com/ImYourBoyRoy/periwire";

/** Talk to the app shell; in the browser demo the answers are made up. */
async function native(cmd, args) {
  if (!demo) return T.core.invoke(cmd, args);
  await new Promise((r) => setTimeout(r, 500));
  if (cmd === "app_version") return "0.1.1";
  if (cmd === "update_support") return JSON.stringify({ kind: new URLSearchParams(location.search).has("package") ? "package" : "folder", supported: !new URLSearchParams(location.search).has("package"), reason: "this copy belongs to a package: update it with your package manager, or install the new .deb from the release page", replaces: [] });
  if (cmd === "cli_on_path") return null;
  if (cmd === "terminal_setup") return "(demo) nothing changed";
  if (cmd === "self_update") return "updated periwire 0.1.1 -> 0.2.0 (demo: nothing changed)";
  return null;
}

export function initAbout({ S, h, $, esc, modal, toast, setText, vEls, go, ic }) {
  const st = { app: null, update: null, checkedAt: null, phase: "idle", error: "", support: null, cli: undefined, setUp: false, dismissed: null };
  const daemonVersion = () => S.status?.version || null;
  const page = vEls.about;

  // ------------------------------------------------------------------ the sidebar line and the banner
  const side = h("button", { class: "ver-card", id: "ver", title: "About PeriWire and updates", "aria-label": "About PeriWire and updates", onclick: () => go("about") },
    h("b", { id: "ver-name" }, "PeriWire"), h("span", { id: "ver-state" }, ""));
  $(".side").append(side);
  const banner = h("div", { class: "update-banner", id: "update-banner", role: "status", hidden: "" });
  $(".main").insertBefore(banner, $(".main .top").nextSibling);

  const openRelease = async (url) => {
    if (demo) { window.open(url, "_blank", "noopener"); return; }
    try { await T.core.invoke("open_url", { url }); } catch (e) { toast(`Could not open the browser: ${e.message || e}. ${url}`, "bad", 9000); }
  };

  // ------------------------------------------------------------------ the page
  const el = (id) => page.querySelector("#" + id);
  page.innerHTML = `
    <div class="card about-hero">
      <img src="logo.png" alt="" width="68" height="68">
      <div class="about-id"><h2>PeriWire</h2><div class="about-ver" id="ab-ver">Version …</div><div class="about-meta" id="ab-meta"></div></div>
      <div class="about-hero-side"><span class="chip" id="ab-chip"><i class="dot"></i><span id="ab-chip-t"></span></span><button class="btn primary" id="ab-main"></button></div>
    </div>
    <div class="about-warn" id="ab-mismatch" hidden></div>
    <div class="section-title">Updates<span class="line"></span></div>
    <div class="card about-card">
      <div class="about-row"><div class="ico">${ic("down")}</div><div class="grow"><b id="ab-up-title"></b><div class="muted" id="ab-up-sub"></div></div>
        <div class="about-actions"><button class="btn" id="ab-check">Check now</button><button class="btn ghost" id="ab-notes">What's new</button></div></div>
      <div class="about-progress" id="ab-progress" hidden><div class="bar"><i></i></div><div class="muted" id="ab-progress-t"></div></div>
      <div class="about-error" id="ab-error" hidden></div>
      <div class="about-row"><div class="ico">${ic("clock")}</div><div class="grow"><b>Check automatically</b><div class="muted">One request to github.com every few hours, nothing else is sent. Updates are never installed without you.</div></div>
        <div class="switch" id="ab-auto" role="switch" tabindex="0" aria-label="Check for updates automatically"></div></div>
    </div>
    <div class="section-title">Terminal command<span class="line"></span></div>
    <div class="card about-card">
      <div class="about-row"><div class="ico">${ic("terminal")}</div><div class="grow"><b id="ab-cli-title"></b><div class="muted" id="ab-cli-sub"></div></div>
        <div class="about-actions"><button class="btn" id="ab-cli-set">Set up</button><button class="btn ghost" id="ab-cli-undo">Undo</button></div></div>
    </div>
    <div class="section-title">Help<span class="line"></span></div>
    <div class="about-links">
      <button class="btn" data-url="${REPO}/blob/main/docs/GETTING-STARTED.md">Getting started</button>
      <button class="btn" data-url="${REPO}/releases">All releases</button>
      <button class="btn" data-url="${REPO}/blob/main/CHANGELOG.md">Changelog</button>
      <button class="btn" data-url="${REPO}/issues/new/choose">Report a problem</button>
      <span class="faint about-lic">MIT licence · ImYourBoyRoy</span>
    </div>`;
  page.querySelectorAll("[data-url]").forEach((b) => b.addEventListener("click", () => openRelease(b.dataset.url)));

  const when = (t) => (t ? new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : null);

  function render() {
    const u = st.update, app = st.app, d = daemonVersion(), v = d || app || "";
    const newer = !!(u && u.newer), supported = st.support ? st.support.supported : true, busy = st.phase === "checking" || st.phase === "installing";
    // sidebar line
    setText($("#ver-name"), v ? `PeriWire v${v}` : "PeriWire");
    const side$ = $("#ver-state");
    setText(side$, st.phase === "installing" ? "Installing update…" : newer ? `Update ${u.latest} available` : st.phase === "checking" ? "Checking…" : u ? "Up to date" : "");
    side.classList.toggle("has-update", newer);
    // nav badge
    const nb = $("#b-about"); const nbt = newer ? "1" : ""; if (nb && nb.textContent !== nbt) nb.textContent = nbt;
    // hero
    setText(el("ab-ver"), v ? `Version ${v}` : "Version …");
    setText(el("ab-meta"), `Desktop app ${app || "?"} · daemon ${d || "not running"}`);
    const chip = el("ab-chip"), chipT = el("ab-chip-t");
    const tone = st.phase === "installing" ? ["acc live", "Installing…"] : st.phase === "checking" ? ["live", "Checking…"] : newer ? ["acc", `${u.latest} available`] : st.error && !u ? ["warn", "Could not check"] : u ? ["ok", "Up to date"] : ["", "Not checked yet"];
    chip.className = `chip ${tone[0]}`; setText(chipT, tone[1]);
    const main = el("ab-main");
    main.disabled = busy;
    main.className = "btn primary";
    setText(main, st.phase === "installing" ? "Installing…" : st.phase === "checking" ? "Checking…" : newer ? (supported ? `Install ${u.latest}` : "Open download page") : "Check for updates");
    // mismatch
    const mm = el("ab-mismatch"), mismatch = !!(app && d && app !== d);
    mm.hidden = !mismatch;
    const mmKey = mismatch ? `${d}|${app}` : "";
    if (mismatch && mm._key !== mmKey) { mm._key = mmKey; mm.innerHTML = `<b>The daemon is still ${esc(d)}.</b> This app is ${esc(app)}: restart the daemon to finish updating (tray menu: <i>Quit and stop the daemon</i>, then start it again; or <code>periwire stop</code>).`; }
    // updates card
    setText(el("ab-up-title"), st.phase === "installing" ? "Installing the update…" : newer ? `PeriWire ${u.latest} is available` : u ? "You're up to date" : st.error ? "Could not check for updates" : "Checking for updates…");
    const how = !st.support ? "" : st.support.supported ? (st.support.kind === "appimage" ? "Updates replace this AppImage." : "Updates replace the program files here.") : st.support.reason;
    setText(el("ab-up-sub"), [u ? `You have ${u.current}${newer ? `, the latest is ${u.latest}` : ""}.` : "", when(st.checkedAt) ? `Last checked ${when(st.checkedAt)}.` : "", how].filter(Boolean).join(" "));
    el("ab-check").disabled = busy; el("ab-notes").hidden = !u; el("ab-notes").textContent = newer ? "What's new" : "Release notes";
    el("ab-progress").hidden = st.phase !== "installing";
    const er = el("ab-error"); er.hidden = !st.error || st.phase === "installing"; if (!er.hidden) er.textContent = st.error;
    const auto = el("ab-auto"); auto.classList.toggle("on", S.status?.update_check !== false); auto.setAttribute("aria-checked", String(S.status?.update_check !== false));
    // terminal
    const on = st.setUp || !!st.cli;
    setText(el("ab-cli-title"), on ? "periwire works in your terminals" : "periwire is not in your terminals yet");
    setText(el("ab-cli-sub"), st.setUp ? "Set up. Open a new terminal (PowerShell, cmd, bash, zsh or fish) to use it." : st.cli ? `Found on your PATH: ${st.cli}` : "Adds the command to your PATH (a link in ~/.local/bin and a marked block in your shell profiles; on Windows your user PATH). Reversible.");
    el("ab-cli-set").hidden = on && !!st.cli; el("ab-cli-undo").hidden = !st.setUp && !st.cli;
  }

  async function check(quiet) {
    if (st.phase === "checking" || st.phase === "installing") return st.update;
    st.phase = "checking"; st.error = ""; render();
    try { st.update = await call("update.check"); st.checkedAt = Date.now(); st.phase = "idle"; }
    catch (e) { st.phase = "idle"; if (!st.update) st.error = e.message || String(e); if (!quiet) toast(`Could not check: ${e.message || e}`, "bad", 7000); }
    render(); return st.update;
  }

  async function install() {
    const u = st.update; if (!u?.newer) return;
    if (st.support && !st.support.supported) { openRelease(u.url); return; }
    const ok = await modal({ title: `Install PeriWire ${u.latest}?`, body: `PeriWire downloads the release, checks its SHA-256 and replaces its program files. The app then <b>restarts</b>. A daemon this app started restarts with it; a daemon started another way (a service, the terminal) keeps running the old version until you restart it.`,
      actions: [{ label: "Not now", value: false }, { label: `Install ${u.latest}`, value: true, kind: "primary" }] });
    if (!ok) return;
    st.phase = "installing"; st.error = ""; render();
    setText(el("ab-progress-t"), "Downloading, checking the SHA-256 and replacing the program files…");
    try {
      const text = await native("self_update");
      toast(text.split("\n").find((l) => l.startsWith("updated")) || "Updated", "ok", 8000);
      setText(el("ab-progress-t"), "Updated. PeriWire is restarting…");
    } catch (e) { st.phase = "idle"; st.error = e.message || String(e); render(); }
  }

  async function terminal(undo) {
    const b = el(undo ? "ab-cli-undo" : "ab-cli-set"); b.disabled = true;
    try { const text = await native("terminal_setup", { undo }); st.setUp = !undo; if (undo) st.cli = null; toast(text.split("\n").slice(0, 3).join(" · "), "ok", 9000); }
    catch (e) { toast(`${e.message || e}`, "bad", 9000); }
    b.disabled = false; render();
  }

  el("ab-main").addEventListener("click", () => { const u = st.update; if (u?.newer) install(); else check(false); });
  el("ab-check").addEventListener("click", () => check(false));
  el("ab-notes").addEventListener("click", () => openRelease(st.update?.url || `${REPO}/releases`));
  el("ab-cli-set").addEventListener("click", () => terminal(false));
  el("ab-cli-undo").addEventListener("click", () => terminal(true));
  el("ab-auto").addEventListener("click", async (e) => {
    const next = S.status?.update_check === false;
    try { await call("update.set", { enabled: next }); if (S.status) S.status.update_check = next; render(); } catch (err) { toast(err.message || String(err), "bad"); }
  });

  // banner on every page
  let drawn = "";
  function paintBanner() {
    const u = st.update; const show = !!(u?.newer && st.dismissed !== u.latest && S.view !== "about" && st.phase !== "installing");
    const key = show ? `${u.current}>${u.latest}` : "";
    if (key === drawn) return; drawn = key;
    banner.hidden = !show; if (!show) { banner.replaceChildren(); return; }
    banner.replaceChildren(h("span", {}, `PeriWire ${u.latest} is available (you have ${u.current}).`),
      h("button", { class: "btn primary sm", onclick: () => go("about") }, "Review and install"),
      h("button", { class: "btn ghost sm", onclick: () => { st.dismissed = u.latest; drawn = ""; paintBanner(); }, "aria-label": "Dismiss" }, "Not now"));
  }

  const refresh = () => { render(); paintBanner(); };
  let first = true;
  on("status", (s) => { refresh(); if (first && s) { first = false; if (s.update_check !== false) check(true); } });
  document.addEventListener("periwire:view", refresh);
  native("app_version").then((v) => { st.app = v; refresh(); }).catch(() => {});
  native("update_support").then((t) => { st.support = JSON.parse(t); refresh(); }).catch(() => {});
  native("cli_on_path").then((p) => { st.cli = p; refresh(); }).catch(() => {});
  setInterval(() => { if (S.connected && S.status?.update_check !== false) check(true); }, SIX_HOURS);
  refresh();
  return { check, install };
}
