// Transport to the periwire daemon. Inside the desktop app it goes through Tauri; in a plain browser
// (or with ?demo) it runs against a built-in simulated daemon so the UI can be explored safely.
const T = window.__TAURI__;
export const demo = !T || new URLSearchParams(location.search).has("demo");

const listeners = { status: new Set(), event: new Set(), connection: new Set(), toast: new Set() };
const emit = (k, v) => listeners[k].forEach((f) => f(v));
export const on = (k, f) => { listeners[k].add(f); return () => listeners[k].delete(f); };

// ---------------------------------------------------------------- real transport
async function realCall(method, params = {}) {
  try { return await T.core.invoke("api", { method, params }); }
  catch (e) { throw typeof e === "string" ? { code: "failed", message: e } : e; }
}
function startReal() {
  T.event.listen("periwire-status", (e) => emit("status", e.payload));
  T.event.listen("periwire-event", (e) => emit("event", e.payload));
  T.event.listen("periwire-connection", (e) => emit("connection", e.payload.connected));
  T.event.listen("periwire-toast", (e) => emit("toast", e.payload));
}

// ---------------------------------------------------------------- simulated daemon
const now = () => Math.floor(Date.now() / 1000);
const winDemo = new URLSearchParams(location.search).has("win"); // ?demo&win simulates Windows without the driver
const sim = (() => {
  const t0 = now() - 5400;
  const st = {
    version: "0.1.0", os: winDemo ? "windows" : "linux", arch: "x86_64", hostname: "studio-laptop", pid: 4242, daemon: true, started: t0, root: true,
    fingerprint: "7f3c9a1e5b28d4406ac1e9d37b52f8a0c4d6e1b93a7f205d8c4e6b1a9f30d7e2",
    export: { enabled: true, listen: "0.0.0.0:3241", state: "listening", error: null },
    import: winDemo ? { available: false, reason: "the Windows virtual USB host controller driver is not installed: run `periwire driver install` as Administrator" } : { available: true, reason: null },
    shares: [{ spec: "0781:5583", present: true }, { spec: "05ac:12ab", present: true }],
    sessions: [{ id: 1, peer: "10.0.0.31:51200", peer_name: "build-mac", busid: "1-1", vid: "05ac", pid: "12ab", info: { busid: "1-1", vid: "05ac", pid: "12ab", display: "Test iPad", kind: "apple-ipad", kind_label: "iPad", speed: "high (480 Mb/s)" }, since: now() - 840, urbs: 12044, bytes_in: 2.1e6, bytes_out: 9.4e7 }],
    attachments: [{ id: 1, addr: "media-pc.local:3241", busid: "3-2", auto: true, state: "attached", error: null, info: { busid: "3-2", vid: "1050", pid: "0407", display: "Studio YubiKey", kind: "security-key", kind_label: "Security key", speed: "full (12 Mb/s)" }, vid: "1050", pid: "0407", speed: "full (12 Mb/s)", port: 0, local_busid: "5-1", mounts: [], created: now() - 3600, attached_at: now() - 3500, retries: 0, bytes_in: 1.8e5, bytes_out: 4.4e4 }],
    connections: [{ name: "Capture card", addr: "media-pc.local:3241", busid: "3-2", auto: true }, { name: "Label printer", addr: "office-nuc.local:3241", busid: "1-4", auto: false }],
    trusted: [{ fingerprint: "a41c0e77d93b6f58c2e1049ab7d36f81e5c20d9b47a8e3f6120c5d98b4e7a613", name: "build-mac" }, { fingerprint: "19be83d7a02c5f6e48b1d09c7e3a65f24d8b01c97e5a3f40b6d28e1c7a95f304", name: "media-pc" }],
  };
  const D = (busid, vid, pid, manufacturer, product, kind, kind_label, speed, o = {}) => ({ busid, vid, pid, manufacturer, product, serial: null, key: `${vid}:${pid}`, label: null, kind, kind_label, display: product ? (manufacturer && !product.includes(manufacturer) ? `${manufacturer} ${product}` : product) : kind_label, speed, class: kind, interfaces: [kind], shared: false, in_use_by: null, mounted: [], ...o });
  const devices = [
    D("2-2", "0781", "5583", "SanDisk", "Ultra Fit", "storage", "Storage drive", "super (5 Gb/s)", { shared: true, label: "Photo backup stick", display: "Photo backup stick", key: "0781:5583:demo1" }),
    D("1-1", "05ac", "12ab", "Apple Inc.", "iPad", "apple-ipad", "iPad", "high (480 Mb/s)", { shared: true, in_use_by: "build-mac", label: "Test iPad", display: "Test iPad" }),
    D("1-3", "046d", "085e", "Logitech", "BRIO 4K Webcam", "webcam", "Camera / webcam", "super (5 Gb/s)"),
    D("1-5", "1050", "0407", "Yubico", "YubiKey 5 NFC", "security-key", "Security key", "full (12 Mb/s)"),
    D("3-1", "0fd9", "0060", "Elgato", "Stream Deck", "hid", "Input device", "full (12 Mb/s)"),
    D("2-4", "0951", "1666", "Kingston", "DataTraveler 3.0", "storage", "Storage drive", "super (5 Gb/s)", { mounted: ["/dev/sdc1 on /run/media/demo/KINGSTON"] }),
    D("1-7", "05ac", "1227", "Apple Inc.", "Apple Mobile Device (DFU Mode)", "apple-dfu", "Apple device in DFU mode", "high (480 Mb/s)", { display: "Apple device in DFU mode" }),
    D("1-9", "18d1", "4ee7", "Google", "Pixel 8", "android", "Android device", "super (5 Gb/s)"),
  ];
  const R = (busid, vid, pid, manufacturer, product, kind, kind_label, speed, label = null) => { const d = D(busid, vid, pid, manufacturer, product, kind, kind_label, speed, { label }); if (label) d.display = label; delete d.shared; delete d.mounted; delete d.in_use_by; return d; };
  const remote = {
    "media-pc.local:3241": [
      R("3-2", "1050", "0407", "Yubico", "YubiKey 5 NFC", "security-key", "Security key", "full (12 Mb/s)", "Studio YubiKey"),
      R("3-4", "0bda", "8153", "Realtek", "USB 10/100/1000 LAN", "network", "Network adapter", "super (5 Gb/s)"),
    ],
    "build-mac.local:3241": [
      R("1-2", "05ac", "12a8", "Apple Inc.", "iPhone", "apple-iphone", "iPhone", "high (480 Mb/s)", "Sam's iPhone"),
      R("1-6", "1234", "5678", "Example Corp", "USB Flash Drive", "storage", "Storage drive", "super (5 Gb/s)"),
      R("2-1", "05ac", "1281", "Apple Inc.", "Apple Mobile Device (Recovery Mode)", "apple-recovery", "Apple device in Recovery mode", "high (480 Mb/s)"),
      R("2-3", "2341", "0043", "Arduino", "Uno R3", "serial", "Serial / debug adapter", "full (12 Mb/s)"),
    ],
  };
  const logs = [];
  const log = (m) => { const d = new Date(); logs.push(`${d.toTimeString().slice(0, 8)} ${m}`); if (logs.length > 300) logs.shift(); emit("event", { event: "log", data: { line: logs[logs.length - 1] } }); };
  [["periwire 0.1.0 starting; identity 7f3c9a1e5b28…"], ["exporter listening on 0.0.0.0:3241"], ["control socket /run/periwire/periwire.sock"], ["auto-attaching 3-2 from media-pc.local:3241"], ["attached 3-2 1050:0407 from media-pc.local:3241 on vhci port 0"], ["[10.0.0.31:51200] imported 1-1 05ac:12ab"]].forEach(([m]) => log(m));
  let nextAtt = 2;
  const delay = (ms) => new Promise((r) => setTimeout(r, ms));
  const fail = (code, message) => { throw { code, message }; };
  const specOf = (d) => [d.busid, `${d.vid}:${d.pid}`];

  async function call(method, p = {}) {
    await delay(120 + Math.random() * 160);
    switch (method) {
      case "status": return { ...st, uptime: now() - st.started, shares: devices.filter((d) => d.shared).map((d) => ({ spec: `${d.vid}:${d.pid}`, present: true })) };
      case "devices": return devices.map((d) => ({ ...d }));
      case "identity": return { fingerprint: st.fingerprint, hostname: st.hostname };
      case "device.label": { const d = devices.find((d) => d.key === p.key); if (!d) fail("not_found", "no such device"); d.label = p.label || null; d.display = p.label || (d.product ? d.product : d.kind_label); log(p.label ? `labelled ${d.busid} as ${p.label}` : `cleared label of ${d.busid}`); return { key: p.key }; }
      case "identity.reset": { if (!p.confirm) fail("invalid", "pass confirm=true"); st.fingerprint = Array.from({ length: 64 }, () => "0123456789abcdef"[Math.floor(Math.random() * 16)]).join(""); st.sessions = []; devices.forEach((d) => (d.in_use_by = null)); log("identity reset"); return { fingerprint: st.fingerprint }; }
      case "share.add": { const d = devices.find((d) => specOf(d).includes(p.spec)); if (!d) fail("invalid", `no device matches ${p.spec}`); if (d.mounted.length) log(`warning: ${d.busid} is mounted locally`); const ro = !!p.read_only; const changed = !!d.shared_read_only !== ro; d.shared = true; d.shared_read_only = ro; let kicked = 0; if (changed && d.in_use_by) { kicked = 1; d.in_use_by = null; st.sessions = st.sessions.filter((s) => s.busid !== d.busid); } log(`sharing ${p.spec}${ro ? " (read-only for everyone)" : ""}`); return { spec: p.spec, read_only: ro, disconnected: kicked }; }
      case "share.remove": { const d = devices.find((d) => specOf(d).includes(p.spec)); if (!d) fail("not_found", `${p.spec} is not shared`); d.shared = false; d.shared_read_only = false; const had = d.in_use_by; d.in_use_by = null; st.sessions = st.sessions.filter((s) => s.busid !== d.busid); log(`stopped sharing ${p.spec}`); return { spec: p.spec, disconnected: had ? 1 : 0 }; }
      case "remote.list": { await delay(500); const a = p.addr.includes(":") ? p.addr : p.addr + ":3241"; if (!remote[a]) { if (a.startsWith("untrusted")) fail("failed", "TLS handshake failed (is that machine's fingerprint trusted? see `periwire trust --help`)"); fail("failed", `connecting to ${a}: connection refused`); } return remote[a]; }
      case "attach": {
        await delay(900); const a = p.addr.includes(":") ? p.addr : p.addr + ":3241";
        if (st.attachments.some((x) => x.addr === a && x.busid === p.busid)) fail("busy", `${p.busid} from ${a} is already attached`);
        const dev = (remote[a] || []).find((d) => d.busid === p.busid); if (!dev) fail("failed", `${a} refused to export ${p.busid}`);
        const att = { id: nextAtt++, addr: a, busid: p.busid, auto: !!p.auto, read_only: !!p.read_only, state: "attached", error: null, info: dev, vid: dev.vid, pid: dev.pid, speed: dev.speed, port: st.attachments.length, local_busid: `5-${st.attachments.length + 1}`, mounts: [], created: now(), attached_at: now(), retries: 0, bytes_in: 0, bytes_out: 0 };
        st.attachments.push(att);
        if (p.auto && !st.connections.some((c) => c.addr === a && c.busid === p.busid)) st.connections.push({ name: p.name || "", addr: a, busid: p.busid, auto: true, read_only: !!p.read_only });
        log(`attached ${p.busid} ${dev.vid}:${dev.pid} from ${a}${p.read_only ? " (read-only)" : ""}`); return att;
      }
      case "detach": {
        const a = st.attachments.find((x) => x.id === p.id); if (!a) fail("not_found", "no such attachment");
        if (a.mounts.length && !p.force) fail("busy", `${a.mounts[0]} is busy (files are open on it). Close them and retry, or force the eject.`);
        await delay(700); st.attachments = st.attachments.filter((x) => x.id !== p.id); const c = st.connections.find((c) => c.addr === a.addr && c.busid === a.busid); if (c) c.auto = false;
        log(`detached ${a.busid}`); return { detached: true, id: a.id, unmounted: a.mounts };
      }
      case "kick": { const s = st.sessions.find((x) => x.id === p.session); if (!s) fail("not_found", "no such session"); st.sessions = st.sessions.filter((x) => x !== s); const d = devices.find((d) => d.busid === s.busid); if (d) d.in_use_by = null; log(`session ${s.id} on ${s.busid} disconnected by operator`); return { disconnected: 1 }; }
      case "trust.list": return st.trusted;
      case "trust.probe": { await delay(600); if (!/\w/.test(p.addr)) fail("failed", "bad address"); return { addr: p.addr, fingerprint: "c0ffee5d1be3b9a1d27f6054c8ae13b97d42f6805ce1a9374bd6280f5c1e7a93", already_trusted: false }; }
      case "trust.add": { if (!/^[0-9a-f]{64}$/i.test(p.fingerprint)) fail("invalid", "fingerprint must be 64 hex characters"); st.trusted.push({ fingerprint: p.fingerprint.toLowerCase(), name: p.name || "peer" }); log(`trusted ${p.fingerprint.slice(0, 12)}`); return { fingerprint: p.fingerprint }; }
      case "trust.remove": { st.trusted = st.trusted.filter((t) => t.fingerprint !== p.fingerprint); return { removed: p.fingerprint }; }
      case "connection.remove": { st.connections = st.connections.filter((c) => !(c.addr === p.addr && c.busid === p.busid)); return { removed: true }; }
      case "driver.status": return winDemo ? { supported: true, status: { ready: st.import.available, embedded_version: "0.9.8.1", admin: true, vc_runtime: true, packages: [], device: null, tools: null } } : { supported: false, note: "only Windows needs a separate driver" };
      case "driver.install": { if (!p.confirm) fail("invalid", "needs confirm=true"); await delay(2600); st.import = { available: true, reason: null }; logs.push(`${new Date().toTimeString().slice(0, 8)} installed the Windows virtual USB host controller driver`); return { reboot: false, status: { ready: true } }; }
      case "export.set": { st.export.enabled = p.enabled; st.export.state = p.enabled ? "listening" : "disabled"; if (!p.enabled) { st.sessions = []; devices.forEach((d) => (d.in_use_by = null)); } log(p.enabled ? `exporter listening on ${st.export.listen}` : "exporter stopped; devices return to this machine"); return { enabled: p.enabled }; }
      case "doctor": { await delay(700); return [
        { id: "identity", title: "Identity", status: "ok", detail: `fingerprint ${st.fingerprint.slice(0, 16)}…`, fix: null },
        { id: "libusb", title: "USB access (built-in libusb)", status: "ok", detail: `${devices.length} shareable device(s) visible`, fix: null },
        { id: "usb-perms", title: "Permission to claim devices", status: "ok", detail: "running as root", fix: null },
        { id: "import", title: "Importing devices", status: "ok", detail: "vhci_hcd loaded", fix: null },
        ...(winDemo && !st.import.available ? [{ id: "usbip-win2", title: "Windows virtual USB host controller", status: "warn", detail: "not installed: needed to use remote devices on Windows", fix: "run `periwire driver install` as Administrator (the signed driver is carried inside this program)" }] : []),
        { id: "export", title: "Exporter", status: "ok", detail: `listening on ${st.export.listen}`, fix: null },
        { id: "trust", title: "Trusted peers", status: "ok", detail: `${st.trusted.length} trusted`, fix: null },
        { id: "share:2-4", title: "Shared device 2-4", status: "warn", detail: "mounted locally (/run/media/demo/KINGSTON); peers can't take it until it's unmounted", fix: "unmount it (or eject it) first" },
      ]; }
      case "logs": return logs.slice(-(p.lines || 100));
      case "shutdown": return { stopping: true };
      default: fail("not_found", `unknown method ${method}`);
    }
  }
  setInterval(() => { // live counters
    st.attachments.forEach((a) => { if (a.state === "attached") { a.bytes_in += Math.floor(Math.random() * 90000 + (a.id % 2 ? 40000 : 400000)); a.bytes_out += Math.floor(Math.random() * 30000); } });
    st.sessions.forEach((s) => { s.bytes_out += Math.floor(Math.random() * 600000 + 200000); s.bytes_in += Math.floor(Math.random() * 40000); s.urbs += 70; });
    emit("status", { ...st, uptime: now() - st.started, shares: devices.filter((d) => d.shared).map((d) => ({ spec: `${d.vid}:${d.pid}`, present: true })) });
  }, 1000);
  if (new URLSearchParams(location.search).has("fresh")) { // ?demo&fresh: a brand-new install, to see the first-run guide
    st.shares = []; st.trusted = []; st.attachments = []; st.connections = []; st.sessions = [];
    devices.forEach((d) => { d.shared = false; d.in_use_by = null; });
  }
  return { call, st, log };
})();

// ---------------------------------------------------------------- public API
export const call = demo ? (m, p) => sim.call(m, p) : realCall;
export async function startDaemon(privileged) {
  if (demo) return;
  try { await T.core.invoke("start_daemon", { privileged }); } catch (e) { throw typeof e === "string" ? { message: e } : e; }
}
export function start() {
  if (demo) { setTimeout(() => emit("connection", true), 50); return; }
  startReal();
}
