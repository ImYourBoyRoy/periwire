//! The taskbar / menu-bar icon, whenever the daemon runs.
//!
//! The icon belongs to the desktop app (`periwire-gui`), not to the daemon: a daemon started from a terminal, by the MCP
//! setup, by a service or by the app itself should all show it. So the daemon starts the app in its tray-only mode
//! (`--hidden`: no window, the icon only; `--follow-daemon`: leave again once this daemon is gone and nobody opened the
//! window). The app allows one instance, so if it is already running the second start ends at once and nothing doubles.
//!
//! Opt out with `periwire daemon --no-tray` or `PERIWIRE_NO_TRAY=1`.
use std::path::PathBuf;

/// What to do about the icon on this machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Do not start it, and why (shown in the log).
    Skip(&'static str),
    /// Start it, as this user (`None`) or, when the daemon runs as root on behalf of someone, as that user.
    Start { as_user: Option<u32> },
}

/// The decision, from facts only. `invoker` is the user who ran `sudo` or `pkexec` (their uid), `user_session` whether that user
/// has a graphical session we can reach (Linux, root case), `display` whether this process itself has a display (Linux, user case).
pub fn plan(opt_out: bool, linux: bool, euid: u32, invoker: Option<u32>, user_session: bool, display: bool) -> Plan {
    if opt_out {
        return Plan::Skip("switched off (--no-tray / PERIWIRE_NO_TRAY)");
    }
    if !linux {
        return Plan::Start { as_user: None }; // macOS and Windows: the daemon runs in the user's own session
    }
    if euid == 0 {
        return match invoker {
            Some(uid) if uid != 0 && user_session => Plan::Start { as_user: Some(uid) },
            Some(uid) if uid != 0 => Plan::Skip("the user who started this has no graphical session"),
            _ => Plan::Skip("running as root with no user to show it to (log in and start `periwire-gui`, or install the autostart entry: `sudo periwire service install`)"),
        };
    }
    if display {
        Plan::Start { as_user: None }
    } else {
        Plan::Skip("no graphical session here")
    }
}

/// The desktop app: next to this program, else on the PATH.
pub fn gui_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "periwire-gui.exe" } else { "periwire-gui" };
    if let Some(p) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name))).filter(|p| p.exists()) {
        return Some(p);
    }
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.exists()))
}

/// The XDG autostart entry that starts the icon at login (for a daemon that runs as a system service).
pub fn autostart_entry(gui: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=PeriWire (taskbar icon)\nComment=Shows the PeriWire daemon in the taskbar\nExec=\"{}\" --hidden --follow-daemon\nIcon=periwire\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        gui.display()
    )
}

fn env_flag(k: &str) -> bool {
    std::env::var_os(k).map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Starts the icon if this machine can show one. Never fails the daemon: problems only go to the log.
pub fn ensure(opt_out: bool) {
    let opt_out = opt_out || env_flag("PERIWIRE_NO_TRAY");
    #[cfg(unix)]
    let (euid, invoker) = (unsafe { libc::geteuid() }, ["SUDO_UID", "PKEXEC_UID"].iter().find_map(|k| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok())));
    #[cfg(not(unix))]
    let (euid, invoker): (u32, Option<u32>) = (1, None);
    let user_session = invoker.map(|u| std::path::Path::new(&format!("/run/user/{u}/bus")).exists()).unwrap_or(false);
    let display = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    match plan(opt_out, cfg!(target_os = "linux"), euid, invoker, user_session, display) {
        Plan::Skip(why) => {
            if !opt_out {
                crate::logln!("taskbar icon not started: {why}");
            }
        }
        Plan::Start { as_user } => {
            let Some(gui) = gui_path() else {
                crate::logln!("taskbar icon not started: the desktop app (periwire-gui) is not installed next to this program");
                return;
            };
            match launch(&gui, as_user) {
                Ok(()) => crate::logln!("taskbar icon: started {} (it stays if the app was already running)", gui.display()),
                Err(e) => crate::logln!("taskbar icon not started: {e}"),
            }
        }
    }
}

fn launch(gui: &std::path::Path, as_user: Option<u32>) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(gui);
    cmd.args(["--hidden", "--follow-daemon"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // not part of the daemon's terminal job: Ctrl-C there must not take the icon with it
        if let Some(uid) = as_user {
            let (gid, home) = user_ids(uid).ok_or_else(|| std::io::Error::other(format!("user {uid} not found")))?;
            let run = format!("/run/user/{uid}");
            let wayland =
                std::fs::read_dir(&run).ok().and_then(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock")).min());
            cmd.env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()))
                .env("HOME", home)
                .env("XDG_RUNTIME_DIR", &run)
                .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={run}/bus"))
                .env("DISPLAY", std::env::var_os("DISPLAY").unwrap_or_else(|| ":0".into()));
            if let Some(w) = wayland {
                cmd.env("WAYLAND_DISPLAY", w);
            }
            if let Some(x) = std::env::var_os("XAUTHORITY") {
                cmd.env("XAUTHORITY", x);
            }
            // Dropping from root also drops root's supplementary groups (the standard library does that when no groups are given).
            cmd.gid(gid).uid(uid);
        }
    }
    #[cfg(not(unix))]
    let _ = as_user;
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait(); // reap it: a second instance exits straight away
    });
    Ok(())
}

#[cfg(unix)]
fn user_ids(uid: u32) -> Option<(u32, std::ffi::OsString)> {
    use std::os::unix::ffi::OsStringExt;
    // SAFETY: getpwuid returns a pointer to static storage that is only read here, before any other call.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_bytes().to_vec();
        Some(((*pw).pw_gid, std::ffi::OsString::from_vec(dir)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_autostart_entry_starts_the_icon_only() {
        let e = autostart_entry(std::path::Path::new("/usr/bin/periwire-gui"));
        assert!(e.contains("Exec=\"/usr/bin/periwire-gui\" --hidden --follow-daemon"));
        assert!(e.starts_with("[Desktop Entry]\n") && e.contains("NoDisplay=true"), "no launcher entry of its own: the app has one");
    }

    #[test]
    fn the_opt_out_wins_everywhere() {
        assert!(matches!(plan(true, true, 1000, None, false, true), Plan::Skip(_)));
        assert!(matches!(plan(true, false, 1, None, false, false), Plan::Skip(_)));
    }

    #[test]
    fn macos_and_windows_always_start_it() {
        assert_eq!(plan(false, false, 1, None, false, false), Plan::Start { as_user: None });
    }

    #[test]
    fn a_linux_user_needs_a_display() {
        assert_eq!(plan(false, true, 1000, None, false, true), Plan::Start { as_user: None });
        assert!(matches!(plan(false, true, 1000, None, false, false), Plan::Skip(_)), "an ssh session or a server has nowhere to show it");
    }

    #[test]
    fn root_starts_it_for_the_person_who_ran_sudo_or_pkexec() {
        assert_eq!(plan(false, true, 0, Some(1000), true, false), Plan::Start { as_user: Some(1000) });
        assert!(matches!(plan(false, true, 0, Some(1000), false, true), Plan::Skip(_)), "that person is not logged in graphically");
        assert!(matches!(plan(false, true, 0, None, false, true), Plan::Skip(_)), "a system service has nobody to show it to");
        assert!(matches!(plan(false, true, 0, Some(0), true, true), Plan::Skip(_)), "never start the app as root");
    }
}
