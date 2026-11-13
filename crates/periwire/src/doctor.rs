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
