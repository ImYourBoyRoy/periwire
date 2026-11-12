// Six themes: 3 dark, 3 light, plus "auto" (follows the OS). Choice is remembered per device.
export const THEMES = [
  { id: "clay",   name: "Clay",   mode: "dark",  bg: "#121110", ac: "#d9895d" },
  { id: "moss",   name: "Moss",   mode: "dark",  bg: "#0f1311", ac: "#86bd9b" },
  { id: "plum",   name: "Plum",   mode: "dark",  bg: "#130f15", ac: "#c49ad6" },
  { id: "linen",  name: "Linen",  mode: "light", bg: "#f7f3ec", ac: "#a9692a" },
  { id: "rose",   name: "Rose",   mode: "light", bg: "#f9f1f2", ac: "#b04e6c" },
  { id: "lagoon", name: "Lagoon", mode: "light", bg: "#eff6f4", ac: "#23796f" },
];
const KEY = "periwire.theme";
const mq = window.matchMedia ? matchMedia("(prefers-color-scheme: light)") : null;
let choice = "auto";
try { choice = localStorage.getItem(KEY) || "auto"; } catch {}
if (choice !== "auto" && !THEMES.some((t) => t.id === choice)) choice = "auto";

export const current = () => choice;
export function apply(c = choice) {
  choice = c;
  const id = c === "auto" ? (mq && mq.matches ? "linen" : "clay") : c;
  document.documentElement.dataset.theme = id;
  const t = THEMES.find((x) => x.id === id);
  const meta = document.querySelector('meta[name="theme-color"]'); if (meta && t) meta.content = t.bg;
  try { localStorage.setItem(KEY, c); } catch {}
  document.querySelectorAll(".sw[data-id]").forEach((b) => b.classList.toggle("on", b.dataset.id === c));
}
if (mq) mq.addEventListener?.("change", () => { if (choice === "auto") apply("auto"); });
apply(choice);

export function openPicker(anchor) {
  const old = document.getElementById("theme-pop"); if (old) { old.remove(); return; }
  const pop = document.createElement("div"); pop.id = "theme-pop"; pop.className = "theme-pop"; pop.setAttribute("role", "dialog"); pop.setAttribute("aria-label", "Theme");
  const sw = (t) => `<button class="sw" data-id="${t.id}" title="${t.name}" aria-label="${t.name} (${t.mode})"><i style="background:${t.bg}"><b style="background:${t.ac}"></b></i><span>${t.name}</span></button>`;
  pop.innerHTML =
    `<div class="tp-row"><small>Dark</small><div>${THEMES.filter((t) => t.mode === "dark").map(sw).join("")}</div></div>` +
    `<div class="tp-row"><small>Light</small><div>${THEMES.filter((t) => t.mode === "light").map(sw).join("")}</div></div>` +
    `<button class="sw auto" data-id="auto"><i class="split"></i><span>Match system</span></button>`;
  document.body.append(pop);
  const r = anchor.getBoundingClientRect();
  pop.style.top = Math.min(innerHeight - pop.offsetHeight - 12, r.bottom + 8) + "px";
  pop.style.left = Math.max(12, Math.min(innerWidth - pop.offsetWidth - 12, r.right - pop.offsetWidth)) + "px";
  pop.querySelectorAll(".sw").forEach((b) => b.classList.toggle("on", b.dataset.id === choice));
  pop.addEventListener("click", (e) => { const b = e.target.closest(".sw"); if (b) apply(b.dataset.id); });
  const close = (e) => { if (!pop.contains(e.target) && !anchor.contains(e.target)) { pop.remove(); document.removeEventListener("pointerdown", close, true); } };
  setTimeout(() => document.addEventListener("pointerdown", close, true), 0);
  document.addEventListener("keydown", function k(e) { if (e.key === "Escape") { pop.remove(); document.removeEventListener("keydown", k); } });
}
