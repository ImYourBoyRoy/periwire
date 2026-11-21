//! Health checks: what works, what doesn't, and how to fix it.
use crate::daemon::{is_root, Daemon};
use crate::export;
use serde_json::{json, Value};

fn check(id: &str, title: &str, status: &str, detail: impl Into<String>, fix: Option<&str>) -> Value {
    json!({"id": id, "title": title, "status": status, "detail": detail.into(), "fix": fix})
}

pub fn run(d: &Daemon) -> Vec<Value> {
    let mut c = vec![];
    c.push(check("identity", "Identity", "ok", format!("fingerprint {}", d.fingerprint()), None));

    // libusb / device access
    match export::local_devices() {
        Ok(devs) => {
            c.push(check("libusb", "USB access (built-in libusb)", "ok", format!("{} shareable device(s) visible", devs.len()), None));
            if !is_root() && cfg!(target_os = "linux") {
                c.push(check(
                    "usb-perms",
                    "Permission to claim devices",
                    "warn",
                    "not running as root; devices can only be shared if udev grants this user access to them",
                    Some("run the daemon as root (systemd service) or add a udev rule for the devices"),
                ));
            } else {
                c.push(check("usb-perms", "Permission to claim devices", "ok", if is_root() { "running as root" } else { "ok" }, None));
            }
        }
        Err(e) => c.push(check("libusb", "USB access (built-in libusb)", "fail", format!("{e}"), Some("check that USB is available to this user"))),
    }

    // the command line, reachable from a shell
    c.push(cli_on_path_check());
    // leftovers of an app that was moved or deleted (Linux)
    c.extend(stale_install_checks());
    // Apple devices on this computer need usbmuxd (Linux)
    if let Some(chk) = usbmuxd_check() {
        c.push(chk);
    }

    // importer
    match crate::attach::can_import() {
        Ok(()) => c.push(check("import", "Importing devices", "ok", import_detail(), None)),
        Err(e) => {
            let fix = if cfg!(target_os = "linux") {
                "sudo modprobe vhci-hcd, and run the daemon as root"
            } else if cfg!(target_os = "macos") {
                "needs the com.apple.developer.usb.host-controller-interface entitlement (see docs/MACOS.md)"
            } else if cfg!(windows) {
                "run `periwire driver install` as Administrator (the signed driver is carried inside this program)"
            } else {
                "importing is not available on this OS yet; sharing still works"
            };
            c.push(check("import", "Importing devices", "warn", e, Some(fix)));
        }
    }

    // daemon / exporter
    c.push(check(
        "daemon",
        "Daemon",
        if d.running { "ok" } else { "warn" },
        if d.running { format!("running since {}s, pid {}", crate::events::now() - d.started, std::process::id()) } else { "not running".into() },
        if d.running { None } else { Some("periwire daemon") },
    ));
    if d.running {
        let ex = d.export.lock().unwrap().clone();
        let listen = d.cfg.lock().unwrap().export.listen.clone();
        match ex.state {
            "listening" => c.push(check("export", "Exporter", "ok", format!("listening on {listen}"), None)),
            "error" => c.push(check("export", "Exporter", "fail", ex.error.unwrap_or_default(), Some("pick another port with `periwire daemon --listen ADDR`"))),
            _ => c.push(check("export", "Exporter", "info", "off (this machine is not sharing devices)", Some("periwire export on"))),
        }
    }

    // trust
    let trusted = crate::identity::load_trusted().map(|t| t.len()).unwrap_or(0);
    if trusted == 0 {
        c.push(check(
            "trust",
            "Trusted peers",
            "warn",
            "no peer is trusted yet, so nobody can connect to this machine",
            Some("periwire trust add <fingerprint>  (the peer shows it with `periwire id`)"),
        ));
    } else {
        c.push(check("trust", "Trusted peers", "ok", format!("{trusted} trusted"), None));
    }

    // shares
    let specs = d.shared.specs();
    if let Ok(devs) = export::local_devices() {
        for s in &specs {
            let found = devs.iter().find(|(u, _)| u.busid == *s || s.eq_ignore_ascii_case(&format!("{:04x}:{:04x}", u.id_vendor, u.id_product)));
            match found {
                None => c.push(check(&format!("share:{s}"), &format!("Shared device {s}"), "warn", "not plugged in right now", None)),
                Some((u, _)) => {
                    let m = export::mounts_for(&u.busid);
                    if m.is_empty() {
                        c.push(check(&format!("share:{s}"), &format!("Shared device {s}"), "ok", format!("{:04x}:{:04x} ready", u.id_vendor, u.id_product), None));
                    } else {
                        c.push(check(
                            &format!("share:{s}"),
                            &format!("Shared device {s}"),
                            "warn",
                            format!("mounted locally ({}); peers can't take it until it's unmounted", m.iter().map(|x| x.1.clone()).collect::<Vec<_>>().join(", ")),
                            Some("unmount it (or eject it) first"),
                        ));
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    {
        let st = crate::windrv::status();
        if st.ready {
            c.push(check(
                "usbip-win2",
                "Windows virtual USB host controller",
                "ok",
                format!("installed (usbip-win2 {}, host controller {})", crate::windrv::VERSION, st.device.clone().unwrap_or_default()),
                None,
            ));
        } else {
            c.push(check(
                "usbip-win2",
                "Windows virtual USB host controller",
                "warn",
                "not installed: needed to use remote devices on Windows",
                Some("run `periwire driver install` as Administrator (the signed driver is carried inside this program)"),
            ));
        }
        if !st.vc_runtime {
            c.push(check("vcruntime", "Visual C++ runtime", "warn", "missing: the driver's command-line tools need it", Some("winget install Microsoft.VCRedist.2015+.x64")));
        }
        c.push(check(
            "winusb",
            "Sharing from Windows",
            "info",
            "devices must use the WinUSB driver to be shared (bind it with Zadig); Windows' own drivers (storage, HID) keep them away from libusb",
            None,
        ));
    }

    // competing software
    for (name, what) in [("vhusbd", "VirtualHere server"), ("vhclientx86_64", "VirtualHere client"), ("usbipd", "usbipd")] {
        if process_running(name) {
            c.push(check(
                &format!("conflict:{name}"),
                &format!("{what} is running"),
                "warn",
                format!("`{name}` may hold the same devices"),
                Some(&format!("stop `{name}` before sharing the devices it manages")),
            ));
        }
    }
    c
}

/// The `TryExec=` target of a launcher entry written by the app itself (it carries the marker), if the file is one of ours.
#[cfg(any(target_os = "linux", test))]
fn own_launcher_target(entry_text: &str) -> Option<String> {
    entry_text.contains("X-PeriWire-Self-Registered=true").then(|| entry_text.lines().find_map(|l| l.strip_prefix("TryExec=")).map(|t| t.trim().to_string())).flatten()
}

/// A launcher entry or `~/.local/bin/periwire` link whose program is gone: the app was moved or deleted after it registered itself.
#[cfg(target_os = "linux")]
fn stale_install_checks() -> Vec<Value> {
    let mut out = vec![];
    let home = dirs::home_dir();
    let data = dirs::data_dir();
    if let Some(entry) = data.map(|d| d.join("applications/periwire.desktop")) {
        if let Some(target) = std::fs::read_to_string(&entry).ok().and_then(|t| own_launcher_target(&t)) {
            if !std::path::Path::new(&target).exists() {
                out.push(check(
                    "launcher",
                    "App launcher entry",
                    "warn",
                    format!("the menu entry points at {target}, which is gone (the app was moved or deleted)"),
                    Some("start the app from its new place (it fixes the entry itself), or delete ~/.local/share/applications/periwire.desktop"),
                ));
            }
        }
    }
    if let Some(link) = home.map(|h| h.join(".local/bin/periwire")) {
        if std::fs::symlink_metadata(&link).map(|m| m.file_type().is_symlink()).unwrap_or(false) && !link.exists() {
            out.push(check(
                "cli-link",
                "The periwire link",
                "warn",
                format!("{} points at a program that is gone", link.display()),
                Some("run `periwire self-install` from the program's new place, or `periwire self-uninstall`, or delete the link"),
            ));
        }
    }
    out
}
#[cfg(not(target_os = "linux"))]
fn stale_install_checks() -> Vec<Value> {
    vec![]
}

/// The first folder of `path_env` that holds an executable `name`.
fn find_on_path(name: &str, path_env: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    std::env::split_paths(path_env).map(|d| d.join(name)).find(|p| p.is_file())
}

fn cli_on_path_check() -> Value {
    let name = if cfg!(windows) { "periwire.exe" } else { "periwire" };
    let here = std::env::current_exe().ok();
    match std::env::var_os("PATH").and_then(|p| find_on_path(name, &p)) {
        Some(p) => check("cli-path", "The periwire command", "ok", format!("found on your PATH: {}", p.display()), None),
        None => {
            let fix = if cfg!(windows) {
                "run `periwire self-install` (adds the folder that holds periwire.exe to your user PATH), then open a new PowerShell or cmd window"
            } else {
                "run `periwire self-install` (links ~/.local/bin/periwire and sets up your shells), or install the .deb; then open a new terminal"
            };
            let detail = match here {
                Some(h) => format!("not found on your PATH (this copy runs from {})", h.display()),
                None => "not found on your PATH".into(),
            };
            check("cli-path", "The periwire command", "warn", detail, Some(fix))
        }
    }
}

/// What is wrong with usbmuxd, given `systemctl is-enabled` / `is-active` output and whether an Apple device is attached here and not currently
/// handed to another computer. A masked or stopped usbmuxd leaves an iPhone/iPad stuck in its basic USB mode: it never shows up in the file manager or
/// tethers, and unplugging and replugging does nothing.
#[cfg(any(target_os = "linux", test))]
fn usbmuxd_problem(enabled: &str, active: &str, apple_here: bool) -> Option<(&'static str, String, &'static str)> {
    if !apple_here {
        return None;
    }
    if enabled.starts_with("masked") {
        return Some((
            "fail",
            "usbmuxd is masked (blocked from starting), so an iPhone or iPad attached to this computer cannot finish connecting".into(),
            "sudo systemctl unmask usbmuxd (or `unmask --runtime`), then sudo systemctl start usbmuxd",
        ));
    }
    if active != "active" {
        return Some(("warn", format!("usbmuxd is {active} while an Apple device is attached"), "sudo systemctl start usbmuxd, or unplug and replug the device"));
    }
    None
}

#[cfg(target_os = "linux")]
fn usbmuxd_check() -> Option<Value> {
    let apple_here = export::local_devices().map(|d| d.iter().any(|(u, _)| u.id_vendor == 0x05ac)).unwrap_or(false);
    let sctl = |a: &str| std::process::Command::new("systemctl").args([a, "usbmuxd"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let (enabled, active) = if apple_here { (sctl("is-enabled"), sctl("is-active")) } else { (String::new(), String::new()) };
    match usbmuxd_problem(&enabled, &active, apple_here) {
        Some((status, detail, fix)) => Some(check("usbmuxd", "iPhone/iPad support (usbmuxd)", status, detail, Some(fix))),
        None if apple_here => Some(check("usbmuxd", "iPhone/iPad support (usbmuxd)", "ok", "running", None)),
        None => None,
    }
}
#[cfg(not(target_os = "linux"))]
fn usbmuxd_check() -> Option<Value> {
    None
}

fn import_detail() -> String {
    #[cfg(target_os = "linux")]
    {
        return format!("vhci_hcd {}", if crate::import::vhci::loaded() { "loaded" } else { "available (loads on first attach)" });
    }
    #[allow(unreachable_code)]
    "ok".into()
}

fn process_running(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                if let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) {
                    if comm.trim() == name || (name.len() > 15 && comm.trim() == &name[..15]) {
                        return true;
                    }
                }
            }
        }
    }
    let _ = name;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_masked_or_stopped_usbmuxd_is_reported_only_when_an_apple_device_is_here() {
        assert!(usbmuxd_problem("masked", "inactive", false).is_none(), "nothing to say without an iPhone or iPad");
        assert_eq!(usbmuxd_problem("masked-runtime", "inactive", true).unwrap().0, "fail");
        assert_eq!(usbmuxd_problem("static", "inactive", true).unwrap().0, "warn");
        assert!(usbmuxd_problem("static", "active", true).is_none());
    }

    #[test]
    fn only_our_own_launcher_entries_are_judged() {
        let ours = "[Desktop Entry]\nName=PeriWire\nTryExec=/home/x/PeriWire.AppImage\nX-PeriWire-Self-Registered=true\n";
        assert_eq!(own_launcher_target(ours).as_deref(), Some("/home/x/PeriWire.AppImage"));
        assert_eq!(own_launcher_target("[Desktop Entry]\nName=Mine\nTryExec=/x\n"), None, "somebody else's entry is not ours to judge");
    }

    #[test]
    fn the_command_is_found_in_the_first_path_folder_that_has_it() {
        let d = std::env::temp_dir().join(format!("pw-doctor-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("periwire"), b"x").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(find_on_path("periwire", &path), Some(b.join("periwire")));
        assert_eq!(find_on_path("nothing", &path), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
