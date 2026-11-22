//! Things on this computer that stand in the way of exporting a device, stepped aside while PeriWire holds it and put back afterwards.
//!
//! Today that is `usbmuxd` (Linux): the service that talks to iPhones and iPads keeps their USB interface open, so a share of an Apple device
//! failed with "Resource busy" until somebody stopped it by hand, and afterwards nothing started it again, so the device never came back on the
//! desktop. Now a root daemon stops it when it first claims an Apple device and starts it when the last one is released (a session ending, an
//! unshare, the daemon stopping), which makes it look at the bus again. What was stopped is also written to `restore.json`, so a daemon that was
//! killed in between puts it back at its next start.
use std::collections::HashMap;
use std::sync::Mutex;

const APPLE: u16 = 0x05ac;

/// What can be done to the host; a fake in the tests.
pub trait Host: Send {
    fn is_root(&self) -> bool;
    fn usbmuxd_active(&self) -> bool;
    fn stop_usbmuxd(&self) -> bool;
    fn start_usbmuxd(&self);
    fn save(&self, stopped: bool);
    fn saved(&self) -> bool;
}

#[derive(Default)]
pub struct Prep {
    /// Apple devices held now, by busid.
    held: HashMap<String, ()>,
    /// We stopped usbmuxd and have not started it again.
    stopped: bool,
}

impl Prep {
    /// A device is about to be claimed. Returns a line for the log when something was stepped aside.
    pub fn acquire(&mut self, host: &dyn Host, vid: u16, busid: &str) -> Option<String> {
        if vid != APPLE {
            return None;
        }
        self.held.insert(busid.to_string(), ());
        if self.stopped || !cfg!(target_os = "linux") || !host.is_root() || !host.usbmuxd_active() {
            return None;
        }
        if host.stop_usbmuxd() {
            self.stopped = true;
            host.save(true);
            return Some("stopped usbmuxd so the Apple device can be shared; it is started again when the device is released".into());
        }
        None
    }

    /// A device was released. Returns a line for the log when something was put back.
    pub fn release(&mut self, host: &dyn Host, vid: u16, busid: &str) -> Option<String> {
        if vid != APPLE {
            return None;
        }
        self.held.remove(busid);
        self.restore_if_idle(host)
    }

    /// Put back what was stepped aside, unless an Apple device is still held.
    pub fn restore_if_idle(&mut self, host: &dyn Host) -> Option<String> {
        if self.stopped && self.held.is_empty() {
            self.stopped = false;
            host.start_usbmuxd();
            host.save(false);
            return Some("started usbmuxd again; the iPhone or iPad is back on this computer".into());
        }
        None
    }

    /// At the daemon's start: a previous run that was killed may have left usbmuxd stopped.
    pub fn recover(&mut self, host: &dyn Host) -> Option<String> {
        if host.saved() && host.is_root() && self.held.is_empty() {
            host.start_usbmuxd();
            host.save(false);
            return Some("started usbmuxd, which an earlier PeriWire run had stopped".into());
        }
        None
    }
}

/// The real host.
pub struct System;

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl").args(args).stdin(std::process::Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

impl Host for System {
    fn is_root(&self) -> bool {
        crate::daemon::is_root()
    }
    fn usbmuxd_active(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            return systemctl(&["is-active", "--quiet", "usbmuxd"]);
        }
        #[allow(unreachable_code)]
        false
    }
    fn stop_usbmuxd(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            return systemctl(&["stop", "usbmuxd"]);
        }
        #[allow(unreachable_code)]
        false
    }
    fn start_usbmuxd(&self) {
        #[cfg(target_os = "linux")]
        {
            // The device has just been handed back: give the kernel a moment to bind its drivers again, then let usbmuxd look at the bus.
            std::thread::sleep(std::time::Duration::from_millis(400));
            systemctl(&["start", "usbmuxd"]);
        }
    }
    fn save(&self, stopped: bool) {
        let path = crate::identity::home().join("restore.json");
        if stopped {
            let _ = std::fs::create_dir_all(crate::identity::home());
            let _ = std::fs::write(path, br#"{"usbmuxd": "stopped by periwire"}"#);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    fn saved(&self) -> bool {
        crate::identity::home().join("restore.json").exists()
    }
}

static PREP: Mutex<Option<Prep>> = Mutex::new(None);

fn with_prep<R>(f: impl FnOnce(&mut Prep) -> R) -> R {
    let mut g = PREP.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Prep::default))
}

pub fn acquire(vid: u16, busid: &str) {
    if let Some(line) = with_prep(|p| p.acquire(&System, vid, busid)) {
        crate::logln!("{line}");
    }
}

pub fn release(vid: u16, busid: &str) {
    if let Some(line) = with_prep(|p| p.release(&System, vid, busid)) {
        crate::logln!("{line}");
    }
}

/// The daemon is stopping: put back whatever is still stepped aside.
pub fn restore_all() {
    if let Some(line) = with_prep(|p| {
        p.held.clear();
        p.restore_if_idle(&System)
    }) {
        crate::logln!("{line}");
    }
}

/// The daemon is starting.
pub fn recover() {
    if let Some(line) = with_prep(|p| p.recover(&System)) {
        crate::logln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        root: bool,
        active: RefCell<bool>,
        calls: RefCell<Vec<&'static str>>,
        saved: RefCell<bool>,
    }
    // the trait needs Send; a test fake used on one thread is fine
    unsafe impl Send for Fake {}
    impl Fake {
        fn new(root: bool, active: bool) -> Self {
            Fake { root, active: RefCell::new(active), calls: RefCell::new(vec![]), saved: RefCell::new(false) }
        }
    }
    impl Host for Fake {
        fn is_root(&self) -> bool {
            self.root
        }
        fn usbmuxd_active(&self) -> bool {
            *self.active.borrow()
        }
        fn stop_usbmuxd(&self) -> bool {
            self.calls.borrow_mut().push("stop");
            *self.active.borrow_mut() = false;
            true
        }
        fn start_usbmuxd(&self) {
            self.calls.borrow_mut().push("start");
            *self.active.borrow_mut() = true;
        }
        fn save(&self, stopped: bool) {
            *self.saved.borrow_mut() = stopped;
        }
        fn saved(&self) -> bool {
            *self.saved.borrow()
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn usbmuxd_is_stopped_for_the_first_apple_device_and_started_after_the_last() {
        let h = Fake::new(true, true);
        let mut p = Prep::default();
        assert!(p.acquire(&h, 0x1234, "1-1").is_none(), "other devices are none of its business");
        assert!(p.acquire(&h, APPLE, "1-2").is_some());
        assert!(p.acquire(&h, APPLE, "1-3").is_none(), "already stepped aside");
        assert!(*h.saved.borrow(), "written down, in case the daemon is killed");
        assert!(p.release(&h, APPLE, "1-2").is_none(), "another Apple device is still held");
        assert_eq!(*h.calls.borrow(), ["stop"]);
        assert!(p.release(&h, APPLE, "1-3").is_some());
        assert_eq!(*h.calls.borrow(), ["stop", "start"]);
        assert!(!*h.saved.borrow());
    }

    #[test]
    fn nothing_is_touched_without_root_or_when_usbmuxd_is_not_running() {
        let h = Fake::new(false, true);
        let mut p = Prep::default();
        assert!(p.acquire(&h, APPLE, "1-2").is_none());
        assert!(h.calls.borrow().is_empty(), "a daemon without root cannot and does not stop a system service");
        let h = Fake::new(true, false);
        assert!(Prep::default().acquire(&h, APPLE, "1-2").is_none());
        assert!(h.calls.borrow().is_empty());
    }

    #[test]
    fn a_killed_daemon_is_cleaned_up_after_at_the_next_start_and_the_stop_restores_too() {
        let h = Fake::new(true, false);
        *h.saved.borrow_mut() = true; // the marker a killed run left behind
        let mut p = Prep::default();
        assert!(p.recover(&h).is_some());
        assert_eq!(*h.calls.borrow(), ["start"]);
        assert!(!*h.saved.borrow());
        assert!(p.recover(&h).is_none(), "once");
        // the daemon stopping with a device still held
        #[cfg(target_os = "linux")]
        {
            let h = Fake::new(true, true);
            let mut p = Prep::default();
            p.acquire(&h, APPLE, "1-2");
            p.held.clear();
            assert!(p.restore_if_idle(&h).is_some());
            assert_eq!(*h.calls.borrow(), ["stop", "start"]);
        }
    }
}
