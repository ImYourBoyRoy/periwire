// periwire desktop app: a thin shell around the daemon's control API.
// The window (gui/ui) talks to the daemon through the `api` command; this process also keeps a
// tray icon in sync and relays daemon events to the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
#[cfg(target_os = "linux")]
mod launcher;
mod tray;

use daemon::{api, ensure_daemon, stop_daemon_and_exit, OWNER};
use periwire_client::Client;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::image::Image;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tray::{build_menu, refresh_tray, tray_view, TRAY};
/// Started by a daemon (`--follow-daemon`) just to show the icon: leave again when that daemon is gone, unless somebody opened the window.
static FOLLOW_DAEMON: AtomicBool = AtomicBool::new(false);
/// Whether the tray icon exists. Without one, closing the window must quit: hiding it would leave nothing to bring it back.
static TRAY_OK: AtomicBool = AtomicBool::new(false);

/// How long a daemon may be gone before an app that only followed it leaves (longer if it never saw the daemon at all).
const FOLLOW_GRACE: Duration = Duration::from_secs(10);
const FOLLOW_NEVER_SEEN: Duration = Duration::from_secs(30);

/// True when this app should now leave because the daemon it followed is gone.
fn should_follow_out(follow: bool, seen_daemon: bool, gone_for: Duration, window_visible: bool) -> bool {
    follow && !window_visible && gone_for >= if seen_daemon { FOLLOW_GRACE } else { FOLLOW_NEVER_SEEN }
}

fn show_main(app: &AppHandle) {
    FOLLOW_DAEMON.store(false, Ordering::Relaxed); // somebody is using the app now: it stays until quit
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
    let (mut seen_daemon, mut gone_since) = (false, std::time::Instant::now());
    loop {
        if let Ok(mut c) = Client::connect_default().await {
            seen_daemon = true;
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
                            tokio::time::sleep(Duration::from_millis(120)).await;
                            // coalesce bursts
                        }
                        Ok(None) => break,
                        Err(_) => {}
                    }
                }
            }
        }
        // (a connection that just ended counts as gone from now)
        if announced == Some(true) {
            gone_since = std::time::Instant::now();
        }
        announce_connection(&app, &mut announced, false);
        refresh_tray(&app, None);
        let visible = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
        if should_follow_out(FOLLOW_DAEMON.load(Ordering::Relaxed), seen_daemon, gone_since.elapsed(), visible) {
            app.exit(0);
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn main() {
    // `--register-launcher`: add the app to the launcher (Linux, when not installed from a package) and leave; the same happens at every start.
    #[cfg(target_os = "linux")]
    {
        if std::env::args().any(|a| a == "--register-launcher") {
            match launcher::ensure() {
                Some(o) => println!("launcher: {o:?}"),
                None => std::process::exit(1),
            }
            return;
        }
        if !cfg!(debug_assertions) {
            launcher::ensure();
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A daemon starting the icon again (`--hidden`) must not pop the window up over what the user is doing.
            if !args.iter().any(|a| a == "--hidden") {
                show_main(app);
            }
        }))
        .invoke_handler(tauri::generate_handler![daemon::api, daemon::start_daemon, daemon::app_version, daemon::open_url])
        .setup(|app| {
            let handle = app.handle().clone();
            let (items, _, _) = tray_view(None, false);
            let menu = build_menu(&handle, &items)?;
            FOLLOW_DAEMON.store(std::env::args().any(|a| a == "--follow-daemon"), Ordering::Relaxed);
            let tray = TrayIconBuilder::with_id("main")
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
                        "quit-stop" => {
                            tauri::async_runtime::spawn(stop_daemon_and_exit(app.clone()));
                        }
                        "start" => {
                            tauri::async_runtime::spawn(async {
                                let _ = ensure_daemon(false).await;
                            });
                        }
                        "start-root" => {
                            tauri::async_runtime::spawn(async {
                                let _ = ensure_daemon(true).await;
                            });
                        }
                        _ => {
                            if let Some((kind, n)) = id.split_once(':') {
                                let Ok(n) = n.parse::<u64>() else { return };
                                let (method, params) = match kind {
                                    "eject" => ("detach", json!({"id": n})),
                                    "kick" => ("kick", json!({"session": n})),
                                    _ => return,
                                };
                                let app = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    let r = api(method.into(), Some(params)).await;
                                    let _ = app.emit(
                                        "periwire-toast",
                                        match r {
                                            Ok(_) => json!({"ok": true, "message": "Done"}),
                                            Err(e) => json!({"ok": false, "message": e["message"]}),
                                        },
                                    );
                                });
                            }
                        }
                    }
                })
                .build(app);
            match tray {
                Ok(_) => TRAY_OK.store(true, Ordering::Relaxed),
                // A desktop without a tray (no status-notifier host) still gets the window; closing it then quits.
                Err(e) => eprintln!("periwire: no tray icon here ({e}); the window stays the only way in"),
            }
            if std::env::args().any(|a| a == "--hidden") && TRAY_OK.load(Ordering::Relaxed) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            tauri::async_runtime::spawn(watch_daemon(handle));
            Ok(())
        })
        .on_window_event(|w, ev| {
            // Closing the window keeps the app (and its tray icon) running; the tray's menu quits it.
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if TRAY_OK.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = w.hide();
                } else if OWNER.lock().unwrap().has_daemon() {
                    // No tray to come back to: closing the window quits the app, and the daemon it started goes with it.
                    api.prevent_close();
                    tauri::async_runtime::spawn(stop_daemon_and_exit(w.app_handle().clone()));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running periwire");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_only_followed_the_daemon_leaves_with_it() {
        let g = Duration::from_secs(11);
        assert!(should_follow_out(true, true, g, false), "daemon gone, nobody looking: leave");
        assert!(!should_follow_out(true, true, Duration::from_secs(3), false), "not for a short gap (the daemon restarting)");
        assert!(!should_follow_out(true, true, g, true), "never while the window is open");
        assert!(!should_follow_out(false, true, g, false), "an app somebody started stays");
        assert!(!should_follow_out(true, false, g, false), "a daemon not yet seen gets longer");
        assert!(should_follow_out(true, false, Duration::from_secs(31), false));
    }
}
