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

fn build_menu(app: &AppHandle, st: Option<&Value>) -> tauri::Result<Menu<tauri::Wry>> {
    let mut m = MenuBuilder::new(app);
    match st {
        None => {
            m = m.item(&MenuItemBuilder::with_id("hdr", "periwire: daemon not running").enabled(false).build(app)?);
            m = m.item(&MenuItemBuilder::with_id("start", "Start daemon").build(app)?);
            if cfg!(target_os = "linux") {
                m = m.item(&MenuItemBuilder::with_id("start-root", "Start daemon (can import devices)…").build(app)?);
            }
        }
        Some(st) => {
            let atts = st["attachments"].as_array().cloned().unwrap_or_default();
            let sess = st["sessions"].as_array().cloned().unwrap_or_default();
            let shares = st["shares"].as_array().map(|a| a.len()).unwrap_or(0);
            m = m.item(&MenuItemBuilder::with_id("hdr", format!("periwire · {} imported · {} shared", atts.len(), shares)).enabled(false).build(app)?);
            if !atts.is_empty() {
                m = m.item(&PredefinedMenuItem::separator(app)?);
            }
            for a in &atts {
                let label = format!("Eject {} {}:{}  ({})", s(a, "busid"), s(a, "vid"), s(a, "pid"), s(a, "state"));
                m = m.item(&MenuItemBuilder::with_id(format!("eject:{}", a["id"]), label).build(app)?);
            }
            if !sess.is_empty() {
                m = m.item(&PredefinedMenuItem::separator(app)?);
            }
            for x in &sess {
                let who = if s(x, "peer_name").is_empty() { s(x, "peer") } else { s(x, "peer_name") };
                m = m.item(&MenuItemBuilder::with_id(format!("kick:{}", x["id"]), format!("Disconnect {} (used by {who})", s(x, "busid"))).build(app)?);
            }
        }
    }
    m = m.item(&PredefinedMenuItem::separator(app)?);
    m = m.item(&MenuItemBuilder::with_id("open", "Open periwire").build(app)?);
    m = m.item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?);
    m.build()
}

fn refresh_tray(app: &AppHandle, st: Option<&Value>) {
    let Some(tray) = app.tray_by_id("main") else { return };
    if let Ok(menu) = build_menu(app, st) {
        let _ = tray.set_menu(Some(menu));
    }
    let (icon, tip) = match st {
        None => (TRAY_WARN, "periwire: daemon not running".to_string()),
        Some(st) => {
            let na = st["attachments"].as_array().map(|a| a.len()).unwrap_or(0);
            let ns = st["sessions"].as_array().map(|a| a.len()).unwrap_or(0);
            let problem = st["export"]["state"] == "error";
            let icon = if problem { TRAY_WARN } else if na + ns > 0 { TRAY_ACTIVE } else { TRAY };
            (icon, format!("periwire: {na} imported, {ns} shared device(s) in use"))
        }
    };
    if let Ok(img) = Image::from_bytes(icon) {
        let _ = tray.set_icon(Some(img));
    }
    let _ = tray.set_tooltip(Some(tip));
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Keep a connection to the daemon: mirror its state to the tray and the window, forever.
async fn watch_daemon(app: AppHandle) {
    loop {
        if let Ok(mut c) = Client::connect_default().await {
            let _ = app.emit("periwire-connection", json!({"connected": true}));
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
        let _ = app.emit("periwire-connection", json!({"connected": false}));
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
            let menu = build_menu(&handle, None)?;
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
