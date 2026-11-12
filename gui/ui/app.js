import { call, on, start, startDaemon, demo } from "./api.js";
import { openPicker } from "./theme.js";
import { PAGES, BASICS, GLOSSARY, EXPERT } from "./help.js";

// ------------------------------------------------------------------ helpers
const $ = (s, r = document) => r.querySelector(s);
const h = (tag, props = {}, ...kids) => {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k === "class") e.className = v; else if (k === "html") e.innerHTML = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v); else if (v !== false && v != null) e.setAttribute(k, v === true ? "" : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) e.append(k.nodeType ? k : document.createTextNode(k));
  return e;
};
const P = {
  grid: '<rect x="3" y="3" width="7" height="7" rx="1.6"/><rect x="14" y="3" width="7" height="7" rx="1.6"/><rect x="3" y="14" width="7" height="7" rx="1.6"/><rect x="14" y="14" width="7" height="7" rx="1.6"/>',
  share: '<path d="M12 16V4"/><path d="m7 9 5-5 5 5"/><path d="M5 20h14"/>',
  connect: '<path d="M12 4v12"/><path d="m7 11 5 5 5-5"/><path d="M5 20h14"/>',
  shield: '<path d="M12 3 5 6v5c0 4.5 3 8 7 10 4-2 7-5.5 7-10V6l-7-3z"/><path d="m9 12 2 2 4-4"/>',
  pulse: '<path d="M3 12h4l2-6 4 12 2-6h6"/>',
  terminal: '<path d="m4 7 5 5-5 5"/><path d="M12 18h8"/>',
  storage: '<path d="M7 9h10v11a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2z"/><path d="M9 9V3h6v6"/><path d="M11 5.5h.01M13 5.5h.01"/>',
  imaging: '<path d="M4 8h3l2-3h6l2 3h3v11H4z"/><circle cx="12" cy="13" r="3.5"/>',
  tablet: '<rect x="5" y="2.5" width="14" height="19" rx="2.5"/><path d="M11 18.5h2"/>',
  video: '<circle cx="12" cy="10" r="6"/><circle cx="12" cy="10" r="2"/><path d="M8 21h8M12 16v5"/>',
  hid: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  audio: '<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>',
  printer: '<path d="M7 9V3h10v6"/><rect x="3" y="9" width="18" height="8" rx="2"/><path d="M7 14h10v7H7z"/>',
  wireless: '<path d="M2 9a15 15 0 0 1 20 0"/><path d="M5 12.5a10 10 0 0 1 14 0"/><path d="M8.5 16a5 5 0 0 1 7 0"/><path d="M12 19.5h.01"/>',
  chip: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>',
  serial: '<path d="M4 7h16M4 12h16M4 17h10"/>',
  warn: '<path d="M12 3 2 20h20z"/><path d="M12 10v5M12 18h.01"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  refresh: '<path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 4v7h-7"/>',
  eject: '<path d="M12 5 4.5 15h15z"/><path d="M5 19h14"/>',
  plug: '<path d="M9 2v6M15 2v6"/><path d="M6 8h12v4a6 6 0 0 1-12 0z"/><path d="M12 18v4"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  zap: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
  down: '<path d="M12 5v12M6 13l6 6 6-6"/>',
  phone: '<rect x="7" y="2.5" width="10" height="19" rx="2.5"/><path d="M11 18.5h2"/>',
  key: '<circle cx="8" cy="15" r="4"/><path d="m11 12 9-9M16 7l3 3M14 9l2 2"/>',
  mouse: '<rect x="6" y="3" width="12" height="18" rx="6"/><path d="M12 7v4"/>',
  wrench: '<path d="M14.5 6.5a4 4 0 0 0-5 5L3.5 17.5l3 3 6-6a4 4 0 0 0 5-5l-2.5 2.5-2.5-.5-.5-2.5z"/>',
  bluetooth: '<path d="m7 7 10 10-5 4V3l5 4L7 17"/>',
  network: '<rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="14" width="18" height="6" rx="2"/><path d="M7 7.5h.01M7 17h.01"/>',
  pencil: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="m13.5 6.5 4 4"/>',
  chev: '<path d="m6 9 6 6 6-6"/>',
};
const ic = (n) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${P[n] || P.chip}</svg>`;
const bytes = (n) => { const u = ["B", "KB", "MB", "GB", "TB"]; let i = 0, v = n || 0; while (v >= 1024 && i < 4) { v /= 1024; i++; } return i ? `${v.toFixed(v < 10 ? 2 : 1)} ${u[i]}` : `${v} B`; };
const rate = (bps) => (bps < 1 ? "idle" : `${bytes(bps)}/s`);
const dur = (s) => { s = Math.max(0, Math.floor(s)); return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m ${s % 60}s` : s < 86400 ? `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m` : `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`; };
const nowS = () => Math.floor(Date.now() / 1000);
const short = (fp) => (fp && fp.length > 16 ? `${fp.slice(0, 8)}…${fp.slice(-8)}` : fp || "");
const devName = (d) => { if (d.display) return d.display; const m = d.manufacturer || "", p = d.product || ""; if (!m && !p) return d.kind_label || `USB device ${d.vid}:${d.pid}`; if (!p) return m; if (!m || p.toLowerCase().includes(m.toLowerCase())) return p; return `${m} ${p}`; };
const KIND_ICON = { "apple-ipad": "tablet", "apple-iphone": "phone", "apple-dfu": "wrench", "apple-recovery": "wrench", android: "phone", "android-fastboot": "wrench", storage: "storage", "security-key": "key", keyboard: "hid", mouse: "mouse", hid: "hid", webcam: "video", camera: "imaging", audio: "audio", printer: "printer", bluetooth: "bluetooth", serial: "serial", network: "network", other: "chip" };
const iconFor = (d) => KIND_ICON[d.kind] || { storage: "storage", imaging: "imaging", video: "video", hid: "hid", audio: "audio", printer: "printer", wireless: "wireless", communications: "serial", vendor: "chip" }[d.class] || "chip";
const isSpecial = (d) => ["apple-dfu", "apple-recovery", "android-fastboot"].includes(d?.kind);
const hostShort = (a) => (a || "").replace(/:3241$/, "");
const speedShort = (s) => (s || "").split(" (")[0];
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

// Keyed list reconciliation: cards enter/leave with CSS transitions instead of being re-rendered.
function sync(parent, items, key, create, update) {
  const existing = new Map([...parent.children].filter((c) => c.dataset.key && !c.classList.contains("leave")).map((c) => [c.dataset.key, c]));
  let prev = null;
  for (const it of items) {
    const k = String(key(it));
    let node = existing.get(k);
    if (!node) {
      node = create(it); node.dataset.key = k; node.classList.add("enter");
      requestAnimationFrame(() => requestAnimationFrame(() => node.classList.remove("enter")));
    } else existing.delete(k);
    update(node, it);
    const ref = prev ? prev.nextSibling : parent.firstChild;
    if (node !== ref) parent.insertBefore(node, ref);
    prev = node;
  }
  existing.forEach((n) => { n.classList.add("leave"); setTimeout(() => n.remove(), 380); });
}
const setText = (e, t) => { if (e.textContent !== String(t)) e.textContent = t; };
const setHTML = (e, t) => { if (e.innerHTML !== t) e.innerHTML = t; };

// animated numbers
function tween(e, to, fmt = (v) => Math.round(v)) {
  const from = e._v ?? 0; if (from === to) { setText(e, fmt(to)); return; }
  const t0 = performance.now(), D = 600; e._v = to;
  cancelAnimationFrame(e._raf);
  const step = (t) => { const k = Math.min(1, (t - t0) / D), v = from + (to - from) * (1 - Math.pow(1 - k, 3)); setText(e, fmt(v)); if (k < 1) e._raf = requestAnimationFrame(step); };
  e._raf = requestAnimationFrame(step);
}
function spark(svg, data, color = "var(--accent)") {
  const w = 300, hgt = 46, max = Math.max(1, ...data), n = Math.max(2, data.length);
  const pts = data.map((v, i) => [(i / (n - 1)) * w, hgt - 4 - (v / max) * (hgt - 10)]);
  const d = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(" ");
  const id = svg.dataset.g || (svg.dataset.g = "g" + Math.random().toString(36).slice(2, 7));
  svg.setAttribute("viewBox", `0 0 ${w} ${hgt}`); svg.setAttribute("preserveAspectRatio", "none");
  svg.innerHTML = `<defs><linearGradient id="${id}" x1="0" x2="0" y1="0" y2="1"><stop offset="0" stop-color="${color}" stop-opacity=".35"/><stop offset="1" stop-color="${color}" stop-opacity="0"/></linearGradient></defs>` +
    (pts.length > 1 ? `<path d="${d} L${w},${hgt} L0,${hgt} Z" fill="url(#${id})"/><path d="${d}" fill="none" stroke="${color}" stroke-width="1.8" stroke-linejoin="round"/>` : "");
}

// ------------------------------------------------------------------ state
const S = { status: null, devices: [], connected: false, view: "overview", rates: new Map(), hist: Array(40).fill(0), logs: [], remote: { addr: "", list: null, error: null, loading: false, forAddr: "" }, doctor: null, doctorLoading: false, autoOn: true, roOn: false, busy: new Set() };
const VIEWS = [
  { id: "overview", label: "Overview", icon: "grid", sub: "Everything that is connected, live." },
  { id: "share", label: "Share", icon: "share", sub: "USB devices on this computer. Switch one on to let trusted computers use it." },
  { id: "connect", label: "Connect", icon: "connect", sub: "Use a device that is plugged into another computer." },
  { id: "trust", label: "Trust", icon: "shield", sub: "Only computers you trust can connect to each other." },
  { id: "health", label: "Health", icon: "pulse", sub: "Checks that everything needed is working, with fixes." },
  { id: "logs", label: "Activity", icon: "terminal", sub: "What the daemon is doing right now." },
];

// ------------------------------------------------------------------ toasts, modals
function toast(msg, kind = "ok", ms = 4200) {
  const t = h("div", { class: `toast ${kind}` }, h("span", { class: "mark" }, kind === "ok" ? "✓" : kind === "bad" ? "!" : "i"), h("span", {}, msg));
  $("#toasts").append(t);
  const kill = () => { t.classList.add("out"); setTimeout(() => t.remove(), 350); };
  t.addEventListener("click", kill); setTimeout(kill, ms);
}
function modal({ title, body, actions }) {
  return new Promise((resolve) => {
    const close = (v) => { scrim.classList.add("out"); setTimeout(() => scrim.remove(), 200); resolve(v); };
    const box = h("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-label": title }, h("h2", {}, title), typeof body === "string" ? h("p", { html: body }) : body,
      h("div", { class: "actions" }, actions.map((a) => h("button", { class: `btn ${a.kind || ""}`, onclick: () => close(a.value) }, a.label))));
    const scrim = h("div", { class: "scrim", onmousedown: (e) => e.target === scrim && close(null) }, box);
    document.body.append(scrim);
    const f = box.querySelector("input"); if (f) f.focus();
    scrim._close = close;
  });
}
// ------------------------------------------------------------------ help drawer + first-run guide
const fmt = (t) => esc(t).replace(/`([^`]+)`/g, "<code>$1</code>");
function helpHTML(tab, view) {
  if (tab === "page") {
    const p = PAGES[view] || PAGES.overview;
    return `<h3>${esc(p.title)}</h3><p class="lead">${fmt(p.what)}</p><ol class="hsteps">${p.steps.map((s) => `<li>${fmt(s)}</li>`).join("")}</ol>
      <details class="hx"><summary>For experts</summary><p>${fmt(p.expert)}</p></details>
      <p class="faint" style="font-size:12.5px">Press <kbd>?</kbd> any time to open or close this help. <kbd>1</kbd>–<kbd>6</kbd> switch pages.</p>`;
  }
  if (tab === "basics") {
    return `<p class="lead">${fmt(BASICS.intro)}</p>
      <ol class="hsteps big">${BASICS.steps.map(([t, d]) => `<li><b>${esc(t)}</b><span>${fmt(d)}</span></li>`).join("")}</ol>
      <h4>Good to know</h4><ul class="htips">${BASICS.tips.map((t) => `<li>${fmt(t)}</li>`).join("")}</ul>`;
  }
  if (tab === "terms") return `<dl class="hterms">${GLOSSARY.map(([t, d]) => `<dt>${esc(t)}</dt><dd>${fmt(d)}</dd>`).join("")}</dl>`;
  return EXPERT.sections.map(([t, d], i) => `<details class="hx"${i === 0 ? " open" : ""}><summary>${esc(t)}</summary><p>${fmt(d)}</p></details>`).join("") +
    `<details class="hx"><summary>Command line cheat sheet</summary><table class="cli">${EXPERT.cli.map(([c, d]) => `<tr><td><code>${esc(c)}</code></td><td>${esc(d)}</td></tr>`).join("")}</table>
    <p class="faint" style="font-size:12.5px">Run <code>periwire --help</code> or <code>periwire COMMAND --help</code> for every option.</p></details>`;
}
function openHelp(tab) {
  const open = $(".help-scrim");
  if (open) { open._close(); if (!tab) return; }
  let cur = tab || "page";
  const tabs = [["page", "This page"], ["basics", "Basics"], ["terms", "Terms"], ["pro", "For experts"]];
  const tabsEl = h("div", { class: "tabs help-tabs", role: "tablist" }, tabs.map(([k, l]) => h("button", { "data-t": k, role: "tab", onclick: () => { cur = k; render(); } }, l)));
  const body = h("div", { class: "help-body sel" });
  const st = S.status;
  const foot = h("div", { class: "help-foot faint" }, st ? `periwire ${st.version} · ${st.os} ${st.arch} · ${st.hostname}${st.root ? " · admin" : ""}` : "periwire");
  const close = () => { scrim.classList.add("out"); setTimeout(() => scrim.remove(), 200); };
  const box = h("aside", { class: "help-drawer", role: "dialog", "aria-label": "Help" },
    h("div", { class: "help-head" }, h("b", {}, "Help"), h("button", { class: "btn ghost sm", onclick: close, "aria-label": "Close help" }, "✕")), tabsEl, body, foot);
  const scrim = h("div", { class: "help-scrim", onmousedown: (e) => e.target === scrim && close() }, box);
  scrim._close = close;
  function render() { body.innerHTML = helpHTML(cur, S.view); tabsEl.querySelectorAll("button").forEach((b) => { const on = b.dataset.t === cur; b.classList.toggle("on", on); b.setAttribute("aria-selected", on); }); body.scrollTop = 0; }
  document.body.append(scrim); render();
}
const ONB_KEY = "periwire.onboarding.dismissed";
const onbDismissed = () => { try { return localStorage.getItem(ONB_KEY) === "1"; } catch { return false; } };
function renderOnboard() {
  const host = $("#onboard"); if (!host) return; const st = S.status;
  const done = st ? [st.shares.length > 0, st.trusted.length > 0, st.attachments.length > 0 || st.connections.length > 0] : [false, false, false];
  const show = !!st && !onbDismissed() && !done.every(Boolean);
  const sig = JSON.stringify([show, done]);
  if (host._sig === sig) return; host._sig = sig; host.innerHTML = ""; if (!show) return;
  const steps = [["Share a device", "Open Share and switch on a device that is plugged into this computer.", "share"], ["Trust the other computer", "Compare the code on both screens, then add it under Trust.", "trust"], ["Use its device", "Open Connect, browse the other computer and press Attach.", "connect"]];
  host.append(h("div", { class: "card onboard" },
    h("div", { class: "row wrap", style: "justify-content:space-between;gap:10px;margin-bottom:12px" },
      h("div", {}, h("b", {}, "Getting started"), h("div", { class: "muted", style: "font-size:13px" }, "Three steps to use a USB device from another computer.")),
      h("div", { class: "row", style: "gap:8px" }, h("button", { class: "btn sm ghost", onclick: () => openHelp("basics") }, "How it works"),
        h("button", { class: "btn sm ghost", "aria-label": "Dismiss getting started", onclick: () => { try { localStorage.setItem(ONB_KEY, "1"); } catch {} renderOnboard(); } }, "Dismiss"))),
    h("div", { class: "onb-steps" }, steps.map(([t, d, v], i) => h("button", { class: "onb-step" + (done[i] ? " done" : ""), onclick: () => go(v) },
      h("span", { class: "onb-n" }, done[i] ? "✓" : String(i + 1)), h("span", {}, h("b", {}, t), h("span", { class: "muted" }, d)))))));
}

// Switches are plain elements: make them real switches for keyboard and screen-reader users.
let a11yQueued = false;
function a11y() {
  a11yQueued = false;
  document.querySelectorAll(".switch").forEach((sw) => {
    if (!sw.hasAttribute("role")) { sw.setAttribute("role", "switch"); sw.tabIndex = 0; }
    const on = sw.classList.contains("on") ? "true" : "false";
    if (sw.getAttribute("aria-checked") !== on) sw.setAttribute("aria-checked", on);
    if (!sw.hasAttribute("aria-label")) {
      const devName = sw.closest(".dev")?.querySelector(".nm")?.textContent; const lab = (sw.closest("label")?.textContent.trim() ? sw.closest("label").textContent.trim() + (devName ? ": " + devName : "") : "") || (sw.closest(".dev") ? "Share " + (sw.closest(".dev").querySelector(".nm")?.textContent || "device") : sw.closest(".card")?.querySelector("b")?.textContent) || "Toggle";
      sw.setAttribute("aria-label", lab.replace(/\s+/g, " "));
    }
  });
}
new MutationObserver(() => { if (!a11yQueued) { a11yQueued = true; setTimeout(a11y, 30); } }).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["class"] });
document.addEventListener("keydown", (e) => {
  if ((e.key === " " || e.key === "Enter") && e.target.classList?.contains("switch")) { e.preventDefault(); e.target.click(); }
  if (e.key === "Escape") { const s = $(".scrim"); if (s) s._close(null); else { const hs = $(".help-scrim"); if (hs) hs._close(); } }
  if (e.key === "?" && !/INPUT|TEXTAREA/.test(document.activeElement.tagName) && !e.metaKey && !e.ctrlKey) openHelp();
  if (!/INPUT|TEXTAREA/.test(document.activeElement.tagName) && !e.metaKey && !e.ctrlKey && /^[1-6]$/.test(e.key)) go(VIEWS[+e.key - 1].id);
});
async function act(key, fn, okMsg) {
  if (S.busy.has(key)) return; S.busy.add(key); renderAll();
  try { const r = await fn(); if (okMsg) toast(typeof okMsg === "function" ? okMsg(r) : okMsg); return r; }
  catch (e) { toast(e.message || String(e), "bad", 7000); }
  finally { S.busy.delete(key); await refresh(); }
}
async function refresh() { try { S.status = await call("status"); S.devices = await call("devices"); } catch {} renderAll(); }

// ------------------------------------------------------------------ actions
const specFor = (d) => { const have = (S.status?.shares || []).map((s) => s.spec.toLowerCase()); return have.includes(d.busid) ? d.busid : `${d.vid}:${d.pid}`; };
async function toggleShare(d) {
  if (d.shared && d.in_use_by) {
    const ok = await modal({ title: `Stop sharing ${devName(d)}?`, body: `<b>${esc(d.in_use_by)}</b> is using it right now. It will be disconnected immediately and the device returns to this computer. Unsaved work on that computer may be lost.`, actions: [{ label: "Keep sharing", value: false }, { label: "Disconnect & stop", value: true, kind: "danger" }] });
    if (!ok) return;
  }
  if (!d.shared && d.mounted?.length) toast(`${devName(d)} is mounted on this computer. Another computer can only take it after you eject it here.`, "info", 7000);
  await act("share:" + d.busid, () => call(d.shared ? "share.remove" : "share.add", { spec: d.shared ? specFor(d) : `${d.vid}:${d.pid}` }), d.shared ? "Stopped sharing" : "Now shared with trusted computers");
}
// Owner-side read-only: every computer that imports this drive gets it write-protected. Changing it disconnects a current user.
async function toggleShareRO(d) {
  const ro = !d.shared_read_only;
  if (d.in_use_by) {
    const ok = await modal({ title: ro ? "Make it read-only?" : "Allow writing again?", body: `<b>${esc(d.in_use_by)}</b> is using it right now. Changing this disconnects them; they can reconnect and will get the new access.`, actions: [{ label: "Cancel", value: false }, { label: "Change and disconnect", value: true, kind: "primary" }] });
    if (!ok) return;
  }
  await act("shro:" + d.busid, () => call("share.add", { spec: specFor(d), read_only: ro }), ro ? "Shared read-only" : "Shared with read and write");
}
async function eject(a, force = false) {
  const name = `${a.vid}:${a.pid}`;
  try {
    S.busy.add("att:" + a.id); renderAll();
    const r = await call("detach", { id: a.id, force });
    toast(r.unmounted?.length ? `Ejected ${name}. Unmounted ${r.unmounted.join(", ")}` : `Ejected ${name}`);
  } catch (e) {
    if (e.code === "busy" && !force) {
      const ok = await modal({ title: "Device is busy", body: `${esc(e.message)}<br><br>Forcing the eject can lose unsaved data on the device.`, actions: [{ label: "Cancel", value: false }, { label: "Force eject", value: true, kind: "danger" }] });
      S.busy.delete("att:" + a.id); if (ok) return eject(a, true);
    } else toast(e.message, "bad", 7000);
  } finally { S.busy.delete("att:" + a.id); await refresh(); }
}
async function kick(s) {
  const who = s.peer_name || s.peer;
  const ok = await modal({ title: `Disconnect ${who}?`, body: `${esc(who)} loses ${s.vid}:${s.pid} immediately and the device comes back to this computer. Their open files on it may be lost — ask them to eject it first if you can.`, actions: [{ label: "Cancel", value: false }, { label: "Disconnect", value: true, kind: "danger" }] });
  if (ok) act("kick:" + s.id, () => call("kick", { session: s.id }), "Disconnected");
}
async function browse(addr) {
  addr = (addr || "").trim(); if (!addr) return;
  Object.assign(S.remote, { addr, loading: true, error: null, list: null, forAddr: addr }); renderConnect();
  try { S.remote.list = await call("remote.list", { addr }); } catch (e) { S.remote.error = e.message; }
  S.remote.loading = false; renderConnect();
}
async function attach(addr, busid, auto, name, read_only) {
  const key = `attach:${addr}:${busid}`;
  const r = await act(key, () => call("attach", { addr, busid, auto, name, read_only: !!read_only }), (r) => `Attached ${r.vid}:${r.pid} — it now appears on this computer${r.read_only ? ", write-protected" : ""}`);
  if (r) go("overview");
}
// Read-only is decided when the device is imported (the other computer enforces it), so changing it is a safe eject + re-attach.
async function setAccess(a, ro) {
  const ok = await modal({ title: ro ? "Switch to read-only?" : "Allow writing again?",
    body: ro ? "The drive is safely ejected and reconnected <b>write-protected</b>: nothing on it can be changed from this computer. Anything open from it closes for a moment."
             : "The drive is safely ejected and reconnected with <b>read and write</b> access. Anything open from it closes for a moment.",
    actions: [{ label: "Cancel", value: false }, { label: ro ? "Switch to read-only" : "Allow writing", value: true, kind: "primary" }] });
  if (!ok) return;
  await act("access:" + a.id, async () => { await call("detach", { id: a.id }); await call("attach", { addr: a.addr, busid: a.busid, auto: a.auto, read_only: ro }); }, ro ? "Now read-only" : "Read and write enabled");
}
async function probeTrust(addr) {
  addr = (addr || "").trim(); if (!addr) return;
  let p; try { toast("Connecting…", "info", 1500); p = await call("trust.probe", { addr }); } catch (e) { return toast(e.message, "bad", 7000); }
  if (p.already_trusted) return toast(`${addr} is already trusted`, "info");
  const chk = h("input", { type: "checkbox", id: "cmp" });
  const body = h("div", {}, h("p", {}, `${addr} presents this fingerprint. Run `, h("span", { class: "mono" }, "periwire id"), " (or open Trust in the app) on that computer and compare. Trust it only if they are identical."),
    h("div", { class: "row", style: "align-items:flex-start" }, fpArt(p.fingerprint), h("div", { class: "fp" }, p.fingerprint.match(/.{1,8}/g).join(" "))),
    h("label", { class: "row", style: "margin-top:16px;cursor:pointer" }, chk, h("span", {}, "I compared it and it matches")));
  const ok = await modal({ title: "Trust this computer?", body, actions: [{ label: "Cancel", value: false }, { label: "Trust", value: true, kind: "primary" }] });
  if (ok && chk.checked) act("trust", () => call("trust.add", { fingerprint: p.fingerprint, name: addr }), "Trusted");
  else if (ok) toast("Tick the box to confirm you compared the fingerprints", "info", 5000);
}
function copy(text) {
  const done = () => toast("Copied");
  if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).then(done, fallback); else fallback();
  function fallback() { const t = h("textarea", { style: "position:fixed;opacity:0" }); t.value = text; document.body.append(t); t.select(); try { document.execCommand("copy"); done(); } catch { toast("Could not copy", "bad"); } t.remove(); }
}
function fpArt(fp) {
  const g = h("div", { class: "fpart" }); const hex = (fp || "0".repeat(64)).padEnd(64, "0");
  for (let r = 0; r < 8; r++) for (let c = 0; c < 8; c++) {
    const cc = c < 4 ? c : 7 - c, v = parseInt(hex[r * 4 + cc] || "0", 16);
    g.append(h("i", { style: `background:${v < 5 ? "var(--surface-2)" : `hsl(${200 + v * 10} 75% ${52 + (v % 3) * 6}%)`}` }));
  }
  return g;
}

// ------------------------------------------------------------------ shell: nav, views, header
const nav = $("#nav"), viewsEl = $("#views"), pill = $("#pill");
const vEls = {};
VIEWS.forEach((v) => {
  nav.append(h("button", { "data-v": v.id, onclick: () => go(v.id), html: `${ic(v.icon)}<span>${v.label}</span><em class="badge" id="b-${v.id}"></em>` }));
  vEls[v.id] = h("section", { class: "view", id: "v-" + v.id }); viewsEl.append(vEls[v.id]);
});
function go(id) {
  S.view = id; const v = VIEWS.find((x) => x.id === id);
  document.querySelectorAll(".nav button").forEach((b) => b.classList.toggle("active", b.dataset.v === id));
  Object.entries(vEls).forEach(([k, e]) => e.classList.toggle("active", k === id));
  const t = $("#title"); t.style.transition = "none"; t.style.opacity = 0; t.style.transform = "translateY(6px)"; setText(t, v.label); setText($("#subtitle"), v.sub);
  requestAnimationFrame(() => { t.style.transition = "all .45s var(--ease)"; t.style.opacity = 1; t.style.transform = "none"; });
  const i = VIEWS.findIndex((x) => x.id === id); const b = nav.children[i + 1]; pill.style.transform = `translateY(${b.offsetTop}px)`;
  renderActions(); if (id === "health" && !S.doctor) runDoctor(); if (id === "logs") scrollLogs(true);
  try { location.hash = id; } catch {}
  renderAll();
}
function renderActions() {
  const a = $("#actions"); a.innerHTML = "";
  a.append(h("span", { class: `chip mobile-only ${S.connected ? "ok live" : "bad"}`, html: `<i class="dot"></i>${S.connected ? "daemon running" : "daemon offline"}` }));
  if (S.view === "health") a.append(h("button", { class: "btn", onclick: runDoctor, html: `${ic("refresh")} Run checks` }));
  if (S.view === "share" && S.status) a.append(h("span", { class: `chip ${S.status.export.state === "listening" ? "ok live" : S.status.export.state === "error" ? "bad" : ""}`, html: `<i class="dot"></i>${S.status.export.state === "listening" ? "Listening " + esc(S.status.export.listen) : S.status.export.state === "error" ? "Exporter error" : "Exporter off"}` }));
}

// ------------------------------------------------------------------ rates
function updateRates(st) {
  const t = performance.now(); let total = 0; const seen = new Set();
  const feed = (k, b) => { seen.add(k); const r = S.rates.get(k); if (r) { const dt = (t - r.t) / 1000; if (dt > 0.3) { const bps = Math.max(0, (b - r.b) / dt); r.bps = r.bps * 0.4 + bps * 0.6; r.b = b; r.t = t; r.h.push(r.bps); if (r.h.length > 40) r.h.shift(); } } else S.rates.set(k, { b, t, bps: 0, h: Array(40).fill(0) }); total += S.rates.get(k).bps; };
  (st.attachments || []).forEach((a) => feed("a" + a.id, a.bytes_in + a.bytes_out));
  (st.sessions || []).forEach((s) => feed("s" + s.id, s.bytes_in + s.bytes_out));
  [...S.rates.keys()].forEach((k) => !seen.has(k) && S.rates.delete(k));
  S.hist.push(total); if (S.hist.length > 40) S.hist.shift();
}
const bps = (k) => S.rates.get(k)?.bps || 0;

// ------------------------------------------------------------------ Overview
const ov = vEls.overview;
ov.innerHTML = `<div class="drv-banner"></div><div id="onboard"></div>
  <div class="grid g4" id="tiles"></div>
  <div class="card topo" id="topo"><svg id="topo-svg" viewBox="0 0 900 270"></svg><div class="empty" id="topo-empty"></div></div>
  <div class="section-title">Active connections<span class="line"></span></div>
  <div class="list" id="conns"></div>`;
const tiles = [
  { id: "t-shared", label: "Shared by this computer", icon: "share" }, { id: "t-imp", label: "Imported here", icon: "connect" },
  { id: "t-rate", label: "Throughput", icon: "zap", spark: true }, { id: "t-trust", label: "Trusted computers", icon: "shield" },
];
tiles.forEach((t) => $("#tiles").append(h("div", { class: "card tile", id: t.id, html: `<div class="label">${ic(t.icon)}${t.label}</div><div class="value"><span class="n">0</span><small></small></div><div class="sub"></div>${t.spark ? '<svg class="spark"></svg>' : ""}` })));
let topoSig = "";
function renderOverview() {
  const st = S.status; if (!st) return;
  const inUse = st.sessions.length, nsh = st.shares.length;
  const set = (id, v, small, sub, fmt) => { const t = $("#" + id); tween($(".n", t), v, fmt); setText($("small", t), small || ""); setText($(".sub", t), sub || ""); };
  set("t-shared", nsh, nsh === 1 ? "device" : "devices", inUse ? `${inUse} in use right now` : "none in use");
  set("t-imp", st.attachments.length, st.attachments.length === 1 ? "device" : "devices", st.import.available ? (st.attachments.filter((a) => a.auto).length ? "auto-reconnect on" : "ready to attach") : "importing unavailable");
  const tot = S.hist[S.hist.length - 1]; const tt = $("#t-rate"); setText($(".n", tt), tot < 1 ? "0" : bytes(tot).split(" ")[0]); setText($("small", tt), tot < 1 ? "B/s" : bytes(tot).split(" ")[1] + "/s"); setText($(".sub", tt), tot < 1 ? "idle" : "across all connections"); spark($(".spark", tt), S.hist, "var(--accent-2)");
  set("t-trust", st.trusted.length, "", st.trusted.length ? st.trusted.map((t) => t.name).slice(0, 2).join(", ") : "nobody yet");
  // topology
  const peers = [...new Set(st.sessions.map((s) => s.peer_name || s.peer.split(":")[0]))];
  const hosts = [...new Set(st.attachments.map((a) => a.addr))];
  const sig = JSON.stringify([peers, hosts, st.hostname, Math.round($("#topo").clientWidth / 40)]);
  if (sig !== topoSig) { topoSig = sig; drawTopo(st, peers, hosts); }
  document.querySelectorAll("#topo-svg .flow.live").forEach((p) => {
    const r = p.dataset.key[0] === "p" ? st.sessions.filter((s) => (s.peer_name || s.peer.split(":")[0]) === p.dataset.name).reduce((a, s) => a + bps("s" + s.id), 0) : st.attachments.filter((a) => a.addr === p.dataset.name).reduce((a, x) => a + bps("a" + x.id), 0);
    p.style.animationDuration = Math.max(0.45, 2.4 - Math.log10(r + 10) * 0.33) + "s";
  });
  // lists
  const items = [...st.attachments.map((a) => ({ k: "a" + a.id, kind: "att", a })), ...st.sessions.map((s) => ({ k: "s" + s.id, kind: "ses", s }))];
  const box = $("#conns");
  sync(box, items, (i) => i.k, (i) => connItem(i), updateConnItem);
  const empty = $("#conns-empty") || h("div", { class: "empty-card", id: "conns-empty", html: "<b>Nothing connected</b>Share a device from this computer, or attach one from another computer." });
  if (!items.length && !empty.isConnected) box.append(empty); else if (items.length && empty.isConnected) empty.remove();
}
function connItem(i) {
  const e = h("div", { class: "item", html: `<div class="head"><div class="ico"></div>
    <div class="grow" style="flex:1;min-width:0"><div class="row" style="gap:8px;flex-wrap:wrap"><b class="t"></b><span class="chip st"></span><span class="chip kind"></span><span class="chip ro warn" style="display:none">read-only</span></div><div class="muted sub" style="font-size:12.5px;margin-top:2px"></div></div>
    <div class="stats"></div><svg class="spark" style="position:static;width:90px;height:30px;flex:none"></svg><button class="btn sm danger act"></button><svg class="chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg></div>
    <div class="details"><div class="kv"></div><label class="row ro-row" style="gap:10px;margin-top:12px;cursor:pointer;display:none"><div class="switch ro-sw"></div><span><b>Read-only</b><span class="muted" style="display:block;font-size:12px">Write-protect this drive. Off = read and write.</span></span></label></div>` });
  $(".ro-row .ro-sw", e).addEventListener("click", () => { const cur = e._i; if (cur && cur.kind === "att") setAccess(cur.a, !cur.a.read_only); });
  $(".head", e).addEventListener("click", (ev) => { if (!ev.target.closest(".act")) e.classList.toggle("open"); });
  $(".act", e).addEventListener("click", () => { const cur = e._i; cur.kind === "att" ? eject(cur.a) : kick(cur.s); });
  return e;
}
function kvRows(rows) { return rows.filter((r) => r[1] !== "" && r[1] != null).map(([k, v]) => `<span>${esc(k)}</span><span class="sel">${esc(v)}</span>`).join(""); }
function updateConnItem(e, i) {
  e._i = i; const att = i.kind === "att";
  const info = (att ? i.a.info : i.s.info) || {}; const known = !!info.busid;
  const x = att ? i.a : i.s; const k = (att ? "a" : "s") + x.id;
  const title = known ? devName(info) : `USB device ${x.vid}:${x.pid}`;
  setText($(".t", e), title); setHTML($(".ico", e), ic(known ? iconFor(info) : "chip"));
  const roOn = !!x.read_only; const roChip = $(".chip.ro", e); roChip.style.display = roOn ? "" : "none";
  const roRow = $(".ro-row", e); roRow.style.display = att ? "flex" : "none"; if (att) { const sw = $(".ro-sw", e); sw.classList.toggle("on", roOn); sw.classList.toggle("busy", S.busy.has("access:" + x.id)); }
  const kc = $(".kind", e); kc.className = "chip kind" + (isSpecial(info) ? " special" : ""); setText(kc, known ? info.kind_label : "USB device"); kc.style.display = known ? "" : "none";
  const rows = [["Device ID", `${x.vid}:${x.pid}`], ["Port", x.busid], ["Speed", att ? x.speed : info.speed]];
  if (att) {
    const a = i.a; const live = a.state === "attached";
    const ch = $(".st", e); ch.className = `chip st ${live ? "ok live" : "warn live"}`; setHTML(ch, `<i class="dot"></i>${live ? "connected" : a.state}`);
    setText($(".sub", e), `from ${hostShort(a.addr)}${a.mounts?.length ? " · mounted at " + a.mounts[0] : ""}${a.error ? " · " + a.error : ""}`);
    setHTML($(".stats", e), `<span>↓ <b>${bytes(a.bytes_in)}</b></span><span>↑ <b>${bytes(a.bytes_out)}</b></span><span><b>${rate(bps(k))}</b></span>`);
    const b = $(".act", e); setHTML(b, S.busy.has("att:" + a.id) ? '<i class="spin"></i> Ejecting' : `${ic("eject")} Eject`); b.disabled = S.busy.has("att:" + a.id);
    rows.push(["Computer", a.addr], ["Appears here as", a.local_busid], ["Connected for", a.attached_at ? dur(nowS() - a.attached_at) : ""], ["Access", a.read_only ? "read-only (write-protected)" : "read and write"], ["Reconnects automatically", a.auto ? "yes" : "no"], ["Mounted at", (a.mounts || []).join(", ")]);
  } else {
    const s = i.s; const ch = $(".st", e); ch.className = "chip st acc live"; setHTML(ch, `<i class="dot"></i>in use by ${esc(s.peer_name || s.peer)}`);
    setText($(".sub", e), `shared from this computer · connected ${dur(nowS() - s.since)}`);
    setHTML($(".stats", e), `<span>↑ <b>${bytes(s.bytes_out)}</b></span><span>↓ <b>${bytes(s.bytes_in)}</b></span><span><b>${rate(bps(k))}</b></span>`);
    setHTML($(".act", e), `${ic("plug")} Disconnect`);
    rows.push(["Used by", s.peer_name ? `${s.peer_name} (${s.peer})` : s.peer], ["USB requests", String(s.urbs)]);
  }
  spark($(".spark", e), S.rates.get(k)?.h || [], "var(--accent)");
  setHTML($(".kv", e), kvRows(rows));
}
function drawTopo(st, peers, hosts) {
  const svg = $("#topo-svg"); const W = Math.max(300, Math.round($("#topo").clientWidth - 24)), narrow = W < 560;
  const H = narrow ? 230 : 270, cx = W / 2, cy = H / 2, nw = narrow ? Math.min(112, W * 0.3) : Math.min(168, W * 0.2), cw = narrow ? Math.min(124, W * 0.34) : 190;
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`); svg.style.height = H + "px";
  const clip = (t) => (t.length > (narrow ? 13 : 22) ? t.slice(0, narrow ? 12 : 21) + "…" : t);
  const node = (x, y, w, t, sub, cls = "") => `<g class="node ${cls}" transform="translate(${x - w / 2},${y - 28})"><rect width="${w}" height="56" rx="15"/><text x="${w / 2}" y="25" text-anchor="middle">${esc(clip(t))}</text><text class="sub" x="${w / 2}" y="42" text-anchor="middle">${esc(sub)}</text></g>`;
  const col = (list, x) => list.slice(0, 4).map((n, i, a) => ({ n, x, y: a.length === 1 ? cy : 44 + (i * (H - 88)) / (a.length - 1) }));
  const lx = nw / 2 + 8, L = col(peers, lx), R = col(hosts, W - lx), side = cw / 2, edge = nw / 2;
  let out = `<defs><linearGradient id="grad" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="${W}" y2="0"><stop offset="0" style="stop-color:var(--accent)"/><stop offset="1" style="stop-color:var(--accent-2)"/></linearGradient></defs>`;
  const mid = (a, b) => (a + b) / 2;
  L.forEach((p, i) => { const n = st.sessions.filter((s) => (s.peer_name || s.peer.split(":")[0]) === p.n).length; out += `<path class="flow live" data-key="p${i}" data-name="${esc(p.n)}" d="M${cx - side},${cy} C${mid(cx - side, p.x + edge)},${cy} ${mid(cx - side, p.x + edge)},${p.y} ${p.x + edge},${p.y}"/>` + node(p.x, p.y, nw, p.n, narrow ? `${n} used` : `using ${n} of mine`); });
  R.forEach((p, i) => { const n = st.attachments.filter((a) => a.addr === p.n).length; out += `<path class="flow live" data-key="h${i}" data-name="${esc(p.n)}" d="M${p.x - edge},${p.y} C${mid(cx + side, p.x - edge)},${p.y} ${mid(cx + side, p.x - edge)},${cy} ${cx + side},${cy}"/>` + node(p.x, p.y, nw, hostShort(p.n), narrow ? `${n} imported` : `${n} device${n === 1 ? "" : "s"} imported`); });
  if (peers.length > 4) out += `<text x="${lx}" y="${H - 6}" text-anchor="middle" fill="var(--text-3)" font-size="11">+${peers.length - 4} more</text>`;
  if (hosts.length > 4) out += `<text x="${W - lx}" y="${H - 6}" text-anchor="middle" fill="var(--text-3)" font-size="11">+${hosts.length - 4} more</text>`;
  out += `<circle class="pulse-ring" cx="${cx}" cy="${cy}" r="${peers.length + hosts.length ? 52 : 46}"/>` + node(cx, cy, cw, st.hostname, "this computer", "me");
  svg.innerHTML = out;
  const empty = $("#topo-empty"); const none = !peers.length && !hosts.length;
  empty.style.display = none ? "grid" : "none"; svg.style.opacity = none ? 0.3 : 1;
  $("#topo").classList.toggle("is-empty", none);
  if (none && !empty.children.length) { empty.append(h("h3", {}, "Nothing connected yet"), h("p", {}, "Share a USB device from this computer, or use one that is plugged into another."), h("div", { class: "row", style: "flex-wrap:wrap;justify-content:center" }, h("button", { class: "btn primary", onclick: () => go("share"), html: `${ic("share")} Share a device` }), h("button", { class: "btn", onclick: () => go("connect"), html: `${ic("connect")} Use a remote device` }))); }
}

// ------------------------------------------------------------------ Share
const sh = vEls.share;
sh.innerHTML = `<div class="card row" style="padding:16px 20px;justify-content:space-between" id="export-card"><div class="row"><div class="ico">${ic("share")}</div><div><b>Let other computers use my devices</b><div class="muted" id="export-sub"></div></div></div><div class="switch" id="export-sw"></div></div>
  <div class="section-title">Devices on this computer<span class="line"></span></div><div class="grid g-auto" id="devs"></div>`;
$("#export-sw").addEventListener("click", () => act("export", () => call("export.set", { enabled: !S.status.export.enabled }), S.status?.export.enabled ? "Sharing paused" : "Sharing resumed"));
function renderShare() {
  const st = S.status; if (!st) return;
  const on = st.export.state === "listening"; const sw = $("#export-sw"); sw.classList.toggle("on", st.export.enabled); sw.classList.toggle("busy", S.busy.has("export"));
  setText($("#export-sub"), st.export.state === "error" ? `Cannot listen: ${st.export.error}` : on ? `Listening on ${st.export.listen} · ${st.trusted.length} trusted computer${st.trusted.length === 1 ? "" : "s"} may connect` : "Off — nothing is shared, every device stays on this computer");
  sync($("#devs"), S.devices, (d) => d.busid, (d) => devCard(d), updateDevCard);
}
function devCard(d) {
  const e = h("div", { class: "card dev", html: `<div class="dev-head"><div class="ico"></div><div class="grow"><div class="dev-title"><span class="nm"></span><button class="edit-btn" title="Rename">${ic("pencil")}</button></div><div class="dev-sub"></div></div><div class="switch"></div></div><div class="dev-body"></div><label class="row ro-wrap" style="gap:9px;cursor:pointer;display:none" title="Nobody who connects can change this drive"><div class="switch sm ro-share"></div><span style="font-size:12.5px">Read-only for others</span></label><div class="dev-foot"><span class="faint note" style="font-size:12px"></span><span class="slot"></span></div>` });
  $(".switch:not(.ro-share)", e).addEventListener("click", () => toggleShare(e._d));
  $(".ro-share", e).addEventListener("click", () => toggleShareRO(e._d));
  $(".edit-btn", e).addEventListener("click", (ev) => { ev.stopPropagation(); editLabel(e); });
  return e;
}
function editLabel(e) {
  const d = e._d, t = $(".dev-title", e); if (t.querySelector("input")) return;
  const inp = h("input", { class: "input", value: d.label || "", placeholder: d.product || d.kind_label, maxlength: "60", spellcheck: "false" });
  const wrap = h("div", { class: "name-edit" }, inp); t.style.display = "none"; t.after(wrap); inp.focus(); inp.select();
  let done = false;
  const finish = async (save) => {
    if (done) return; done = true; wrap.remove(); t.style.display = "";
    if (save && inp.value.trim() !== (d.label || "")) act("label:" + d.key, () => call("device.label", { key: d.key, label: inp.value.trim() }), inp.value.trim() ? "Renamed" : "Name cleared");
  };
  inp.addEventListener("keydown", (ev) => { if (ev.key === "Enter") finish(true); if (ev.key === "Escape") { ev.stopPropagation(); finish(false); } });
  inp.addEventListener("blur", () => finish(true));
}
function updateDevCard(e, d) {
  e._d = d; const busy = S.busy.has("share:" + d.busid);
  e.classList.toggle("is-shared", d.shared); e.classList.toggle("in-use", !!d.in_use_by);
  setHTML($(".ico", e), ic(iconFor(d))); setText($(".nm", e), devName(d));
  setText($(".dev-sub", e), `${d.label && d.product ? d.product + " · " : ""}${d.vid}:${d.pid} · port ${d.busid}`);
  const sw = $(".switch:not(.ro-share)", e); sw.classList.toggle("on", d.shared); sw.classList.toggle("busy", busy);
  const roWrap = $(".ro-wrap", e); roWrap.style.display = d.shared && d.kind === "storage" ? "flex" : "none"; const rs = $(".ro-share", e); rs.classList.toggle("on", !!d.shared_read_only); rs.classList.toggle("busy", S.busy.has("shro:" + d.busid));
  const chips = [`<span class="chip ${isSpecial(d) ? "special" : ""}">${esc(d.kind_label || d.class)}</span>`, `<span class="chip">${esc(speedShort(d.speed))}</span>`];
  if (d.in_use_by) chips.push(`<span class="chip acc live"><i class="dot"></i>in use by ${esc(d.in_use_by)}</span>`); else if (d.shared) chips.push('<span class="chip ok"><i class="dot"></i>shared</span>');
  if (d.shared && d.shared_read_only) chips.push('<span class="chip warn">read-only</span>');
  if (d.mounted?.length) chips.push(`<span class="chip warn" title="${esc(d.mounted.join("\n"))}">${ic("warn").replace("<svg", '<svg width="12" height="12"')}mounted here</span>`);
  setHTML($(".dev-body", e), chips.join(""));
  setText($(".note", e), d.in_use_by ? "" : d.shared ? "Visible to trusted computers" : d.mounted?.length ? "Eject it here before sharing" : "Only on this computer");
  const slot = $(".slot", e);
  if (d.in_use_by && !slot.firstChild) slot.append(h("button", { class: "btn sm danger", onclick: () => { const s = S.status.sessions.find((x) => x.busid === e._d.busid); if (s) kick(s); }, html: `${ic("plug")} Disconnect` })); else if (!d.in_use_by) slot.innerHTML = "";
}

// ------------------------------------------------------------------ Connect
// Windows has no built-in virtual USB host controller; PeriWire carries a signed one and installs it on request.
function driverBanner() {
  const st = S.status;
  const need = !!st && st.os === "windows" && st.import && !st.import.available;
  const sig = String(need) + S.busy.has("driver");
  document.querySelectorAll(".drv-banner").forEach((host) => {
    if (host._sig === sig) return; host._sig = sig; host.innerHTML = "";
    if (!need) return;
    const busy = S.busy.has("driver");
    host.append(driverCard(busy));
  });
}
function driverCard(busy) {
  return (h("div", { class: "card", style: "padding:16px 20px;margin-bottom:16px;border-color:rgba(var(--accent-rgb),.45)" },
    h("div", { class: "row wrap", style: "justify-content:space-between;gap:14px" },
      h("div", { style: "flex:1;min-width:240px" }, h("b", {}, "One-time setup: install the USB driver"),
        h("div", { class: "muted", style: "margin-top:3px" }, "To use devices that are plugged into other computers, Windows needs a small virtual USB driver. PeriWire already contains it, so there is nothing to download.")),
      h("button", { class: "btn primary", disabled: busy ? "" : null, onclick: installDriver }, busy ? h("span", { class: "spin" }) : null, busy ? "Installing…" : "Install driver"))));
}
async function installDriver() {
  const ok = await modal({ title: "Install the USB driver?", body: "This installs a Microsoft-signed virtual USB host controller (a kernel driver) that ships inside PeriWire. Your USB 3 hubs restart for a moment, so connected USB devices blink off and on. Windows may ask for a restart afterwards. You can remove it later with <b>periwire driver uninstall</b>.", actions: [{ label: "Cancel", value: false }, { label: "Install driver", value: true, kind: "primary" }] });
  if (!ok) return;
  await act("driver", () => call("driver.install", { confirm: true }), (r) => (r.reboot ? "Driver installed — restart Windows to finish" : "Driver installed — you can connect to other computers now"));
}
const cn = vEls.connect;
cn.innerHTML = `<div class="drv-banner"></div><div class="card" style="padding:18px 20px"><div class="row"><input class="input" id="host" placeholder="Computer to connect to — name or address, e.g. media-pc.local" spellcheck="false"><button class="btn primary" id="browse">${ic("search")} Browse devices</button></div><div class="muted" style="margin-top:10px;font-size:12.5px">The other computer must run periwire and trust this one. Default port is 3241.</div></div>
  <div id="remote-out"></div>
  <div class="section-title">Attached now<span class="line"></span></div><div class="list" id="att-list"></div>
  <div class="section-title">Remembered<span class="line"></span></div><div class="list" id="mem-list"></div>`;
$("#host").addEventListener("keydown", (e) => e.key === "Enter" && browse(e.target.value));
$("#browse").addEventListener("click", () => browse($("#host").value));
function renderConnect() {
  const st = S.status; if (!st) return; driverBanner(); const r = S.remote; const out = $("#remote-out");
  const sig = JSON.stringify([r.loading, r.error, r.list, r.forAddr, [...S.busy].filter((k) => k.startsWith("attach:")), st.attachments.map((a) => a.addr + a.busid), S.autoOn, S.roOn]);
  if (out._sig !== sig) {
    out._sig = sig; out.innerHTML = "";
    if (r.loading) out.append(h("div", { class: "grid g-auto", style: "margin-top:16px" }, [1, 2, 3].map(() => h("div", { class: "skeleton", style: "height:140px;border-radius:18px" }))));
    else if (r.error) {
      const untrusted = /fingerprint|TLS|trust/i.test(r.error);
      out.append(h("div", { class: "card", style: "margin-top:16px;padding:18px 20px;border-color:rgba(251,113,133,.35)" }, h("b", {}, "Could not list devices"), h("div", { class: "muted", style: "margin:4px 0 12px" }, r.error), untrusted ? h("button", { class: "btn", onclick: () => probeTrust(r.forAddr), html: `${ic("shield")} Review & trust ${esc(r.forAddr)}` }) : null));
    } else if (r.list) {
      out.append(h("div", { class: "section-title" }, `Shared by ${r.forAddr}`, h("span", { class: "line" }),
        h("label", { class: "row", style: "gap:8px;text-transform:none;letter-spacing:0;font-size:12.5px;cursor:pointer" }, h("div", { class: "switch" + (S.autoOn ? " on" : ""), style: "transform:scale(.8)", onclick: (e) => { S.autoOn = !S.autoOn; e.currentTarget.classList.toggle("on", S.autoOn); renderConnect(); } }), "Reconnect automatically"),
        h("label", { class: "row", style: "gap:8px;text-transform:none;letter-spacing:0;font-size:12.5px;cursor:pointer", title: "Write-protect USB storage while it is attached. Off = read and write." }, h("div", { class: "switch" + (S.roOn ? " on" : ""), style: "transform:scale(.8)", onclick: (e) => { S.roOn = !S.roOn; e.currentTarget.classList.toggle("on", S.roOn); renderConnect(); } }), "Read-only")));
      if (!r.list.length) out.append(h("div", { class: "empty-card", html: `<b>Nothing shared</b>${esc(r.forAddr)} is not sharing any devices with you.` }));
      else out.append(h("div", { class: "grid g-auto" }, r.list.map((d) => {
        const have = st.attachments.find((a) => a.addr.replace(/:3241$/, "") === r.forAddr.replace(/:3241$/, "") && a.busid === d.busid); const key = `attach:${r.forAddr}:${d.busid}`; const busy = S.busy.has(key);
        return h("div", { class: "card dev" + (have ? " is-shared" : "") }, h("div", { class: "dev-head" }, h("div", { class: "ico", html: ic(iconFor(d)) }), h("div", { class: "grow" }, h("div", { class: "dev-title" }, devName(d)), h("div", { class: "dev-sub" }, `${d.label && d.product ? d.product + " · " : ""}${d.vid}:${d.pid} · port ${d.busid}`))),
          h("div", { class: "dev-body", html: `<span class="chip ${isSpecial(d) ? "special" : ""}">${esc(d.kind_label || d.class)}</span><span class="chip">${esc(speedShort(d.speed))}</span>` }),
          h("div", { class: "dev-foot" }, h("span", { class: "faint", style: "font-size:12px" }, have ? "Attached here" : "Plugged into " + r.forAddr.replace(/:3241$/, "")),
            have ? h("button", { class: "btn sm danger", onclick: () => eject(have), html: `${ic("eject")} Eject` }) : h("button", { class: "btn sm primary", disabled: busy, onclick: () => attach(r.forAddr, d.busid, S.autoOn, undefined, S.roOn), html: busy ? '<i class="spin"></i> Attaching' : `${ic("connect")} Attach` })));
      })));
    }
  }
  sync($("#att-list"), st.attachments, (a) => a.id, (a) => connItem({ kind: "att", a }), (e, a) => updateConnItem(e, { kind: "att", a }));
  const mem = st.connections;
  sync($("#mem-list"), mem, (c) => c.addr + "|" + c.busid, (c) => h("div", { class: "item", html: `<div class="ico">${ic("link")}</div><div style="flex:1;min-width:0"><b class="t"></b><div class="muted sub" style="font-size:12.5px"></div></div><span class="chip auto"></span><button class="btn sm go"></button><button class="btn sm ghost del" title="Forget">${ic("trash")}</button>` }), (e, c) => {
    e._c = c; setText($(".t", e), (c.name || c.busid)); setText($(".sub", e), `${c.busid} from ${c.addr}`);
    const ch = $(".auto", e); ch.className = `chip auto ${c.auto ? "ok" : ""}`; setText(ch, c.auto ? "auto-attach" : "manual");
    const live = st.attachments.some((a) => a.addr === c.addr && a.busid === c.busid); const b = $(".go", e); b.disabled = live; setHTML(b, live ? "Attached" : `${ic("connect")} Attach`);
    b.onclick = () => attach(e._c.addr, e._c.busid, e._c.auto, e._c.name); $(".del", e).onclick = () => act("forget", () => call("connection.remove", { addr: e._c.addr, busid: e._c.busid }), "Forgotten");
  });
  [["att-list", st.attachments.length, "Nothing attached", "Browse a computer above and attach one of its devices."], ["mem-list", mem.length, "Nothing remembered", "Attach a device with “Reconnect automatically” and it will appear here."]].forEach(([id, n, t, s]) => { const box = $("#" + id), emp = box.querySelector(".empty-card"); if (!n && !emp) box.append(h("div", { class: "empty-card", html: `<b>${t}</b>${s}` })); else if (n && emp) emp.remove(); });
}

// ------------------------------------------------------------------ Trust
const tr = vEls.trust;
tr.innerHTML = `<div class="card row" style="padding:20px;gap:20px;align-items:center" id="me-card"></div>
  <div class="section-title">Trusted computers<span class="line"></span><button class="btn sm" id="add-trust">${ic("plus")} Add computer</button></div><div class="list" id="trust-list"></div>
  <div class="danger-zone row wrap" style="justify-content:space-between"><div style="flex:1;min-width:220px"><b>Reset this computer's fingerprint</b><div class="muted" style="font-size:12.5px;margin-top:2px">Creates a new identity. Use it if you think it leaked, or to start over. Every computer that trusted this one must trust the new fingerprint, and active connections drop.</div></div><button class="btn danger" id="reset-id">Reset…</button></div>`;
$("#reset-id").addEventListener("click", async () => {
  const ok = await modal({ title: "Reset fingerprint?", body: "This computer gets a brand-new identity. Other computers will refuse to connect to it until you give them the new fingerprint, and anything currently shared or attached through it disconnects.", actions: [{ label: "Cancel", value: false }, { label: "Reset fingerprint", value: true, kind: "danger" }] });
  if (ok) act("reset", () => call("identity.reset", { confirm: true }), "New fingerprint created — re-pair your other computers");
});
$("#add-trust").addEventListener("click", addTrustModal);
async function addTrustModal() {
  const host = h("input", { class: "input", placeholder: "Computer name or address", spellcheck: "false" });
  const fp = h("input", { class: "input mono", placeholder: "64-character fingerprint", spellcheck: "false" });
  const name = h("input", { class: "input", style: "margin-top:10px", placeholder: "Name (optional)" });
  const read = h("button", { class: "btn" }, "Read");
  const body = h("div", {}, h("p", {}, "Connect to the other computer to read its fingerprint, or paste one you were given."),
    h("div", { class: "row" }, host, read), h("div", { class: "faint", style: "margin:14px 0 8px;font-size:12px;text-align:center" }, "— or —"), fp, name);
  read.addEventListener("click", () => { const a = host.value; $(".scrim")._close(null); probeTrust(a); });
  host.addEventListener("keydown", (e) => e.key === "Enter" && read.click());
  const v = await modal({ title: "Add a trusted computer", body, actions: [{ label: "Cancel", value: null }, { label: "Trust fingerprint", value: "fp", kind: "primary" }] });
  if (v === "fp") act("trust", () => call("trust.add", { fingerprint: fp.value.trim().toLowerCase(), name: name.value.trim() || "peer" }), "Trusted");
}
function renderTrust() {
  const st = S.status; if (!st) return; const me = $("#me-card");
  if (me._fp !== st.fingerprint) {
    me._fp = st.fingerprint; me.innerHTML = ""; me.append(fpArt(st.fingerprint), h("div", { style: "flex:1;min-width:0" }, h("div", { class: "muted", style: "font-size:12px;margin-bottom:4px" }, "THIS COMPUTER'S FINGERPRINT"), h("div", { class: "fp" }, st.fingerprint.match(/.{1,8}/g).join(" ")), h("div", { class: "faint", style: "font-size:12px;margin-top:6px" }, `Give this to other computers (or let them read it) so they can trust ${st.hostname}.`)), h("button", { class: "btn", onclick: () => copy(st.fingerprint), html: `${ic("copy")} Copy` }));
  }
  sync($("#trust-list"), st.trusted, (t) => t.fingerprint, (t) => h("div", { class: "item", html: `<div class="ico">${ic("shield")}</div><div style="flex:1;min-width:0"><b class="t"></b><div class="mono faint fpl"></div></div><button class="btn sm ghost cp" title="Copy fingerprint">${ic("copy")}</button><button class="btn sm danger del">${ic("trash")} Remove</button>` }), (e, t) => {
    e._t = t; setText($(".t", e), t.name || "unnamed"); setText($(".fpl", e), short(t.fingerprint)); $(".cp", e).onclick = () => copy(e._t.fingerprint);
    $(".del", e).onclick = async () => { if (await modal({ title: `Stop trusting ${e._t.name || "this computer"}?`, body: "It will no longer be able to connect, and you will not be able to use its devices.", actions: [{ label: "Cancel", value: false }, { label: "Remove", value: true, kind: "danger" }] })) act("untrust", () => call("trust.remove", { fingerprint: e._t.fingerprint }), "Removed"); };
  });
  const box = $("#trust-list"), emp = box.querySelector(".empty-card");
  if (!st.trusted.length && !emp) box.append(h("div", { class: "empty-card", html: "<b>No trusted computers</b>Add the computer you want to share with or use devices from." })); else if (st.trusted.length && emp) emp.remove();
}

// ------------------------------------------------------------------ Health
const hl = vEls.health; hl.innerHTML = `<div class="list" id="checks"></div><div id="summary" style="margin-top:18px"></div>`;
async function runDoctor() {
  S.doctorLoading = true; renderHealth();
  try { const t0 = Date.now(); const r = await call("doctor"); await new Promise((x) => setTimeout(x, Math.max(0, 500 - (Date.now() - t0)))); S.doctor = r; } catch (e) { toast(e.message, "bad"); }
  S.doctorLoading = false; renderHealth(); renderNav();
}
function renderHealth() {
  const box = $("#checks"), sum = $("#summary");
  if (S.doctorLoading) { box.innerHTML = ""; for (let i = 0; i < 5; i++) box.append(h("div", { class: "skeleton", style: `height:62px;border-radius:14px;opacity:${1 - i * 0.15}` })); sum.innerHTML = ""; return; }
  if (!S.doctor) return; box.innerHTML = "";
  S.doctor.forEach((c, i) => box.append(h("div", { class: "card check", style: `animation:fadeln .5s ${i * 60}ms var(--ease) backwards` }, h("div", { class: `mark ${c.status}`, style: `animation-delay:${i * 60 + 120}ms` }, { ok: "✓", warn: "!", fail: "✕", info: "i" }[c.status]), h("div", { style: "flex:1;min-width:0" }, h("b", {}, c.title), h("div", { class: "muted", style: "font-size:13px;user-select:text" }, c.detail), c.fix && c.status !== "ok" ? h("div", { class: "fix" }, c.fix) : null, c.id === "usbip-win2" && c.status !== "ok" ? h("div", { style: "margin-top:10px" }, h("button", { class: "btn sm primary", onclick: async () => { await installDriver(); runDoctor?.(); } }, "Install driver")) : null))));
  const f = S.doctor.filter((c) => c.status === "fail").length, w = S.doctor.filter((c) => c.status === "warn").length;
  sum.innerHTML = ""; sum.append(h("span", { class: `chip ${f ? "bad" : w ? "warn" : "ok"}`, style: "height:28px;font-size:12.5px", html: f ? `${f} problem${f > 1 ? "s" : ""} to fix` : w ? `Working, with ${w} warning${w > 1 ? "s" : ""}` : "Everything looks good" }));
}

// ------------------------------------------------------------------ Logs
const lg = vEls.logs; lg.innerHTML = `<div class="card logbox sel" id="logbox"></div>`;
function addLog(line) {
  const box = $("#logbox"); const m = /^(\d\d:\d\d:\d\d) (.*)$/.exec(line) || [0, "", line];
  const near = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  box.append(h("div", { class: "ln" + (/fail|error|refus|denied|lost/i.test(m[2]) ? " err" : /attached|imported|trusted|listening/i.test(m[2]) ? " ok" : "") }, h("span", { class: "t" }, m[1]), m[2]));
  while (box.children.length > 600) box.firstChild.remove();
  if (near) box.scrollTop = box.scrollHeight;
}
function scrollLogs(force) { const b = $("#logbox"); if (force) setTimeout(() => (b.scrollTop = b.scrollHeight), 50); }

// ------------------------------------------------------------------ chrome: nav badges, daemon chip, offline
function renderNav() {
  const st = S.status; const badge = (id, n) => { const b = $("#b-" + id); const t = n ? String(n) : ""; if (b.textContent !== t) { b.textContent = t; b.classList.add("pop"); setTimeout(() => b.classList.remove("pop"), 250); } };
  badge("share", st ? st.shares.length : 0); badge("connect", st ? st.attachments.length : 0); badge("health", S.doctor ? S.doctor.filter((c) => c.status === "warn" || c.status === "fail").length : 0);
}
function renderChrome() {
  const st = S.status; const dot = $("#daemon-dot");
  dot.className = "dot " + (!S.connected ? "bad" : st && st.export.state === "error" ? "warn" : "ok");
  setText($("#daemon-title"), S.connected ? "Daemon running" : "Daemon offline");
  setText($("#daemon-sub"), S.connected && st ? `${st.hostname} · up ${dur(st.uptime)}` : "not reachable");
  const off = $("#offline"); off.classList.toggle("show", !S.connected);
  if (!S.connected && !off.firstChild) {
    off.append(h("div", { class: "box" }, h("div", { class: "big", html: ic("plug") }), h("h2", {}, "The periwire daemon isn't running"), h("p", {}, "This app is the control panel; the daemon does the work. Start it to share and use devices."),
      h("div", { class: "row", style: "justify-content:center" }, h("button", { class: "btn primary", onclick: () => startDaemon(false).then(() => toast("Starting…", "info", 2500), (e) => toast(e.message, "bad", 7000)) }, "Start daemon"), h("button", { class: "btn", onclick: () => startDaemon(true).then(() => toast("Waiting for authorisation…", "info", 3000), (e) => toast(e.message, "bad", 7000)) }, "Start with admin rights")),
      h("p", { class: "faint", style: "margin:14px 0 0;font-size:12.5px" }, "Admin rights are needed to import devices on Linux. For an always-on setup, install the service:"), h("code", {}, "sudo periwire service install")));
  }
}
function renderAll() {
  renderChrome(); if (!S.status) return; renderNav(); driverBanner(); renderOnboard();
  ({ overview: renderOverview, share: renderShare, connect: renderConnect, trust: renderTrust })[S.view]?.(); // health/logs render only on their own events
  if (S.view === "share") renderActions();
}

// ------------------------------------------------------------------ boot
on("status", (st) => { S.status = st; updateRates(st); renderAll(); });
on("connection", async (c) => { S.connected = c; renderActions(); if (c) { await refresh(); try { (await call("logs", { lines: 200 })).forEach(addLog); } catch {} } renderChrome(); });
on("event", (ev) => { if (ev.event === "log") addLog(ev.data.line); });
on("toast", (t) => toast(t.message, t.ok ? "ok" : "bad"));
setInterval(async () => { if (S.connected && (S.view === "share" || S.view === "overview")) { try { S.devices = await call("devices"); renderAll(); } catch {} } }, 3000);
if (demo) document.body.append(h("div", { class: "demo-banner" }, "Demo mode — simulated devices"));
start();
go(location.hash.slice(1) && VIEWS.some((v) => v.id === location.hash.slice(1)) ? location.hash.slice(1) : "overview");
let rz; window.addEventListener("resize", () => { clearTimeout(rz); rz = setTimeout(() => { topoSig = ""; renderAll(); const i = VIEWS.findIndex((x) => x.id === S.view); const b = nav.children[i + 1]; if (b) pill.style.transform = `translateY(${b.offsetTop}px)`; }, 120); });

$("#theme-btn").addEventListener("click", (e) => openPicker(e.currentTarget));
$("#help-btn").addEventListener("click", () => openHelp());
