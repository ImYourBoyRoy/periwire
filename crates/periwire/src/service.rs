//! `periwire service ...`: install or remove the daemon as a system service.
use crate::{cli, daemon, tray};
use anyhow::Result;

fn sh(cmd: &str, args: &[&str]) -> Result<()> {
    let st = std::process::Command::new(cmd).args(args).status()?;
    if !st.success() {
        anyhow::bail!("`{cmd} {}` failed", args.join(" "));
    }
    Ok(())
}

/// Starts the taskbar icon for whoever logs in (Linux, when the daemon is a system service).
const TRAY_AUTOSTART: &str = "/etc/xdg/autostart/periwire-tray.desktop";

pub(crate) fn service_install() -> Result<i32> {
    if cfg!(windows) {
        // A console program is not a native Windows service; run it elevated at logon through Task Scheduler.
        let exe = std::env::current_exe()?;
        sh("schtasks", &["/Create", "/F", "/SC", "ONLOGON", "/RL", "HIGHEST", "/TN", "PeriWire", "/TR", &format!("\"{}\" daemon", exe.display())])?;
        let _ = sh("schtasks", &["/Run", "/TN", "PeriWire"]);
        println!("installed the PeriWire task (runs at logon with highest privileges) and started it");
        return Ok(0);
    }
    if !cfg!(target_os = "linux") {
        anyhow::bail!("automatic install is Linux-only for now; use `periwire service print` and install it yourself");
    }
    if !daemon::is_root() {
        anyhow::bail!("run with sudo");
    }
    std::fs::write("/etc/systemd/system/periwire.service", cli::service_text(false))?;
    let _ = sh("groupadd", &["-f", "periwire"]);
    sh("systemctl", &["daemon-reload"])?;
    sh("systemctl", &["enable", "--now", "periwire.service"])?;
    println!("installed and started periwire.service. Let your user control it: sudo usermod -aG periwire $USER (then log in again)");
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

pub(crate) fn service_uninstall() -> Result<i32> {
    if cfg!(windows) {
        let _ = sh("schtasks", &["/End", "/TN", "PeriWire"]);
        sh("schtasks", &["/Delete", "/F", "/TN", "PeriWire"])?;
        println!("removed the PeriWire task");
        return Ok(0);
    }
    if !daemon::is_root() {
        anyhow::bail!("run with sudo");
    }
    let _ = sh("systemctl", &["disable", "--now", "periwire.service"]);
    let _ = std::fs::remove_file("/etc/systemd/system/periwire.service");
    let _ = std::fs::remove_file(TRAY_AUTOSTART);
    let _ = sh("systemctl", &["daemon-reload"]);
    println!("removed periwire.service");
    Ok(0)
}
