//! The tray icon: what its menu, icon and tooltip show for a daemon state, and applying that only when it changed.
use crate::daemon::{track_owner, unix_now, OWNER};
use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::AppHandle;

pub(crate) const TRAY: &[u8] = include_bytes!("../icons/tray.png");
const TRAY_ACTIVE: &[u8] = include_bytes!("../icons/tray-active.png");
const TRAY_WARN: &[u8] = include_bytes!("../icons/tray-warn.png");

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// One line of the tray menu, as plain data so that "did the menu change?" is a simple comparison.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Item {
    /// A greyed-out heading.
    Header(String),
    /// A clickable entry: its id (what the click handler matches on) and its label.
    Action(String, String),
    Separator,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Look {
    Idle,
    Active,
    Warn,
}

/// What the tray shows for a daemon state (`None` = the daemon is not running): the menu lines, which icon, and the tooltip.
pub(crate) fn tray_view(st: Option<&Value>, owned: bool) -> (Vec<Item>, Look, String) {
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

pub(crate) fn build_menu(app: &AppHandle, items: &[Item]) -> tauri::Result<Menu<tauri::Wry>> {
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

pub(crate) fn refresh_tray(app: &AppHandle, st: Option<&Value>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
