//! Talking to the daemon on the window's behalf: the `api` command, starting a daemon, and knowing which daemon is this app's own.
use periwire_client::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tauri::AppHandle;

pub(crate) fn err(code: &str, message: String) -> Value {
    json!({"code": code, "message": message})
}

/// The daemon methods the window may call. Everything else (stopping the daemon, uninstalling the driver, raw API access) stays with the CLI,
/// so a page that somehow ran foreign script could not do more than its own buttons can.
const PAGE_METHODS: &[&str] = &[
    "status",
    "devices",
    "doctor",
    "logs",
    "remote.list",
    "share.add",
    "share.remove",
    "attach",
    "detach",
    "kick",
    "trust.add",
    "trust.probe",
    "trust.remove",
    "connection.remove",
    "device.label",
    "export.set",
    "identity.reset",
    "driver.status",
    "driver.install",
    "subscribe",
];

/// Call a daemon method. A fresh connection per call keeps this trivially robust across daemon restarts.
#[tauri::command]
pub async fn api(method: String, params: Option<Value>) -> Result<Value, Value> {
    if !PAGE_METHODS.contains(&method.as_str()) {
        return Err(err("denied", format!("the window may not call `{method}`; use the periwire command line")));
    }
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
pub(crate) struct Ownership {
    /// Unix time at which this app asked for a daemon; the next daemon that reports a start time from then on is the one.
    spawned_at: Option<u64>,
    /// The pid of the daemon this app started.
    pid: Option<u64>,
}

impl Ownership {
    /// Is there a daemon this app started, running right now?
    pub(crate) fn has_daemon(&self) -> bool {
        self.pid.is_some()
    }
}

/// A request for a daemon (a password prompt, say) that nobody answers must not claim a daemon started later by somebody else.
const SPAWN_CLAIM_SECS: u64 = 60;

/// Update `own` with what the daemon reports (`None` = not running) and say whether that daemon is this app's own.
pub(crate) fn track_owner(own: &mut Ownership, st: Option<&Value>, now: u64) -> bool {
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

pub(crate) static OWNER: std::sync::Mutex<Ownership> = std::sync::Mutex::new(Ownership { spawned_at: None, pid: None });

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Start a daemon, unless one already answers: a second one would fight the first over its port and its devices.
pub(crate) async fn ensure_daemon(privileged: bool) -> Result<(), Value> {
    if Client::connect_default().await.is_ok() {
        return Ok(());
    }
    spawn_daemon(privileged)?;
    OWNER.lock().unwrap().spawned_at = Some(unix_now());
    Ok(())
}

/// Ask the daemon to stop (it ejects what was imported and hands shared devices back), wait for it to be gone, then leave.
pub(crate) async fn stop_daemon_and_exit(app: AppHandle) {
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
pub async fn start_daemon(privileged: bool) -> Result<(), Value> {
    ensure_daemon(privileged).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
