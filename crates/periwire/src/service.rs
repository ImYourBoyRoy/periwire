//! `periwire service ...`: run the daemon as a system service (systemd on Linux, launchd on macOS, a scheduled task on Windows), look at it, remove it.
//!
//! The service runs with administrator rights (so it can import devices) and belongs to one user: `--socket-user` gives that user the control socket,
//! so the desktop app and the command line of that person (and nobody else) can talk to it without elevating.
use crate::daemon;
#[cfg(unix)]
use crate::{cli, tray};
use anyhow::Result;
use serde_json::json;

fn sh(cmd: &str, args: &[&str]) -> Result<()> {
    let st = std::process::Command::new(cmd).args(args).status()?;
    if !st.success() {
        anyhow::bail!("`{cmd} {}` failed", args.join(" "));
    }
    Ok(())
}

fn ok(cmd: &str, args: &[&str]) -> bool {
    std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Starts the taskbar icon for whoever logs in (Linux, when the daemon is a system service).
const TRAY_AUTOSTART: &str = "/etc/xdg/autostart/periwire-tray.desktop";
const LAUNCHD_LABEL: &str = "com.glassprovidence.periwire.daemon";
const LAUNCHD_PLIST: &str = "/Library/LaunchDaemons/com.glassprovidence.periwire.daemon.plist";
const UNIT: &str = "/etc/systemd/system/periwire.service";

/// Who may control the service: `--for-user`, else whoever ran sudo or pkexec.
#[cfg(unix)]
fn controlling_user(for_user: Option<String>) -> Result<String> {
    if let Some(u) = for_user {
        return Ok(u);
    }
    if let Ok(u) = std::env::var("SUDO_USER") {
        if !u.is_empty() && u != "root" {
            return Ok(u);
        }
    }
    if let Some(uid) = std::env::var("PKEXEC_UID").ok().and_then(|v| v.parse::<u32>().ok()) {
        let pw = unsafe { libc::getpwuid(uid) };
        if !pw.is_null() {
            return Ok(unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned());
        }
    }
    anyhow::bail!("say which user may control the service: run it with sudo or pkexec, or add --for-user NAME")
}

pub(crate) fn service_install(for_user: Option<String>) -> Result<i32> {
    if cfg!(windows) {
        // A console program is not a native Windows service; run it elevated at logon through Task Scheduler.
        let exe = std::env::current_exe()?;
        sh("schtasks", &["/Create", "/F", "/SC", "ONLOGON", "/RL", "HIGHEST", "/TN", "PeriWire", "/TR", &format!("\"{}\" daemon", exe.display())])?;
        let _ = sh("schtasks", &["/Run", "/TN", "PeriWire"]);
        println!("installed the PeriWire task (runs at logon with highest privileges) and started it");
        return Ok(0);
    }
    #[cfg(unix)]
    {
        if !daemon::is_root() {
            anyhow::bail!("this needs administrator rights: run it with sudo (or use the app's Settings > Daemon > Install service)");
        }
        let user = controlling_user(for_user)?;
        if cfg!(target_os = "macos") {
            std::fs::write(LAUNCHD_PLIST, cli::service_text(false, Some(&user)))?;
            let _ = sh("launchctl", &["bootout", &format!("system/{LAUNCHD_LABEL}")]);
            sh("launchctl", &["bootstrap", "system", LAUNCHD_PLIST])?;
            println!("installed and started {LAUNCHD_LABEL}; {user} can control it");
            return Ok(0);
        }
        if !cfg!(target_os = "linux") {
            anyhow::bail!("automatic install is not available on this system; use `periwire service print`");
        }
        std::fs::write(UNIT, cli::service_text(false, Some(&user)))?;
        sh("systemctl", &["daemon-reload"])?;
        sh("systemctl", &["enable", "periwire.service"])?;
        sh("systemctl", &["restart", "periwire.service"])?; // also when an older unit is already running: the new one takes effect
        println!("installed and started periwire.service; {user} can control it");
        // A service has no desktop to show an icon on: have the desktop app put it up when somebody logs in.
        match tray::gui_path() {
            Some(gui) => {
                std::fs::create_dir_all("/etc/xdg/autostart")?;
                std::fs::write(TRAY_AUTOSTART, tray::autostart_entry(&gui))?;
                println!("added the taskbar icon at login ({TRAY_AUTOSTART})");
            }
            None => println!("the desktop app (periwire-gui) is not installed next to this program, so there is no taskbar icon; install it to get one"),
        }
        Ok(0)
    }
    #[cfg(not(unix))]
    {
        let _ = for_user;
        anyhow::bail!("automatic install is not available on this system")
    }
}

pub(crate) fn service_uninstall() -> Result<i32> {
    if cfg!(windows) {
        let _ = sh("schtasks", &["/End", "/TN", "PeriWire"]);
        sh("schtasks", &["/Delete", "/F", "/TN", "PeriWire"])?;
        println!("removed the PeriWire task");
        return Ok(0);
    }
    if !daemon::is_root() {
        anyhow::bail!("this needs administrator rights: run it with sudo (or use the app's Settings > Daemon > Remove service)");
    }
    if cfg!(target_os = "macos") {
        let _ = sh("launchctl", &["bootout", &format!("system/{LAUNCHD_LABEL}")]);
        let _ = std::fs::remove_file(LAUNCHD_PLIST);
        println!("removed {LAUNCHD_LABEL}");
        return Ok(0);
    }
    let _ = sh("systemctl", &["disable", "--now", "periwire.service"]);
    let _ = std::fs::remove_file(UNIT);
    let _ = std::fs::remove_file(TRAY_AUTOSTART);
    let _ = sh("systemctl", &["daemon-reload"]);
    println!("removed periwire.service");
    Ok(0)
}

/// `periwire service status`: installed, enabled at boot or login, running now. No rights needed.
pub(crate) fn service_status(json_out: bool) -> Result<i32> {
    let (kind, installed, enabled, active) = if cfg!(windows) {
        let q = ok("schtasks", &["/Query", "/TN", "PeriWire"]);
        let running =
            std::process::Command::new("schtasks").args(["/Query", "/TN", "PeriWire", "/FO", "CSV", "/NH"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains("Running")).unwrap_or(false);
        ("scheduled-task", q, q, running)
    } else if cfg!(target_os = "macos") {
        let installed = std::path::Path::new(LAUNCHD_PLIST).exists();
        ("launchd", installed, installed, ok("launchctl", &["print", &format!("system/{LAUNCHD_LABEL}")]))
    } else {
        let installed = std::path::Path::new(UNIT).exists();
        ("systemd", installed, installed && ok("systemctl", &["is-enabled", "--quiet", "periwire.service"]), installed && ok("systemctl", &["is-active", "--quiet", "periwire.service"]))
    };
    if json_out {
        println!("{}", json!({"kind": kind, "installed": installed, "enabled": enabled, "active": active, "supported": cfg!(any(windows, target_os = "linux", target_os = "macos"))}));
    } else if !installed {
        println!("the PeriWire service is not installed ({kind}); `periwire service install` (as administrator) installs it");
    } else {
        println!(
            "the PeriWire service is installed ({kind}): {}, {}",
            if enabled { "starts automatically" } else { "does not start automatically" },
            if active { "running now" } else { "not running now" }
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use crate::cli::service_text;

    #[test]
    fn the_service_names_the_one_user_who_may_control_it() {
        let t = service_text(false, Some("sam"));
        if cfg!(target_os = "macos") {
            assert!(t.contains("<string>--socket-user</string><string>sam</string>"));
        } else {
            assert!(t.contains("daemon --socket-user sam"), "{t}");
            assert!(!t.contains("socket-group"), "no shared group any more");
        }
        let none = service_text(false, None);
        assert!(!none.contains("socket-user"));
    }
}
