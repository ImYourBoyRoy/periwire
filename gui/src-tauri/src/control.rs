//! Controlling the daemon from the window: whether it starts with the app, the rights it runs with (off, as you, as administrator),
//! and the system service. Elevation is the operating system's own prompt (see `daemon::run_elevated`).
use crate::daemon::{config_dir, current_user, ensure_daemon, err, find_periwire, run_cli, run_elevated, OWNER};
use periwire_client::Client;
use serde_json::{json, Value};
use std::time::Duration;

fn prefs_path() -> std::path::PathBuf {
    config_dir().join("gui.json")
}

/// Does the app start the daemon when it opens? Yes unless the person switched it off.
pub(crate) fn autostart() -> bool {
    std::fs::read_to_string(prefs_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["autostart_daemon"].as_bool()).unwrap_or(true)
}

fn set_autostart(on: bool) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(prefs_path(), json!({"autostart_daemon": on}).to_string());
}

/// How this computer can ask for administrator rights, if it can.
fn elevation() -> &'static str {
    if cfg!(target_os = "linux") {
        if std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("pkexec").is_file())) {
            "pkexec"
        } else {
            "none"
        }
    } else if cfg!(target_os = "macos") {
        "osascript"
    } else {
        "uac"
    }
}

/// What the window needs to know beyond the daemon's own status.
#[tauri::command]
pub fn daemon_info() -> Value {
    json!({"owned": OWNER.lock().unwrap().has_daemon(), "autostart": autostart(), "elevation": elevation(), "user": current_user(), "log": config_dir().join("daemon.log").display().to_string()})
}

/// Ask the running daemon to stop and wait until it has. Ok(false) when none was running.
async fn stop_daemon() -> Result<bool, Value> {
    let Ok(mut c) = Client::connect_default().await else { return Ok(false) };
    let _ = c.call("shutdown", json!({})).await;
    for _ in 0..40 {
        if Client::connect_default().await.is_err() {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(err("failed", "the daemon did not stop within 8 s; stop it with `periwire stop`".into()))
}

async fn wait_for_daemon(secs: u64) -> bool {
    for _ in 0..secs * 5 {
        if Client::connect_default().await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// `mode`: "off" (stop it, and do not start it with the app), "user" (with your own rights) or "admin" (with administrator rights).
#[tauri::command]
pub async fn daemon_set_mode(mode: String) -> Result<String, Value> {
    match mode.as_str() {
        "off" => {
            let was = stop_daemon().await?;
            set_autostart(false);
            Ok(if was { "stopped".into() } else { "already stopped".into() })
        }
        "user" => {
            stop_daemon().await?;
            set_autostart(true);
            ensure_daemon(false).await?;
            if wait_for_daemon(10).await {
                Ok("running with your rights".into())
            } else {
                Err(err("failed", format!("the daemon did not start; its log is {}", config_dir().join("daemon.log").display())))
            }
        }
        "admin" => {
            stop_daemon().await?;
            ensure_daemon(true).await?;
            // the person has to answer the system's password prompt
            if wait_for_daemon(90).await {
                Ok("running with administrator rights".into())
            } else {
                Err(err("failed", "the daemon did not start with administrator rights (the prompt was cancelled, or it failed)".into()))
            }
        }
        other => Err(err("invalid", format!("unknown mode `{other}`"))),
    }
}

/// The system service: `{kind, installed, enabled, active, supported}`.
#[tauri::command]
pub async fn service_status() -> Result<String, Value> {
    run_cli(vec!["--json", "service", "status"]).await
}

async fn service_command(sub: &'static str) -> Result<String, Value> {
    let bin = find_periwire().ok_or_else(|| err("failed", "could not find the `periwire` program next to this app or in PATH".into()))?;
    // the service and this app's own daemon would both want the port: this app's goes first
    stop_daemon().await?;
    let mut args = vec!["service".to_string(), sub.to_string()];
    if sub == "install" && !current_user().is_empty() && cfg!(unix) {
        args.extend(["--for-user".into(), current_user()]);
    }
    let r = tokio::task::spawn_blocking(move || run_elevated(&bin, &args, true)).await.map_err(|e| err("failed", e.to_string()))?;
    if sub == "install" {
        wait_for_daemon(15).await;
    }
    r
}

#[tauri::command]
pub async fn service_install() -> Result<String, Value> {
    let r = service_command("install").await;
    if r.is_ok() {
        set_autostart(true);
    }
    r
}

#[tauri::command]
pub async fn service_uninstall() -> Result<String, Value> {
    service_command("uninstall").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_starts_with_the_app_unless_switched_off() {
        let d = std::env::temp_dir().join(format!("pw-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("PERIWIRE_HOME", &d);
        assert!(autostart(), "on by default");
        set_autostart(false);
        assert!(!autostart());
        set_autostart(true);
        assert!(autostart());
        let _ = std::fs::remove_dir_all(&d);
    }
}
