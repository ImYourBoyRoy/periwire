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
    "update.check",
    "update.set",
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

pub(crate) fn find_periwire() -> Option<std::path::PathBuf> {
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
/// The folder the daemon's log, this app's own settings and the daemon's identity live in.
pub(crate) fn config_dir() -> std::path::PathBuf {
    std::env::var_os("PERIWIRE_HOME").map(std::path::PathBuf::from).or_else(|| dirs::config_dir().map(|d| d.join("periwire"))).unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The person using this app: the one who may control a daemon that runs with administrator rights.
pub(crate) fn current_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

/// Run `periwire <args>` with administrator rights and, when `wait`, until it ends. Linux asks polkit (`pkexec`), macOS the system password dialog
/// (`osascript`), Windows the UAC prompt. The person is asked by the operating system; this app never sees a password.
pub(crate) fn run_elevated(bin: &std::path::Path, args: &[String], wait: bool) -> Result<String, Value> {
    let quiet = || (std::process::Stdio::null(), std::process::Stdio::null());
    if cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("pkexec");
        c.arg(bin).args(args).stdin(std::process::Stdio::null());
        if wait {
            let o = c.output().map_err(|e| err("failed", format!("could not ask for administrator rights (is pkexec installed?): {e}")))?;
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string();
            return if o.status.success() {
                Ok(text)
            } else {
                Err(err("failed", if o.status.code() == Some(126) || o.status.code() == Some(127) { "administrator rights were not granted".into() } else { text }))
            };
        }
        let (o, e) = quiet();
        return c.stdout(o).stderr(e).spawn().map(|_| String::new()).map_err(|e| err("failed", format!("could not ask for administrator rights (is pkexec installed?): {e}")));
    }
    if cfg!(target_os = "macos") {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let cmd = std::iter::once(quote(&bin.display().to_string())).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
        let shell = if wait { cmd } else { format!("{cmd} >/dev/null 2>&1 &") };
        let script = format!("do shell script \"{}\" with administrator privileges", shell.replace('\\', "\\\\").replace('"', "\\\""));
        let o = std::process::Command::new("osascript").arg("-e").arg(script).output().map_err(|e| err("failed", e.to_string()))?;
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string();
        return if o.status.success() { Ok(text) } else { Err(err("failed", if text.contains("-128") { "administrator rights were not granted".into() } else { text })) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let arglist = args.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" ");
        let ps = format!(
            "$p = Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -WindowStyle Hidden {} ; if ($p) {{ exit $p.ExitCode }}",
            bin.display().to_string().replace('\'', "''"),
            arglist.replace('\'', "''"),
            if wait { "-Wait -PassThru" } else { "-PassThru" }
        );
        let o = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps]).creation_flags(0x0800_0000).output().map_err(|e| err("failed", e.to_string()))?;
        return if o.status.success() { Ok(String::new()) } else { Err(err("failed", "administrator rights were not granted, or the program failed".into())) };
    }
    #[allow(unreachable_code)]
    Err(err("failed", "asking for administrator rights is not supported here".into()))
}

/// Start `periwire daemon` with this app's own rights, or (`privileged`) with administrator rights for the person using this app.
fn spawn_daemon(privileged: bool) -> Result<(), Value> {
    let bin = find_periwire().ok_or_else(|| err("failed", "could not find the `periwire` program next to this app or in PATH".into()))?;
    if privileged {
        let mut args = vec!["daemon".to_string()];
        let user = current_user();
        if !user.is_empty() && cfg!(unix) {
            args.extend(["--socket-user".into(), user]); // the daemon belongs to whoever asked for it, however it was elevated
        }
        return run_elevated(&bin, &args, false).map(|_| ());
    }
    // Its output goes to a file: a daemon nobody can read the log of is one nobody can fix.
    let log = std::fs::create_dir_all(config_dir()).ok().and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(config_dir().join("daemon.log")).ok());
    let (out, errf) = match log.and_then(|l| l.try_clone().ok().map(|c| (l, c))) {
        Some((a, b)) => (std::process::Stdio::from(a), std::process::Stdio::from(b)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    std::process::Command::new(&bin)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(errf)
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

/// This app's own version (the daemon reports its own through `status`; the two differ until the daemon is restarted after an update).
#[tauri::command]
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Run the command line program with `args` and return what it printed (an error carries what it printed when it failed).
pub(crate) async fn run_cli(args: Vec<&'static str>) -> Result<String, Value> {
    let bin = find_periwire().ok_or_else(|| err("failed", "could not find the `periwire` program next to this app or in PATH".into()))?;
    let out = tokio::task::spawn_blocking(move || std::process::Command::new(bin).args(args).stdin(std::process::Stdio::null()).output())
        .await
        .map_err(|e| err("failed", e.to_string()))?
        .map_err(|e| err("failed", e.to_string()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(err("failed", text))
    }
}

/// The window's "terminal commands" button: the command line's `self-install` (or `self-uninstall`).
#[tauri::command]
pub async fn terminal_setup(undo: bool) -> Result<String, Value> {
    run_cli(if undo { vec!["self-uninstall", "--yes"] } else { vec!["self-install", "--yes"] }).await
}

/// How this copy can be updated (`periwire self-update --status`): `{kind, supported, reason?, replaces}`.
#[tauri::command]
pub async fn update_support() -> Result<String, Value> {
    run_cli(vec!["--json", "self-update", "--status"]).await
}

/// Where `periwire` is on this computer's PATH, if anywhere (the window shows whether the terminal command is set up).
#[tauri::command]
pub fn cli_on_path() -> Option<String> {
    let name = if cfg!(windows) { "periwire.exe" } else { "periwire" };
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.is_file())).map(|f| f.display().to_string())
}

/// Start the app again from `gui` once this process has gone (a second copy would only hand over to this one), optionally starting the daemon it owned.
fn relaunch(gui: &std::path::Path, start_daemon: bool) {
    let extra = if start_daemon { "--start-daemon" } else { "" };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg("sleep 2; exec \"$0\" $1").arg(gui).arg(extra).process_group(0);
        let _ = c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let line = format!("ping -n 3 127.0.0.1 >NUL & start \"\" \"{}\" {extra}", gui.display());
        let _ = std::process::Command::new("cmd").arg("/C").raw_arg(format!("\"{line}\"")).creation_flags(0x0800_0000).spawn();
    }
}

/// The window's "Install update": the command line's `self-update` (which checks the download's SHA-256 before it replaces anything), then the app
/// starts again from the new files, and so does the daemon if this app had started it. A daemon started elsewhere keeps running until its owner restarts it.
#[tauri::command]
pub async fn self_update(app: AppHandle) -> Result<String, Value> {
    self_update_with(app, vec![]).await
}

/// `self_update` with extra arguments for the command line (the debug-build self-test uses `--to` and `--force`).
pub(crate) async fn self_update_with(app: AppHandle, extra: Vec<&'static str>) -> Result<String, Value> {
    let gui = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from).or_else(|| std::env::current_exe().ok());
    let owned = OWNER.lock().unwrap().has_daemon();
    let mut args = vec!["self-update", "--yes"];
    args.extend(extra);
    let text = run_cli(args).await?;
    if owned {
        if let Ok(mut c) = Client::connect_default().await {
            let _ = c.call("shutdown", json!({})).await;
            for _ in 0..30 {
                if Client::connect_default().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    if let Some(gui) = gui {
        relaunch(&gui, owned);
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await; // the answer reaches the window first
        app.exit(0);
    });
    Ok(text)
}

/// Is this a link the window may ask the system browser to open? Only PeriWire's own releases, with plain URL characters.
pub(crate) fn is_release_link(url: &str) -> bool {
    url.starts_with("https://github.com/ImYourBoyRoy/periwire/") && url.len() < 300 && url.bytes().all(|b| b.is_ascii_alphanumeric() || b"/.-_~:%".contains(&b))
}

/// Open a PeriWire release page in the default browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), Value> {
    if !is_release_link(&url) {
        return Err(err("denied", "the window may only open PeriWire's release pages".into()));
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        std::process::Command::new("explorer.exe")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(&url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ()).map_err(|e| err("failed", format!("{e}")))
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

    #[test]
    fn only_periwire_release_pages_may_be_opened() {
        assert!(is_release_link("https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.2.0"));
        for bad in [
            "https://evil.example/ImYourBoyRoy/periwire/",
            "http://github.com/ImYourBoyRoy/periwire/",
            "https://github.com/ImYourBoyRoy/periwire/ x",
            "https://github.com/ImYourBoyRoy/periwire/&calc",
            "https://github.com/ImYourBoyRoy/periwire/\"",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!is_release_link(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_app_starts_again_after_it_has_gone_and_passes_the_daemon_flag_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pw-relaunch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (script, out) = (dir.join("fake-gui"), dir.join("args.txt"));
        std::fs::write(&script, format!("#!/bin/sh\necho \"started with: $*\" > '{}'\n", out.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        relaunch(&script, true);
        for _ in 0..60 {
            if out.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "started with: --start-daemon");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A button that calls a method the app shell refuses is a bug the user finds first (0.1.2 shipped one: "the window may not call `update.check`").
    #[test]
    fn every_daemon_method_the_window_calls_is_one_it_may_call() {
        for (file, src) in [("app.js", include_str!("../../ui/app.js")), ("settings.js", include_str!("../../ui/settings.js"))] {
            let mut rest = src;
            let mut seen = 0;
            while let Some(i) = rest.find("call(\"") {
                let r = &rest[i + 6..];
                let name = &r[..r.find('"').unwrap()];
                assert!(PAGE_METHODS.contains(&name), "{file} calls `{name}`, which the app shell does not allow: add it to PAGE_METHODS");
                seen += 1;
                rest = r;
            }
            assert!(seen > 0, "{file}: the scan found no calls, so the check proves nothing");
        }
    }
}
