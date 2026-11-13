// periwire desktop app: a thin shell around the daemon's control API.
// The window (gui/ui) talks to the daemon through the `api` command; this process also keeps a
// tray icon in sync and relays daemon events to the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use periwire_client::Client;

const TRAY: &[u8] = include_bytes!("../icons/tray.png");
const TRAY_ACTIVE: &[u8] = include_bytes!("../icons/tray-active.png");
const TRAY_WARN: &[u8] = include_bytes!("../icons/tray-warn.png");

fn err(code: &str, message: String) -> Value {
    json!({"code": code, "message": message})
}

/// Call a daemon method. A fresh connection per call keeps this trivially robust across daemon restarts.
#[tauri::command]
async fn api(method: String, params: Option<Value>) -> Result<Value, Value> {
    let mut c = Client::connect_default().await.map_err(|e| err("not_running", e.to_string()))?;
    c.call(&method, params.unwrap_or(json!({}))).await.map_err(|e| err(&e.code, e.message))
}

fn find_periwire() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "periwire.exe" } else { "periwire" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.exists()))
}

/// Start the daemon. `privileged` asks for administrator rights through the OS (needed on Linux to import devices).
#[tauri::command]
fn start_daemon(privileged: bool) -> Result<(), Value> {
    let bin = find_periwire().ok_or_else(|| err("failed", "could not find the `periwire` program next to this app or in PATH".into()))?;
    let mut cmd = if privileged && cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("pkexec");
        c.arg(&bin);
        c
    } else {
        std::process::Command::new(&bin)
    };
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| err("failed", format!("could not start the daemon: {e}")))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// One line of the tray menu, as plain data so that "did the menu change?" is a simple comparison.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Item {
    /// A greyed-out heading.
    Header(String),
    /// A clickable entry: its id (what the click handler matches on) and its label.
    Action(String, String),
    Separator,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Look {
    Idle,
    Active,
    Warn,
}

/// What the tray shows for a daemon state (`None` = the daemon is not running): the menu lines, which icon, and the tooltip.
fn tray_view(st: Option<&Value>) -> (Vec<Item>, Look, String) {
    let mut items = Vec::new();
    let look;
    let tip;
    match st {
        None => {
            items.push(Item::Header("periwire: daemon not running".into()));
            items.push(Item::Action("start".into(), "Start daemon".into()));
            if cfg!(target_os = "linux") {
                items.push(Item::Action("start-root".into(), "Start daemon (can import devices)…".into()));
            }
            look = Look::Warn;
            tip = "periwire: daemon not running".to_string();
        }
        Some(st) => {
            let atts = st["attachments"].as_array().cloned().unwrap_or_default();
            let sess = st["sessions"].as_array().cloned().unwrap_or_default();
            let shares = st["shares"].as_array().map(|a| a.len()).unwrap_or(0);
            items.push(Item::Header(format!("periwire · {} imported · {} shared", atts.len(), shares)));
            if !atts.is_empty() {
                items.push(Item::Separator);
            }
            for a in &atts {
                let label = format!("Eject {} {}:{}  ({})", s(a, "busid"), s(a, "vid"), s(a, "pid"), s(a, "state"));
                items.push(Item::Action(format!("eject:{}", a["id"]), label));
            }
            if !sess.is_empty() {
                items.push(Item::Separator);
            }
            for x in &sess {
                let who = if s(x, "peer_name").is_empty() { s(x, "peer") } else { s(x, "peer_name") };
                items.push(Item::Action(format!("kick:{}", x["id"]), format!("Disconnect {} (used by {who})", s(x, "busid"))));
            }
            let problem = st["export"]["state"] == "error";
            look = if problem { Look::Warn } else if atts.len() + sess.len() > 0 { Look::Active } else { Look::Idle };
            tip = format!("periwire: {} imported, {} shared device(s) in use", atts.len(), sess.len());
        }
    }
    items.push(Item::Separator);
    items.push(Item::Action("open".into(), "Open periwire".into()));
    items.push(Item::Action("quit".into(), "Quit".into()));
    (items, look, tip)
}

fn build_menu(app: &AppHandle, items: &[Item]) -> tauri::Result<Menu<tauri::Wry>> {
    let mut m = MenuBuilder::new(app);
    for it in items {
        m = match it {
            Item::Header(t) => m.item(&MenuItemBuilder::with_id("hdr", t).enabled(false).build(app)?),
            Item::Action(id, label) => m.item(&MenuItemBuilder::with_id(id, label).build(app)?),
            Item::Separator => m.item(&PredefinedMenuItem::separator(app)?),
        };
    }
    m.build()
}

/// What the tray is showing right now. The tray is only touched when this would change: on Linux (and elsewhere) every
/// `set_menu`/`set_icon`/`set_tooltip` makes the shell redraw the item, so re-applying the same state every couple of
/// seconds made the icon flicker.
#[derive(Default)]
struct Shown {
    items: Option<Vec<Item>>,
    look: Option<Look>,
    tip: Option<String>,
}

/// Which of menu, icon and tooltip differ from what is shown. Updates `shown` to the new state.
fn changes(shown: &mut Shown, items: &[Item], look: Look, tip: &str) -> (bool, bool, bool) {
    let menu = shown.items.as_deref() != Some(items);
    let icon = shown.look != Some(look);
    let tooltip = shown.tip.as_deref() != Some(tip);
    if menu {
        shown.items = Some(items.to_vec());
    }
    if icon {
        shown.look = Some(look);
    }
    if tooltip {
        shown.tip = Some(tip.to_string());
    }
    (menu, icon, tooltip)
}

static SHOWN: std::sync::Mutex<Option<Shown>> = std::sync::Mutex::new(None);

fn refresh_tray(app: &AppHandle, st: Option<&Value>) {
    let Some(tray) = app.tray_by_id("main") else { return };
    let (items, look, tip) = tray_view(st);
    let (menu, icon, tooltip) = {
        let mut g = SHOWN.lock().unwrap();
        changes(g.get_or_insert_with(Shown::default), &items, look, &tip)
    };
    if menu {
        if let Ok(m) = build_menu(app, &items) {
            let _ = tray.set_menu(Some(m));
        }
    }
    if icon {
        let bytes = match look {
            Look::Idle => TRAY,
            Look::Active => TRAY_ACTIVE,
            Look::Warn => TRAY_WARN,
        };
        if let Ok(img) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(img));
        }
    }
    if tooltip {
        let _ = tray.set_tooltip(Some(tip));
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Keep a connection to the daemon: mirror its state to the tray and the window, forever.
/// Tell the window the daemon came or went, but only when that is news: while the daemon is down this loop runs every 2 s, and
/// repeating "offline" made the window redraw its offline screen over and over.
fn announce_connection(app: &AppHandle, last: &mut Option<bool>, connected: bool) {
    if *last != Some(connected) {
        *last = Some(connected);
        let _ = app.emit("periwire-connection", json!({"connected": connected}));
    }
}

async fn watch_daemon(app: AppHandle) {
    let mut announced: Option<bool> = None;
    loop {
        if let Ok(mut c) = Client::connect_default().await {
            announce_connection(&app, &mut announced, true);
            if c.call("subscribe", json!({})).await.is_ok() {
                loop {
                    match c.call("status", json!({})).await {
                        Ok(st) => {
                            refresh_tray(&app, Some(&st));
                            let _ = app.emit("periwire-status", &st);
                        }
                        Err(_) => break,
                    }
                    // Wait for a change, but re-read at least every 2 s so byte counters stay live.
                    match tokio::time::timeout(Duration::from_secs(2), c.next_event()).await {
                        Ok(Some(ev)) => {
                            if ev["event"] != "state" {
                                let _ = app.emit("periwire-event", &ev);
                            }
                            tokio::time::sleep(Duration::from_millis(120)).await; // coalesce bursts
                        }
                        Ok(None) => break,
                        Err(_) => {}
                    }
                }
            }
        }
        announce_connection(&app, &mut announced, false);
        refresh_tray(&app, None);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .invoke_handler(tauri::generate_handler![api, start_daemon])
        .setup(|app| {
            let handle = app.handle().clone();
            let (items, _, _) = tray_view(None);
            let menu = build_menu(&handle, &items)?;
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(TRAY)?)
                .tooltip("periwire")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_main(tray.app_handle());
                    }
                })
                .on_menu_event(|app, ev| {
                    let id = ev.id().as_ref().to_string();
                    match id.as_str() {
                        "open" => show_main(app),
                        "quit" => app.exit(0),
                        "start" => {
                            let _ = start_daemon(false);
                        }
                        "start-root" => {
                            let _ = start_daemon(true);
                        }
                        _ => {
                            if let Some((kind, n)) = id.split_once(':') {
                                let n = n.parse::<u64>().unwrap_or(0);
                                let (method, params) = match kind {
                                    "eject" => ("detach", json!({"id": n})),
                                    "kick" => ("kick", json!({"session": n})),
                                    _ => return,
                                };
                                let app = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    let r = api(method.into(), Some(params)).await;
                                    let _ = app.emit("periwire-toast", match r {
                                        Ok(_) => json!({"ok": true, "message": "Done"}),
                                        Err(e) => json!({"ok": false, "message": e["message"]}),
                                    });
                                });
                            }
                        }
                    }
                })
                .build(app)?;
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            tauri::async_runtime::spawn(watch_daemon(handle));
            Ok(())
        })
        .on_window_event(|w, ev| {
            // Closing the window keeps the app (and its tray icon) running.
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = w.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running periwire");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(atts: usize, sess: usize) -> Value {
        json!({
            "attachments": (0..atts).map(|i| json!({"id": i, "busid": "1-2", "vid": "1234", "pid": "5678", "state": "attached"})).collect::<Vec<_>>(),
            "sessions": (0..sess).map(|i| json!({"id": i, "busid": "2-1", "peer": "10.0.0.2", "peer_name": ""})).collect::<Vec<_>>(),
            "shares": [], "export": {"state": "listening"}
        })
    }

    #[test]
    fn the_same_state_twice_changes_nothing() {
        let mut shown = Shown::default();
        let (items, look, tip) = tray_view(Some(&status(1, 0)));
        assert_eq!(changes(&mut shown, &items, look, &tip), (true, true, true), "the first state is applied in full");
        for _ in 0..5 {
            assert_eq!(changes(&mut shown, &items, look, &tip), (false, false, false), "re-reading an unchanged daemon must not touch the tray (that was the flicker)");
        }
    }

    #[test]
    fn only_what_changed_is_touched() {
        let mut shown = Shown::default();
        let (i0, l0, t0) = tray_view(Some(&status(0, 0)));
        changes(&mut shown, &i0, l0, &t0);
        // an attachment appears: menu, icon (idle -> active) and tooltip all change
        let (i1, l1, t1) = tray_view(Some(&status(1, 0)));
        assert_eq!(changes(&mut shown, &i1, l1, &t1), (true, true, true));
        // a second one: the menu and tooltip change, the icon stays active
        let (i2, l2, t2) = tray_view(Some(&status(2, 0)));
        assert_eq!(changes(&mut shown, &i2, l2, &t2), (true, false, true));
        // the daemon goes away: warning icon and the start menu
        let (i3, l3, t3) = tray_view(None);
        assert_eq!(l3, Look::Warn);
        assert_eq!(changes(&mut shown, &i3, l3, &t3), (true, true, true));
    }

    #[test]
    fn the_menu_lists_what_can_be_ejected_or_disconnected() {
        let (items, look, _) = tray_view(Some(&status(1, 1)));
        assert_eq!(look, Look::Active);
        let ids: Vec<&str> = items.iter().filter_map(|i| if let Item::Action(id, _) = i { Some(id.as_str()) } else { None }).collect();
        assert_eq!(ids, ["eject:0", "kick:0", "open", "quit"]);
        assert!(matches!(items[0], Item::Header(_)));
    }

    #[test]
    fn an_export_error_shows_the_warning_icon() {
        let mut st = status(0, 0);
        st["export"]["state"] = json!("error");
        assert_eq!(tray_view(Some(&st)).1, Look::Warn);
        assert_eq!(tray_view(Some(&status(0, 0))).1, Look::Idle);
    }
}
