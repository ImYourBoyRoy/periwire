// periwire desktop app: a thin shell around the daemon's control API.
// The window (gui/ui) talks to the daemon through the `api` command; this process also keeps a
// tray icon in sync and relays daemon events to the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod launcher;

use periwire_client::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

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
fn spawn_daemon(privileged: bool) -> Result<(), Value> {
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

/// Which daemon this app started, so that quitting the app can take that one (and only that one) down with it.
/// A daemon started from a terminal, as a service or by an AI assistant belongs to whoever started it.
#[derive(Default)]
struct Ownership {
    /// Unix time at which this app asked for a daemon; the next daemon that reports a start time from then on is the one.
    spawned_at: Option<u64>,
    /// The pid of the daemon this app started.
    pid: Option<u64>,
}

/// A request for a daemon (a password prompt, say) that nobody answers must not claim a daemon started later by somebody else.
const SPAWN_CLAIM_SECS: u64 = 60;

/// Update `own` with what the daemon reports (`None` = not running) and say whether that daemon is this app's own.
fn track_owner(own: &mut Ownership, st: Option<&Value>, now: u64) -> bool {
    if own.spawned_at.is_some_and(|t| now > t + SPAWN_CLAIM_SECS) {
        own.spawned_at = None;
    }
    let Some(st) = st else {
        own.pid = None;
        return false;
    };
    let (pid, started) = (st["pid"].as_u64(), st["started"].as_u64().unwrap_or(0));
    if own.pid.is_some() && own.pid != pid {
        own.pid = None; // the daemon we started is gone and another one runs now
    }
    if own.pid.is_none() {
        if let Some(t) = own.spawned_at {
            if started + 2 >= t {
                (own.pid, own.spawned_at) = (pid, None);
            }
        }
    }
    pid.is_some() && own.pid == pid
}

static OWNER: std::sync::Mutex<Ownership> = std::sync::Mutex::new(Ownership { spawned_at: None, pid: None });

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Start a daemon, unless one already answers: a second one would fight the first over its port and its devices.
async fn ensure_daemon(privileged: bool) -> Result<(), Value> {
    if Client::connect_default().await.is_ok() {
        return Ok(());
    }
    spawn_daemon(privileged)?;
    OWNER.lock().unwrap().spawned_at = Some(unix_now());
    Ok(())
}

/// Ask the daemon to stop (it ejects what was imported and hands shared devices back), wait for it to be gone, then leave.
async fn stop_daemon_and_exit(app: AppHandle) {
    if let Ok(mut c) = Client::connect_default().await {
        let _ = c.call("shutdown", json!({})).await;
        for _ in 0..40 {
            if Client::connect_default().await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    app.exit(0);
}

#[tauri::command]
async fn start_daemon(privileged: bool) -> Result<(), Value> {
    ensure_daemon(privileged).await
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
fn tray_view(st: Option<&Value>, owned: bool) -> (Vec<Item>, Look, String) {
    let mut items = Vec::new();
    let (look, tip) = match st {
        None => {
            items.push(Item::Header("periwire: daemon not running".into()));
            items.push(Item::Action("start".into(), "Start daemon".into()));
            if cfg!(target_os = "linux") {
                items.push(Item::Action("start-root".into(), "Start daemon (can import devices)…".into()));
            }
            (Look::Warn, "periwire: daemon not running".to_string())
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
            let look = if problem {
                Look::Warn
            } else if atts.len() + sess.len() > 0 {
                Look::Active
            } else {
                Look::Idle
            };
            (look, format!("periwire: {} imported, {} shared device(s) in use", atts.len(), sess.len()))
        }
    };
    items.push(Item::Separator);
    items.push(Item::Action("open".into(), "Open periwire".into()));
    match st {
        None => items.push(Item::Action("quit".into(), "Quit".into())),
        Some(st) => {
            // Whoever started the daemon decides what Quit means: the app's own daemon goes with it, any other one stays up.
            let busy = st["attachments"].as_array().map_or(0, |a| a.len()) + st["sessions"].as_array().map_or(0, |a| a.len());
            let cost = if busy > 0 { format!(" (disconnects {busy} in use)") } else { String::new() };
            let stop = Item::Action("quit-stop".into(), format!("Quit and stop the daemon{cost}"));
            if owned {
                items.push(stop);
                items.push(Item::Action("quit".into(), "Quit and leave the daemon running".into()));
            } else {
                items.push(Item::Action("quit".into(), "Quit the app (the daemon keeps running)".into()));
                items.push(stop);
            }
        }
    }
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
    let owned = track_owner(&mut OWNER.lock().unwrap(), st, unix_now());
    let (items, look, tip) = tray_view(st, owned);
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
        .invoke_handler(tauri::generate_handler![api, start_daemon])
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
                                let n = n.parse::<u64>().unwrap_or(0);
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
                } else if OWNER.lock().unwrap().pid.is_some() {
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

    fn status(atts: usize, sess: usize) -> Value {
        json!({
            "attachments": (0..atts).map(|i| json!({"id": i, "busid": "1-2", "vid": "1234", "pid": "5678", "state": "attached"})).collect::<Vec<_>>(),
            "sessions": (0..sess).map(|i| json!({"id": i, "busid": "2-1", "peer": "10.0.0.2", "peer_name": ""})).collect::<Vec<_>>(),
            "shares": [], "export": {"state": "listening"}
        })
    }

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

    #[test]
    fn the_same_state_twice_changes_nothing() {
        let mut shown = Shown::default();
        let (items, look, tip) = tray_view(Some(&status(1, 0)), false);
        assert_eq!(changes(&mut shown, &items, look, &tip), (true, true, true), "the first state is applied in full");
        for _ in 0..5 {
            assert_eq!(changes(&mut shown, &items, look, &tip), (false, false, false), "re-reading an unchanged daemon must not touch the tray (that was the flicker)");
        }
    }

    #[test]
    fn only_what_changed_is_touched() {
        let mut shown = Shown::default();
        let (i0, l0, t0) = tray_view(Some(&status(0, 0)), false);
        changes(&mut shown, &i0, l0, &t0);
        // an attachment appears: menu, icon (idle -> active) and tooltip all change
        let (i1, l1, t1) = tray_view(Some(&status(1, 0)), false);
        assert_eq!(changes(&mut shown, &i1, l1, &t1), (true, true, true));
        // a second one: the menu and tooltip change, the icon stays active
        let (i2, l2, t2) = tray_view(Some(&status(2, 0)), false);
        assert_eq!(changes(&mut shown, &i2, l2, &t2), (true, false, true));
        // the daemon goes away: warning icon and the start menu
        let (i3, l3, t3) = tray_view(None, false);
        assert_eq!(l3, Look::Warn);
        assert_eq!(changes(&mut shown, &i3, l3, &t3), (true, true, true));
    }

    #[test]
    fn the_menu_lists_what_can_be_ejected_or_disconnected() {
        let (items, look, _) = tray_view(Some(&status(1, 1)), false);
        assert_eq!(look, Look::Active);
        let ids: Vec<&str> = items.iter().filter_map(|i| if let Item::Action(id, _) = i { Some(id.as_str()) } else { None }).collect();
        assert_eq!(ids, ["eject:0", "kick:0", "open", "quit", "quit-stop"]);
        assert!(matches!(items[0], Item::Header(_)));
    }

    #[test]
    fn an_export_error_shows_the_warning_icon() {
        let mut st = status(0, 0);
        st["export"]["state"] = json!("error");
        assert_eq!(tray_view(Some(&st), false).1, Look::Warn);
        assert_eq!(tray_view(Some(&status(0, 0)), false).1, Look::Idle);
    }

    fn label(items: &[Item], wanted: &str) -> String {
        items.iter().find_map(|i| if let Item::Action(id, l) = i { (id == wanted).then(|| l.clone()) } else { None }).unwrap_or_default()
    }

    #[test]
    fn quit_means_what_the_owner_of_the_daemon_would_expect() {
        let st = status(1, 1);
        let (other, _, _) = tray_view(Some(&st), false);
        assert_eq!(label(&other, "quit"), "Quit the app (the daemon keeps running)", "somebody else's daemon stays up");
        assert_eq!(label(&other, "quit-stop"), "Quit and stop the daemon (disconnects 2 in use)", "and says what stopping it costs");
        let (mine, _, _) = tray_view(Some(&st), true);
        let first_quit = mine.iter().find_map(|i| if let Item::Action(id, _) = i { id.starts_with("quit").then(|| id.clone()) } else { None });
        assert_eq!(first_quit.as_deref(), Some("quit-stop"), "the app's own daemon is what a plain quit takes down");
        assert_eq!(label(&mine, "quit"), "Quit and leave the daemon running");
        let (none, _, _) = tray_view(None, false);
        assert_eq!(label(&none, "quit"), "Quit");
        assert_eq!(label(&none, "quit-stop"), "", "nothing to stop");
        assert_eq!(label(&tray_view(Some(&status(0, 0)), false).0, "quit-stop"), "Quit and stop the daemon");
    }

    #[test]
    fn the_app_owns_the_daemon_it_started_and_no_other() {
        let up = |pid: u64, started: u64| json!({"pid": pid, "started": started});
        let mut own = Ownership::default();
        // a daemon that was running before the app asked for one is not the app's
        assert!(!track_owner(&mut own, Some(&up(10, 1000)), 1005));
        own.spawned_at = Some(2000);
        assert!(!track_owner(&mut own, Some(&up(10, 1000)), 2001), "still the old daemon");
        assert!(track_owner(&mut own, Some(&up(11, 2001)), 2002), "the one that started after the request is the app's");
        assert!(track_owner(&mut own, Some(&up(11, 2001)), 2100), "and stays so");
        assert!(!track_owner(&mut own, None, 2200), "gone");
        assert!(!track_owner(&mut own, Some(&up(12, 2250)), 2251), "a later daemon from a terminal is not the app's");
    }

    #[test]
    fn an_unanswered_request_cannot_claim_a_daemon_started_later() {
        let mut own = Ownership { spawned_at: Some(3000), pid: None };
        // the password prompt was cancelled; a terminal daemon starts minutes later
        assert!(!track_owner(&mut own, Some(&json!({"pid": 20, "started": 3200})), 3200));
        assert_eq!(own.spawned_at, None);
    }
}
