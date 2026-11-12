//! Exporter: shares local USB devices over USB/IP (inside mutual TLS).
//! Devices are driven from userspace through libusb, so the same code runs on Linux, macOS and Windows.
use crate::guard::{Guard, Patch, Verdict};
use crate::{identity, tls};
use anyhow::{anyhow, bail, Result};
use rusb::{Context, Device, DeviceHandle, TransferType, UsbContext};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::watch;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use serde_json::{json, Value};
use usbip_proto::*;

static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn trace_on() -> bool {
    *TRACE.get_or_init(|| std::env::var_os("PERIWIRE_TRACE").map(|v| !v.is_empty() && v != "0").unwrap_or(false))
}
macro_rules! trace {
    ($($a:tt)*) => { if trace_on() { eprintln!("[trace {:?}] {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() % 100_000_000, format!($($a)*)); } };
}

/// State shared between the exporter and the daemon/control API.
pub struct Shared {
    /// Busids or `vid:pid` that may be imported by trusted peers.
    pub specs: RwLock<Vec<String>>,
    /// Shares the owner write-protects for every importer (subset of `specs`).
    pub ro_specs: RwLock<Vec<String>>,
    pub sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// User-chosen names, keyed by `vid:pid[:serial]` (see `device_key`).
    pub labels: RwLock<std::collections::BTreeMap<String, String>>,
    cache: Mutex<HashMap<String, Strings>>,
    next: AtomicU64,
    /// Devices whose importer lost its connection, kept claimed for a grace period so it can resume (see `resume`).
    parked: Mutex<crate::resume::Parking<Dev>>,
}

/// Manufacturer, product and serial strings read from a device.
#[derive(Clone, Default)]
pub struct Strings {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
}

/// One peer currently using one of our devices.
pub struct Session {
    pub id: u64,
    pub peer: String,
    pub peer_name: String,
    pub busid: String,
    pub vid: u16,
    pub pid: u16,
    /// Human-readable device info (display name, kind, label) captured when the session opened.
    pub info: Value,
    pub since: u64,
    pub urbs: AtomicU64,
    /// The importer asked for write protection (enforced by `guard`).
    pub read_only: bool,
    /// Certificate fingerprint of the importer, and whether it asked to be waited for after a lost connection.
    fp: String,
    resume: bool,
    /// The session flags that must match for a parked device to be resumed.
    options: u32,
    /// Set when the same importer reconnected on a new connection: this one is dead, and the device should be parked, not released.
    takeover: AtomicBool,
    /// Set when the shared device itself vanished (unplugged or re-enumerated): the session is over and the importer should attach it again.
    gone: AtomicBool,
    /// Bytes received from the importer (OUT data) and sent to it (IN data).
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    kick: watch::Sender<bool>,
}

impl Session {
    pub fn view(&self) -> Value {
        json!({"id": self.id, "peer": self.peer, "peer_name": self.peer_name, "busid": self.busid,
               "vid": format!("{:04x}", self.vid), "pid": format!("{:04x}", self.pid), "info": self.info, "since": self.since, "read_only": self.read_only,
               "urbs": self.urbs.load(Ordering::Relaxed), "bytes_in": self.rx.load(Ordering::Relaxed), "bytes_out": self.tx.load(Ordering::Relaxed)})
    }
}

impl Shared {
    pub fn new(specs: Vec<String>, ro_specs: Vec<String>, labels: std::collections::BTreeMap<String, String>) -> Arc<Self> {
        Arc::new(Shared { specs: RwLock::new(specs), ro_specs: RwLock::new(ro_specs), sessions: Mutex::new(HashMap::new()), labels: RwLock::new(labels), cache: Mutex::new(HashMap::new()), next: AtomicU64::new(1), parked: Mutex::new(Default::default()) })
    }
    pub fn specs(&self) -> Vec<String> {
        self.specs.read().unwrap().clone()
    }
    /// Force-disconnect a session: the importer loses the device and it returns to this machine.
    pub fn kick(&self, id: u64) -> bool {
        match self.sessions.lock().unwrap().get(&id) {
            Some(s) => {
                let _ = s.kick.send(true);
                true
            }
            None => false,
        }
    }
    pub fn kick_busid(&self, busid: &str) -> usize {
        let g = self.sessions.lock().unwrap();
        let mut n = 0;
        for s in g.values().filter(|s| s.busid == busid) {
            let _ = s.kick.send(true);
            n += 1;
        }
        drop(g);
        // A device waiting for its importer to come back is released too: the operator asked for it back.
        if let Some(d) = self.parked.lock().unwrap().remove(busid) {
            drop(d);
            n += 1;
        }
        n
    }
    /// Is this device being held for an importer that lost its connection?
    pub fn is_parked(&self, busid: &str) -> bool {
        self.parked.lock().unwrap().contains(busid)
    }
    pub fn session_for(&self, busid: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().values().find(|s| s.busid == busid).cloned()
    }
}

use crate::accel::{Seq, Step, MAX_URB_BYTES};
// Linux errno values carried in RET_SUBMIT status (negated).
const EPIPE: i32 = -32;
const ENODEV: i32 = -19;
const ETIMEDOUT: i32 = -110;
const EIO: i32 = -5;
const EPROTO: i32 = -71;
const ECONNRESET: i32 = -104;

pub fn busid(dev: &Device<Context>) -> String {
    let ports = dev.port_numbers().unwrap_or_default();
    if ports.is_empty() {
        format!("{}-0", dev.bus_number())
    } else {
        let p: Vec<String> = ports.iter().map(|p| p.to_string()).collect();
        format!("{}-{}", dev.bus_number(), p.join("."))
    }
}

pub fn matches_spec(dev: &Device<Context>, spec: &str) -> bool {
    if spec == busid(dev) {
        return true;
    }
    match dev.device_descriptor() {
        Ok(d) => spec.eq_ignore_ascii_case(&format!("{:04x}:{:04x}", d.vendor_id(), d.product_id())),
        Err(_) => false,
    }
}

fn speed_code(d: &Device<Context>) -> u32 {
    match d.speed() {
        rusb::Speed::Low => 1,
        rusb::Speed::Full => 2,
        rusb::Speed::High => 3,
        rusb::Speed::Super => 5,
        rusb::Speed::SuperPlus => 6,
        _ => 0,
    }
}

pub fn describe(dev: &Device<Context>) -> Result<(UsbDevice, Vec<UsbInterface>)> {
    let d = dev.device_descriptor()?;
    let cfg = dev.active_config_descriptor().or_else(|_| dev.config_descriptor(0)).ok();
    let (cv, ifs): (u8, Vec<UsbInterface>) = match &cfg {
        Some(c) => (
            c.number(),
            c.interfaces()
                .filter_map(|i| i.descriptors().next())
                .map(|a| UsbInterface { class: a.class_code(), subclass: a.sub_class_code(), protocol: a.protocol_code() })
                .collect(),
        ),
        None => (0, vec![]),
    };
    let id = busid(dev);
    let u = UsbDevice {
        path: format!("periwire/{id}"),
        busid: id,
        busnum: dev.bus_number() as u32,
        devnum: dev.address() as u32,
        speed: speed_code(dev),
        id_vendor: d.vendor_id(),
        id_product: d.product_id(),
        bcd_device: {
            let v = d.device_version();
            ((v.major() as u16) << 8) | ((v.minor() as u16) << 4) | v.sub_minor() as u16
        },
        device_class: d.class_code(),
        device_subclass: d.sub_class_code(),
        device_protocol: d.protocol_code(),
        configuration_value: cv,
        num_configurations: d.num_configurations(),
        num_interfaces: ifs.len() as u8,
    };
    Ok((u, ifs))
}

/// All local devices (hubs excluded) with their busids — used by `periwire local`.
pub fn local_devices() -> Result<Vec<(UsbDevice, Vec<UsbInterface>)>> {
    let ctx = Context::new()?;
    let mut out = vec![];
    for dev in ctx.devices()?.iter() {
        if dev.device_descriptor().map(|d| d.class_code() == 9).unwrap_or(true) {
            continue;
        }
        if let Ok(x) = describe(&dev) {
            out.push(x);
        }
    }
    Ok(out)
}

fn shared_devices(specs: &[String]) -> Result<Vec<(UsbDevice, Vec<UsbInterface>)>> {
    let ctx = Context::new()?;
    let mut out = vec![];
    for dev in ctx.devices()?.iter() {
        if specs.iter().any(|s| matches_spec(&dev, s)) {
            if let Ok(x) = describe(&dev) {
                out.push(x);
            }
        }
    }
    Ok(out)
}

fn clean(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()).filter(|s| !s.is_empty())
}

/// Strings from the device itself (short-lived handle; falls back to sysfs on Linux when we may not open it).
fn read_strings(dev: &Device<Context>) -> Strings {
    let Ok(d) = dev.device_descriptor() else { return Strings::default() };
    if let Ok(h) = dev.open() {
        return Strings { manufacturer: clean(h.read_manufacturer_string_ascii(&d).ok()), product: clean(h.read_product_string_ascii(&d).ok()), serial: clean(h.read_serial_number_string_ascii(&d).ok()) };
    }
    #[cfg(target_os = "linux")]
    {
        let base = format!("/sys/bus/usb/devices/{}", busid(dev));
        let rd = |n: &str| std::fs::read_to_string(format!("{base}/{n}")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        return Strings { manufacturer: clean(rd("manufacturer")), product: clean(rd("product")), serial: clean(rd("serial")) };
    }
    #[allow(unreachable_code)]
    Strings::default()
}

impl Shared {
    fn strings(&self, dev: &Device<Context>, u: &UsbDevice) -> Strings {
        let key = format!("{}|{:04x}:{:04x}", u.busid, u.id_vendor, u.id_product);
        if let Some(x) = self.cache.lock().unwrap().get(&key) {
            return x.clone();
        }
        let x = read_strings(dev);
        self.cache.lock().unwrap().insert(key, x.clone());
        x
    }
    /// Set (or with an empty string clear) a device's label.
    pub fn set_label(&self, key: &str, label: &str) {
        let mut l = self.labels.write().unwrap();
        if label.trim().is_empty() {
            l.remove(key);
        } else {
            l.insert(key.to_string(), label.trim().chars().take(60).collect());
        }
    }
}

/// Stable identity of a device for labels: model plus serial number when it has one.
pub fn device_key(vid: u16, pid: u16, serial: Option<&str>) -> String {
    match serial {
        Some(s) if !s.is_empty() => format!("{vid:04x}:{pid:04x}:{s}"),
        _ => format!("{vid:04x}:{pid:04x}"),
    }
}

/// Name for humans: the label if set, else the product (with its maker), else the kind.
pub fn display_name(label: Option<&str>, st: &Strings, kind_label: &str) -> String {
    if let Some(l) = label.filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    let (m, p) = (st.manufacturer.as_deref().unwrap_or(""), st.product.as_deref().unwrap_or(""));
    match (m.is_empty(), p.is_empty()) {
        (true, true) => kind_label.to_string(),
        (false, true) => format!("{m} {kind_label}"),
        (true, false) => p.to_string(),
        (false, false) => if p.to_lowercase().contains(&m.to_lowercase()) { p.to_string() } else { format!("{m} {p}") },
    }
}

/// Everything the UIs show about one local device.
fn view_device(shared: &Shared, specs: &[String], dev: &Device<Context>, u: &UsbDevice, ifs: &[UsbInterface]) -> Value {
    let ro_specs = shared.ro_specs.read().unwrap().clone();
    let st = shared.strings(dev, u);
    let triples: Vec<(u8, u8, u8)> = ifs.iter().map(|i| (i.class, i.subclass, i.protocol)).collect();
    let kind = crate::classify::classify(u.id_vendor, u.id_product, u.device_class, &triples, st.product.as_deref().unwrap_or(""));
    let key = device_key(u.id_vendor, u.id_product, st.serial.as_deref());
    let label = shared.labels.read().unwrap().get(&key).cloned();
    let session = shared.session_for(&u.busid);
    let mounts: Vec<String> = mounts_for(&u.busid).into_iter().map(|(d, m)| format!("{d} on {m}")).collect();
    json!({
        "busid": u.busid, "vid": format!("{:04x}", u.id_vendor), "pid": format!("{:04x}", u.id_product),
        "manufacturer": st.manufacturer, "product": st.product, "serial": st.serial, "key": key,
        "label": label, "kind": kind.id, "kind_label": kind.label, "display": display_name(label.as_deref(), &st, kind.label),
        "speed": speed_name(u.speed),
        "class": class_name(if u.device_class == 0 { ifs.first().map(|i| i.class).unwrap_or(0) } else { u.device_class }),
        "interfaces": ifs.iter().map(|i| class_name(i.class)).collect::<Vec<_>>(),
        "shared": specs.iter().any(|s| matches_spec(dev, s)),
        "shared_read_only": ro_specs.iter().any(|s| matches_spec(dev, s)),
        "reconnect_hold": shared.is_parked(&u.busid),
        "in_use_by": session.map(|s| if s.peer_name.is_empty() { s.peer.clone() } else { s.peer_name.clone() }),
        "mounted": mounts,
    })
}

/// Rich view of every local (non-hub) device for the control API.
pub fn device_views(shared: &Shared) -> Result<Vec<Value>> {
    let ctx = Context::new()?;
    let specs = shared.specs();
    let mut out = vec![];
    for dev in ctx.devices()?.iter() {
        let Ok(d) = dev.device_descriptor() else { continue };
        if d.class_code() == 9 {
            continue;
        }
        let Ok((u, ifs)) = describe(&dev) else { continue };
        out.push(view_device(shared, &specs, &dev, &u, &ifs));
    }
    Ok(out)
}

/// Same, limited to devices we currently share (what an importing peer may see).
pub fn shared_views(shared: &Shared) -> Result<Vec<Value>> {
    Ok(device_views(shared)?.into_iter().filter(|v| v["shared"] == true).collect())
}

pub fn speed_name(code: u32) -> &'static str {
    match code { 1 => "low (1.5 Mb/s)", 2 => "full (12 Mb/s)", 3 => "high (480 Mb/s)", 5 => "super (5 Gb/s)", 6 => "super+ (10 Gb/s)", _ => "unknown" }
}

pub fn class_name(c: u8) -> &'static str {
    match c {
        0x01 => "audio", 0x02 => "communications", 0x03 => "hid", 0x05 => "physical", 0x06 => "imaging", 0x07 => "printer",
        0x08 => "storage", 0x09 => "hub", 0x0a => "cdc-data", 0x0b => "smart-card", 0x0d => "security", 0x0e => "video",
        0x0f => "healthcare", 0x10 => "av", 0xe0 => "wireless", 0xef => "misc", 0xfe => "application", 0xff => "vendor", _ => "other",
    }
}

/// Mount points of any filesystems on block devices that belong to this USB device (Linux only).
/// Forwarding a device out from under a mounted filesystem risks data loss, so we refuse.
#[cfg(target_os = "linux")]
pub fn mounts_for(busid: &str) -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, depth: u32, names: &mut Vec<String>) {
        if depth > 9 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() {
                continue; // symlinks (driver, subsystem, ...) are not followed
            }
            if e.file_name() == "block" {
                if let Ok(b) = std::fs::read_dir(e.path()) {
                    names.extend(b.flatten().map(|x| x.file_name().to_string_lossy().into_owned()));
                }
            } else {
                walk(&e.path(), depth + 1, names);
            }
        }
    }
    let mut names = vec![];
    if let Ok(rd) = std::fs::read_dir("/sys/bus/usb/devices") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&format!("{busid}:")) {
                walk(&e.path(), 0, &mut names);
            }
        }
    }
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (dev, mp) = (f.next()?, f.next()?);
            let base = dev.strip_prefix("/dev/")?;
            names.iter().any(|n| base.starts_with(n.as_str())).then(|| (dev.to_string(), mp.to_string()))
        })
        .collect()
}
#[cfg(not(target_os = "linux"))]
pub fn mounts_for(_busid: &str) -> Vec<(String, String)> {
    vec![]
}
fn mounted_filesystems(busid: &str) -> Vec<String> {
    mounts_for(busid).into_iter().map(|(d, m)| format!("{d} on {m}")).collect()
}

#[derive(Clone, Copy)]
struct Bot {
    iface: u8,
    ep_in: u8,
    ep_out: u8,
}

/// True when any interface of the device's first configuration is USB mass storage (class 8).
fn is_mass_storage(dev: &Device<Context>) -> bool {
    dev.config_descriptor(0).map(|c| c.interfaces().any(|i| i.descriptors().any(|a| a.class_code() == 0x08))).unwrap_or(false)
}

/// After a reset the device may still be re-enumerating; control requests sent too early time out (5 s each),
/// which stalls an importer's first enumeration. Wait (bounded) until it answers a device-descriptor read.
fn settle(h: &DeviceHandle<Context>) {
    let start = std::time::Instant::now();
    let mut buf = [0u8; 18];
    while start.elapsed() < Duration::from_secs(4) {
        if h.read_control(0x80, 6, 0x0100, 0, &mut buf, Duration::from_millis(250)).is_ok() {
            trace!("device answered after {:?}", start.elapsed());
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    trace!("device still silent after reset (continuing)");
}

/// An opened, fully claimed device. Dropping it releases the interfaces, which re-attaches the
/// original kernel driver (so the device returns to the local machine).
struct Dev {
    h: DeviceHandle<Context>,
    dev: Device<Context>,
    claimed: Vec<u8>,
    eps: HashMap<u8, TransferType>,
    /// The mass-storage Bulk-Only interface, if any (used for reset recovery).
    bot: Option<Bot>,
    /// Set by every bulk transfer; cleared by recovery. A device-descriptor read while set means the importer re-enumerated.
    dirty: std::sync::atomic::AtomicBool,
    /// Set when libusb says the device itself is gone (unplugged, or it dropped off the bus and came back under a new address,
    /// which Apple devices do after the host's mode-switch request). This handle can never work again.
    gone: std::sync::atomic::AtomicBool,
    /// The "unlock the device" hint has been logged for this session (see `config_refused`).
    hinted: std::sync::atomic::AtomicBool,
    config: u8,
}

impl Dev {
    fn open(dev: Device<Context>) -> Result<Self> {
        let (bus, ports) = (dev.bus_number(), dev.port_numbers().unwrap_or_default());
        let storage = is_mass_storage(&dev);
        let h = dev.open()?;
        let _ = h.set_auto_detach_kernel_driver(true);
        let mut me = Dev { h, dev, claimed: vec![], eps: HashMap::new(), config: 0, bot: None, dirty: std::sync::atomic::AtomicBool::new(false), gone: std::sync::atomic::AtomicBool::new(false), hinted: std::sync::atomic::AtomicBool::new(false) };
        if storage {
            // Reset first, then claim: this order is the one that works on real sticks. Claiming a stick that the kernel's
            // storage driver is still bound to, without a reset first, makes some of them stop answering within 100 ms.
            // (Only mass storage is reset: other devices, Apple's for one, re-enumerate and have no stuck-stick problem.)
            match me.h.reset() {
                Ok(()) => trace!("device reset ok"),
                Err(e @ (rusb::Error::NoDevice | rusb::Error::NotFound)) => {
                    trace!("device re-enumerated after the reset ({e}); reopening it");
                    me.reopen(bus, &ports)?;
                }
                Err(e) => trace!("device reset: {e} (continuing)"),
            }
            settle(&me.h);
        } else {
            // Other devices are left alone: many (Apple's, for one) drop off the bus and come back when reset, which
            // would invalidate this handle, and they have no stuck-stick problem to cure.
            trace!("not mass storage: no reset");
        }
        if me.dev.active_config_descriptor().is_err() {
            let first = me.dev.config_descriptor(0)?.number();
            me.h.set_active_configuration(first)?;
        }
        me.claim_all()?;
        Ok(me)
    }

    /// The device came back under a new address after a reset: find it again by its bus and port path and reopen it.
    fn reopen(&mut self, bus: u8, ports: &[u8]) -> Result<()> {
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            let ctx = Context::new()?;
            for d in ctx.devices()?.iter() {
                if d.bus_number() == bus && d.port_numbers().unwrap_or_default() == ports {
                    if let Ok(h) = d.open() {
                        let _ = h.set_auto_detach_kernel_driver(true);
                        self.h = h;
                        self.dev = d;
                        return Ok(());
                    }
                }
            }
        }
        bail!("the device did not come back after being reset")
    }

    fn claim_all(&mut self) -> Result<()> {
        let cfg = self.dev.active_config_descriptor()?;
        self.config = cfg.number();
        self.eps.clear();
        self.bot = None;
        self.claimed.clear(); // claiming an already-claimed interface is a no-op; the list is rebuilt
        for i in cfg.interfaces() {
            for alt in i.descriptors() {
                for ep in alt.endpoint_descriptors() {
                    self.eps.entry(ep.address()).or_insert(ep.transfer_type());
                }
                if self.bot.is_none() && alt.class_code() == 0x08 && alt.sub_class_code() == 0x06 && alt.protocol_code() == 0x50 {
                    let bulk = |d: rusb::Direction| alt.endpoint_descriptors().find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == d).map(|e| e.address());
                    if let (Some(ep_in), Some(ep_out)) = (bulk(rusb::Direction::In), bulk(rusb::Direction::Out)) {
                        self.bot = Some(Bot { iface: alt.interface_number(), ep_in, ep_out });
                    }
                }
            }
            self.h.claim_interface(i.number()).map_err(|e| anyhow!("claim interface {}: {e}", i.number()))?;
            self.claimed.push(i.number());
        }
        Ok(())
    }

    /// Bulk-Only Transport "reset recovery" (USB Mass Storage BOT spec 5.3.4): Mass Storage Reset plus clearing both
    /// bulk halts. A host does this itself when a stick stops answering, but an importer whose USB stack never sees the
    /// real endpoint state (usbip-win2) does not, so a stick wedged by one bad exchange would refuse commands forever.
    fn bot_recover(&self) {
        if let Some(b) = self.bot {
            let r = self.h.write_control(0x21, 0xFF, 0, b.iface as u16, &[], Duration::from_millis(1000));
            let a = self.h.clear_halt(b.ep_in);
            let c = self.h.clear_halt(b.ep_out);
            // Some sticks keep an unread status block (CSW) through the reset; the next command would then read that
            // stale block instead of its own data (an overflow). Drain whatever is still queued on the IN pipe.
            let mut drained = 0;
            let mut junk = [0u8; 1024];
            while drained < 8 && self.h.read_bulk(b.ep_in, &mut junk, Duration::from_millis(80)).is_ok() {
                drained += 1;
            }
            trace!("BOT reset recovery on interface {}: reset={:?} clear_in={:?} clear_out={:?} drained={}", b.iface, r, a, c, drained);
        }
    }

    /// The device refused a configuration change. Apple devices do that while locked (iOS blocks USB data on a locked device), and the
    /// importer then retries forever with nothing else in the log, so say what is going on, once per session.
    fn config_refused(&self, val: u8, e: &rusb::Error) {
        trace!("set configuration {val} refused: {e}");
        let apple = self.dev.device_descriptor().map(|d| d.vendor_id() == 0x05ac).unwrap_or(false);
        if apple && !self.hinted.swap(true, Ordering::SeqCst) {
            crate::logln!("the device refused configuration {val} ({e}). An iPhone or iPad does this while it is locked: unlock it, keep the screen on, and tap Trust if it asks");
        }
    }

    /// A libusb error as a USB/IP status. `NoDevice` also records that the device is gone (see `gone`).
    fn err(&self, e: rusb::Error) -> i32 {
        if device_vanished(&e) {
            self.gone.store(true, Ordering::SeqCst);
        }
        map_err(e)
    }

    fn release_all(&mut self) {
        for n in self.claimed.drain(..) {
            let _ = self.h.release_interface(n);
        }
    }
}

/// libusb reports a disconnected device as `NoDevice`. (`NotFound` is a missing endpoint or interface, not a vanished device.)
fn device_vanished(e: &rusb::Error) -> bool {
    matches!(e, rusb::Error::NoDevice)
}

fn map_err(e: rusb::Error) -> i32 {
    match e {
        rusb::Error::Pipe => EPIPE,
        rusb::Error::NoDevice | rusb::Error::NotFound => ENODEV,
        rusb::Error::Timeout => ETIMEDOUT,
        rusb::Error::Overflow => -75,
        rusb::Error::Io => EIO,
        _ => EPROTO,
    }
}

type Pending = Arc<Mutex<HashMap<u32, Arc<AtomicBool>>>>;

/// After a transfer: if the device vanished under it, end the session so the importer sees a real disconnect.
fn note_gone(dev: &RwLock<Dev>, sess: &Session) {
    if dev.read().unwrap().gone.load(Ordering::SeqCst) && !sess.gone.swap(true, Ordering::SeqCst) {
        let _ = sess.kick.send(true);
    }
}

/// Run one URB to completion (or cancellation). Returns (status, data for IN transfers).
fn run_urb(dev: &RwLock<Dev>, c: &CmdSubmit, out: Vec<u8>, cancel: &AtomicBool) -> (i32, Vec<u8>) {
    let len = c.transfer_buffer_length.max(0) as usize;
    let is_in = c.basic.direction == DIR_IN;
    if c.basic.ep == 0 {
        let s = c.setup;
        let (rt, req) = (s[0], s[1]);
        let val = u16::from_le_bytes([s[2], s[3]]);
        let idx = u16::from_le_bytes([s[4], s[5]]);
        // Requests the kernel's stub driver handles itself must be applied through libusb.
        match (rt, req) {
            (0x00, 0x09) => {
                let mut g = dev.write().unwrap();
                if g.config == val as u8 {
                    return (0, vec![]);
                }
                g.release_all();
                let r = g.h.set_active_configuration(val as u8).map_err(|e| { g.config_refused(val as u8, &e); g.err(e) });
                return match r {
                    Ok(()) => match g.claim_all() { Ok(()) => (0, vec![]), Err(_) => (EIO, vec![]) },
                    Err(e) => { let _ = g.claim_all(); (e, vec![]) }
                };
            }
            (0x01, 0x0b) => {
                let g = dev.write().unwrap();
                return match g.h.set_alternate_setting(idx as u8, val as u8) {
                    Ok(()) => (0, vec![]),
                    Err(e) => (g.err(e), vec![]),
                };
            }
            // vhci's port-reset: a SET_FEATURE(PORT_RESET) aimed at the (virtual) hub port.
            (0x23, 0x03) if val == 4 => {
                let g = dev.write().unwrap();
                return match g.h.reset() { Ok(()) => (0, vec![]), Err(e) => (g.err(e), vec![]) };
            }
            (0x02, 0x01) if val == 0 => {
                let g = dev.read().unwrap();
                return match g.h.clear_halt(idx as u8) { Ok(()) => (0, vec![]), Err(e) => (g.err(e), vec![]) };
            }
            _ => {}
        }
        let t = Duration::from_secs(5);
        let g = dev.read().unwrap();
        // A host that re-reads the device descriptor is re-enumerating the device (what a port reset leads to). Some
        // importers (usbip-win2) only reset their *virtual* device, so a stick left mid-command (an aborted URB, an
        // unread status block) would stay out of sync with the new host state. Do the device-side half ourselves.
        if rt == 0x80 && req == 0x06 && (val >> 8) == 0x01 && g.dirty.swap(false, Ordering::Relaxed) {
            trace!("importer re-enumerated after bulk traffic: resetting the device's mass-storage state");
            g.bot_recover();
        }
        return if rt & 0x80 != 0 {
            let mut buf = vec![0u8; len];
            match g.h.read_control(rt, req, val, idx, &mut buf, t) {
                Ok(n) => { buf.truncate(n); (0, buf) }
                Err(e) => (g.err(e), vec![]),
            }
        } else {
            match g.h.write_control(rt, req, val, idx, &out, t) {
                Ok(_) => (0, vec![]),
                Err(e) => (g.err(e), vec![]),
            }
        };
    }
    let addr = (c.basic.ep as u8) | if is_in { 0x80 } else { 0x00 };
    let ttype = {
        let g = dev.read().unwrap();
        g.dirty.store(true, Ordering::Relaxed);
        g.eps.get(&addr).copied().unwrap_or(TransferType::Bulk)
    };
    if is_in {
        let mut buf = vec![0u8; len];
        let step = if ttype == TransferType::Interrupt { Duration::from_millis(100) } else { Duration::from_millis(1000) };
        loop {
            if cancel.load(Ordering::Relaxed) {
                return (ECONNRESET, vec![]);
            }
            let g = dev.read().unwrap();
            let r = if ttype == TransferType::Interrupt { g.h.read_interrupt(addr, &mut buf, step) } else { g.h.read_bulk(addr, &mut buf, step) };
            drop(g);
            match r {
                Ok(n) => { buf.truncate(n); return (0, buf); }
                Err(rusb::Error::Timeout) => continue,
                Err(e) => return (dev.read().unwrap().err(e), vec![]), // `g` was dropped before the transfer result was looked at
            }
        }
    } else {
        let g = dev.read().unwrap();
        let t = Duration::from_secs(10);
        let r = if ttype == TransferType::Interrupt { g.h.write_interrupt(addr, &out, t) } else { g.h.write_bulk(addr, &out, t) };
        // For OUT transfers a non-negative status is the number of bytes written.
        match r {
            Ok(n) => (n as i32, vec![]),
            Err(e) => {
                if matches!(e, rusb::Error::Timeout) && g.bot.map(|b| b.ep_out == addr).unwrap_or(false) {
                    g.bot_recover(); // the stick refused a command block for 10 s: it is wedged, un-wedge it
                }
                (g.err(e), vec![])
            }
        }
    }
}

/// Device info for one busid (used to describe sessions).
fn info_for(shared: &Shared, busid: &str) -> Value {
    device_views(shared).ok().and_then(|v| v.into_iter().find(|d| d["busid"] == busid)).unwrap_or(Value::Null)
}

fn send(tx: &UnboundedSender<Vec<u8>>, v: Vec<u8>) {
    let _ = tx.send(v);
}

pub async fn bind(listen: &str) -> Result<TcpListener> {
    TcpListener::bind(listen).await.map_err(|e| anyhow!("cannot listen on {listen}: {e}"))
}

/// Accept importers until `stop` fires; then let sessions wind down (their devices go back to this host).
pub async fn serve(shared: Arc<Shared>, listener: TcpListener, mut stop: watch::Receiver<bool>) -> Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => r?,
            _ = stop.changed() => break,
        };
        let _ = tcp.set_nodelay(true);
        let cfg = tls::server_config(&identity::load_or_create()?, identity::load_trusted()?)?; // re-read so an identity reset applies at once
        let (sd, active, shared) = (stop.clone(), active.clone(), shared.clone());
        active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
            match acceptor.accept(tcp).await {
                Ok(s) => {
                    if let Err(e) = handle(s, shared, peer.to_string(), sd).await {
                        crate::logln!("[{peer}] connection ended: {e:#}");
                    }
                }
                Err(e) => crate::logln!("[{peer}] TLS failed: {e}"),
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    for _ in 0..100 {
        if active.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Devices still waiting for an importer go back to this machine now (the daemon is stopping).
    let held = shared.parked.lock().unwrap().drain();
    drop(held);
    Ok(())
}

pub async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("sigterm handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn handle(stream: tokio_rustls::server::TlsStream<TcpStream>, shared: Arc<Shared>, peer: String, shutdown: watch::Receiver<bool>) -> Result<()> {
    let peer_fp = stream.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| identity::fingerprint(c.as_ref())).unwrap_or_default();
    let peer_name = identity::load_trusted_named().unwrap_or_default().into_iter().find(|(f, _)| *f == peer_fp).map(|(_, n)| n).unwrap_or_default();
    let (mut r, mut w) = tokio::io::split(stream);
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if w.write_all(&m).await.is_err() || w.flush().await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });

    let mut hdr = [0u8; 8];
    let mut flags = 0u32;
    let mut accel = false;
    let mut journal_fix = false;
    let mut resume = false;
    let op = loop {
        r.read_exact(&mut hdr).await?;
        let op = OpHeader::decode(&hdr)?;
        if op.code == OP_REQ_FLAGS {
            // PeriWire extension: session options sent before the import request (see usbip-proto)
            let mut b = [0u8; 4];
            r.read_exact(&mut b).await?;
            flags = u32::from_be_bytes(b);
            accel = flags & FLAG_ACCEL != 0 && crate::accel::wanted();
            journal_fix = flags & FLAG_NTFS_JOURNAL != 0;
            resume = flags & FLAG_RESUME != 0 && !crate::resume::grace().is_zero() && !peer_fp.is_empty();
            send(&tx, OpHeader::new(OP_REP_FLAGS, (if accel { FLAG_ACCEL } else { 0 }) | (if journal_fix { FLAG_NTFS_JOURNAL } else { 0 }) | (if resume { FLAG_RESUME } else { 0 })).encode().to_vec());
            continue;
        }
        break op;
    };
    let read_only = flags & FLAG_READ_ONLY != 0; // asked for by the importer
    match op.code {
        OP_REQ_DEVLIST => {
            let s = shared.specs();
            let devs = tokio::task::spawn_blocking(move || shared_devices(&s)).await??;
            send(&tx, encode_devlist_reply(&devs));
            drop(tx);
            let _ = writer.await;
            Ok(())
        }
        OP_REQ_DEVINFO => {
            let sh = shared.clone();
            let mut views = tokio::task::spawn_blocking(move || shared_views(&sh)).await??;
            for v in views.iter_mut() {
                if let Some(o) = v.as_object_mut() {
                    o.remove("serial"); // peers don't need hardware serial numbers
                    o.remove("key");
                    o.remove("in_use_by");
                }
            }
            let body = serde_json::to_vec(&views)?;
            let mut m = OpHeader::new(OP_REP_DEVINFO, 0).encode().to_vec();
            m.extend_from_slice(&(body.len() as u32).to_be_bytes());
            m.extend_from_slice(&body);
            send(&tx, m);
            drop(tx);
            let _ = writer.await;
            Ok(())
        }
        OP_REQ_IMPORT => {
            let mut body = [0u8; 32];
            r.read_exact(&mut body).await?;
            let want = decode_import_request_busid(&body)?;
            let s = shared.specs();
            let ro_specs = shared.ro_specs.read().unwrap().clone();
            // The same importer reconnecting on a new connection while the old one has not been noticed as dead yet
            // (a silent network outage): that connection is gone, so end it and let the device be parked for this one.
            if let Some(old) = shared.session_for(&want) {
                if resume && old.resume && old.fp == peer_fp {
                    crate::logln!("[{peer}] {want}: the same importer is reconnecting; taking over from its old connection");
                    old.takeover.store(true, Ordering::SeqCst);
                    let _ = old.kick.send(true);
                    for _ in 0..100 {
                        if shared.session_for(&want).is_none() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
            if shared.session_for(&want).is_some() {
                crate::logln!("[{peer}] import of {want} refused: already in use by another peer");
                send(&tx, OpHeader::new(OP_REP_IMPORT, 1).encode().to_vec());
                drop(tx);
                let _ = writer.await;
                return Ok(());
            }
            let peer2 = peer.clone();
            let (shared2, fp2) = (shared.clone(), peer_fp.clone());
            let held_at = Instant::now();
            let opened = tokio::task::spawn_blocking(move || -> Result<(Dev, UsbDevice, bool, Vec<Patch>, u32, bool)> {
                let ctx = Context::new()?;
                for dev in ctx.devices()?.iter() {
                    if busid(&dev) == want && s.iter().any(|x| matches_spec(&dev, x)) {
                        let ro = read_only || ro_specs.iter().any(|x| matches_spec(&dev, x)); // the importer's request or the owner's rule
                        let options = (ro as u32) | ((journal_fix as u32) << 1);
                        // A device kept for this importer after it lost its connection is picked up as it is (still claimed, so
                        // nothing mounted it); one kept for somebody else is not ours to take.
                        let taken = shared2.parked.lock().unwrap().take(&want, &fp2, options, Instant::now());
                        let (d, resumed) = match taken {
                            crate::resume::Taken::Resumed(d) => {
                                d.dirty.store(false, Ordering::Relaxed);
                                d.bot_recover(); // the connection died mid-command: put the stick back in a known state
                                (d, true)
                            }
                            crate::resume::Taken::Reserved => bail!("{want} is being held for another computer that lost its connection; try again in a minute"),
                            other => {
                                drop(other); // a stale hold (different options, or expired) is released first, then opened the normal way
                                let mounted = mounted_filesystems(&want);
                                if !mounted.is_empty() {
                                    bail!("refusing to share {want}: still mounted locally ({}); unmount it first", mounted.join(", "));
                                }
                                (Dev::open(dev)?, false)
                            }
                        };
                        if ro && d.bot.is_none() {
                            bail!("read-only unsupported: {want} is not plain USB mass storage, so it cannot be write-protected; refusing rather than leaving it writable");
                        }
                        let patches = if ro && journal_fix { journal_patches(&d, &want, &peer2) } else { vec![] };
                        let (mut u, _) = describe(&d.dev)?;
                        u.configuration_value = d.config;
                        return Ok((d, u, ro, patches, options, resumed));
                    }
                }
                bail!("device {want} is not shared or not present")
            })
            .await?;
            match opened {
                Err(e) => {
                    crate::logln!("[{peer}] import refused: {e:#}");
                    let code = if format!("{e:#}").starts_with("read-only unsupported") { IMPORT_READ_ONLY_UNSUPPORTED } else { 1 };
                    send(&tx, OpHeader::new(OP_REP_IMPORT, code).encode().to_vec());
                    drop(tx);
                    let _ = writer.await;
                    Ok(())
                }
                Ok((dev, udev, read_only, patches, options, resumed)) => {
                    crate::logln!("[{peer}] {} {} {:04x}:{:04x}{}", if resumed { "resumed" } else { "imported" }, udev.busid, udev.id_vendor, udev.id_product, if read_only { " (read-only)" } else { "" });
                    if resumed {
                        crate::logln!("[{peer}] {} came back after {} s", udev.busid, held_at.elapsed().as_secs());
                    }
                    let mut rep = OpHeader::new(OP_REP_IMPORT, 0).encode().to_vec();
                    udev.encode(&mut rep);
                    send(&tx, rep);
                    let (kick_tx, kick_rx) = watch::channel(false);
                    let sess = Arc::new(Session {
                        id: shared.next.fetch_add(1, Ordering::SeqCst), peer: peer.clone(), peer_name, busid: udev.busid.clone(),
                        vid: udev.id_vendor, pid: udev.id_product, info: info_for(&shared, &udev.busid), since: crate::events::now(),
                        urbs: AtomicU64::new(0), read_only, fp: peer_fp.clone(), resume, options, takeover: AtomicBool::new(false), gone: AtomicBool::new(false), rx: AtomicU64::new(0), tx: AtomicU64::new(0), kick: kick_tx,
                    });
                    shared.sessions.lock().unwrap().insert(sess.id, sess.clone());
                    crate::events::emit("session.opened", sess.view());
                    crate::events::emit("state", json!({}));
                    let end = urb_loop(&mut r, tx, Arc::new(RwLock::new(dev)), shutdown, kick_rx, sess.clone(), accel, patches).await;
                    let res = end.result;
                    let _ = writer.await;
                    shared.sessions.lock().unwrap().remove(&sess.id);
                    let grace = crate::resume::grace();
                    match end.dev {
                        Some(dev) if crate::resume::should_park(end.cause, sess.resume, grace) => {
                            let deadline = Instant::now() + grace;
                            let (id, displaced) = shared.parked.lock().unwrap().park(&sess.busid, &sess.fp, sess.options, dev, deadline);
                            drop(displaced);
                            crate::logln!("[{peer}] connection lost: keeping {} for {} s so the same computer can reconnect", sess.busid, grace.as_secs());
                            let (sh, busid) = (shared.clone(), sess.busid.clone());
                            tokio::spawn(async move {
                                tokio::time::sleep(grace).await;
                                let expired = sh.parked.lock().unwrap().expire(&busid, id);
                                if let Some(d) = expired {
                                    let _ = tokio::task::spawn_blocking(move || drop(d)).await;
                                    crate::logln!("{busid}: nobody reconnected in time; released back to local host");
                                    crate::events::emit("state", json!({}));
                                }
                            });
                        }
                        other => {
                            drop(other); // dropping the device hands it back to this machine
                            crate::logln!("[{peer}] device {} released back to local host", sess.busid);
                        }
                    }
                    crate::events::emit("session.closed", sess.view());
                    crate::events::emit("state", json!({}));
                    res
                }
            }
        }
        c => bail!("unknown op code {c:#x}"),
    }
}


struct BotJob {
    c: CmdSubmit,
    out: Vec<u8>,
    cancel: Arc<AtomicBool>,
}

/// Read the status block that follows a finished data stage. `None` leaves it for the host to read the ordinary way
/// (nothing was consumed unless the device returned something unexpected).
fn read_csw(dev: &RwLock<Dev>, ep_in: u8, cancel: &AtomicBool) -> Option<[u8; CSW_LEN]> {
    let start = std::time::Instant::now();
    let mut buf = [0u8; CSW_LEN];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let r = dev.read().unwrap().h.read_bulk(ep_in, &mut buf, Duration::from_millis(500));
        match r {
            Ok(CSW_LEN) if buf.starts_with(b"USBS") => return Some(buf),
            Ok(n) => {
                trace!("status block read returned {n} unexpected bytes; leaving recovery to the host");
                return None;
            }
            Err(rusb::Error::Timeout) if start.elapsed() < Duration::from_secs(5) => {}
            Err(e) => {
                trace!("status block read failed ({e}); the host will read it itself");
                return None;
            }
        }
    }
}

/// Reads whole sectors from a mass-storage drive with plain Bulk-Only READ commands, before a session starts and while
/// nothing else uses the device. Used only to look at the drive's structure (see `ntfs`); it never writes. Every read is
/// bounded, and any failure resets the Bulk-Only state so the drive is left exactly as it was found.
struct DriveReader<'a> {
    dev: &'a Dev,
    tag: u32,
    started: std::time::Instant,
}
impl DriveReader<'_> {
    fn attempt(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>> {
        let b = self.dev.bot?;
        let len = sectors as usize * 512;
        self.tag = self.tag.wrapping_add(1);
        let mut cbw = [0u8; 31];
        cbw[..4].copy_from_slice(b"USBC");
        cbw[4..8].copy_from_slice(&self.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        cbw[12] = 0x80; // data in
        if lba <= u32::MAX as u64 && sectors <= 0xffff {
            cbw[14] = 10; // READ(10): understood by every stick
            cbw[15] = 0x28;
            cbw[17..21].copy_from_slice(&(lba as u32).to_be_bytes());
            cbw[22..24].copy_from_slice(&(sectors as u16).to_be_bytes());
        } else {
            cbw[14] = 16;
            cbw[15] = 0x88;
            cbw[17..25].copy_from_slice(&lba.to_be_bytes());
            cbw[25..29].copy_from_slice(&sectors.to_be_bytes());
        }
        let h = &self.dev.h;
        if h.write_bulk(b.ep_out, &cbw, Duration::from_secs(2)).ok()? != 31 {
            return None;
        }
        let mut data = vec![0u8; len];
        let mut got = 0;
        while got < len {
            let n = h.read_bulk(b.ep_in, &mut data[got..], Duration::from_secs(5)).ok()?;
            if n == 0 {
                return None;
            }
            got += n;
        }
        let mut csw = [0u8; CSW_LEN];
        if h.read_bulk(b.ep_in, &mut csw, Duration::from_secs(2)).ok()? != CSW_LEN || !csw.starts_with(b"USBS") || csw[4..8] != self.tag.to_le_bytes() || csw[12] != 0 {
            return None;
        }
        Some(data)
    }
}
impl crate::ntfs::SectorSource for DriveReader<'_> {
    fn read(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>> {
        if sectors == 0 || sectors > 64 || self.started.elapsed() > Duration::from_secs(8) {
            return None;
        }
        let r = self.attempt(lba, sectors);
        if r.is_none() {
            self.dev.bot_recover(); // leave the drive in a clean Bulk-Only state whatever went wrong
        }
        r
    }
}

/// For a read-only session that asked for it: look at the drive's NTFS volumes and decide what to show instead of an
/// emptied journal. Returns the patches for the guard. Anything unclear means no patch.
fn journal_patches(dev: &Dev, busid: &str, peer: &str) -> Vec<Patch> {
    use crate::ntfs::{clean_head, scan, State};
    let mut src = DriveReader { dev, tag: 0x5057_0000, started: std::time::Instant::now() };
    let stamp = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(1)) as u32;
    let mut patches = vec![];
    for v in scan(&mut src) {
        match v.state {
            State::Emptied => {
                crate::logln!("[{peer}] {busid}: the NTFS journal of the volume at sector {} was emptied by another operating system; showing a valid clean journal header so Windows can mount it read-only (nothing on the drive is changed)", v.start_lba);
                patches.push(Patch { lba: v.log_lba, bytes: clean_head(v.log_size, stamp).to_vec() });
            }
            State::Unfinished => crate::logln!("[{peer}] {busid}: the NTFS volume at sector {} has unfinished changes in its journal; Windows may refuse to mount it read-only until it has been ejected safely from a Windows computer", v.start_lba),
            State::Clean | State::Unknown => {}
        }
    }
    patches
}

/// Runs a storage device's bulk transfers in submission order. A Bulk-Only command is strictly sequential anyway; doing it
/// on one thread lets the last data transfer's reply carry the status block (see `accel`).
fn bot_worker(mut rx: tokio::sync::mpsc::UnboundedReceiver<BotJob>, bot: Bot, dev: Arc<RwLock<Dev>>, tx: UnboundedSender<Vec<u8>>, pending: Pending, sess: Arc<Session>, guard: Option<Arc<Mutex<Guard>>>) {
    let mut seq = Seq::default();
    // The status block last sent along with a data reply. Some hosts (Windows) queue the status read right behind the data
    // read instead of waiting for it, so it can reach us after the device has nothing left to say: answer it from here.
    let mut delivered: Option<[u8; CSW_LEN]> = None;
    while let Some(BotJob { c, out, cancel }) = rx.blocking_recv() {
        let (is_in, ep, len) = (c.basic.direction == DIR_IN, c.basic.ep as u8, c.transfer_buffer_length.max(0) as usize);
        if cancel.load(Ordering::Relaxed) {
            pending.lock().unwrap().remove(&c.basic.seqnum);
            continue;
        }
        let step = if ep == 0 {
            seq.control(&c.setup);
            Step::Other
        } else {
            seq.begin(is_in, ep, &out, bot.ep_in & 0x0f, bot.ep_out & 0x0f)
        };
        let repeat = if is_in && ep == bot.ep_in & 0x0f && len >= CSW_LEN && step == Step::Other { delivered.take() } else { delivered = None; None };
        if let Some(csw) = repeat {
            let live = pending.lock().unwrap().remove(&c.basic.seqnum).is_some();
            trace!("done   seq={} status=0 bytes={} live={} step=Status (answered from the block already sent)", c.basic.seqnum, CSW_LEN, live);
            if !cancel.load(Ordering::Relaxed) && live {
                sess.tx.fetch_add(CSW_LEN as u64, Ordering::Relaxed);
                send(&tx, make_ret(&c, 0, &csw));
            }
            continue;
        }
        let (status, mut data) = if step == Step::Fail { (EPIPE, vec![]) } else { run_urb(&dev, &c, out, &cancel) };
        note_gone(&dev, &sess);
        if let Some(g) = &guard {
            g.lock().unwrap().on_complete(ep, is_in, &mut data);
        }
        let ok = if is_in { status == 0 } else { status >= 0 };
        let moved = if !ok { 0 } else if is_in { data.len() } else { status as usize };
        let csw = if seq.finish(step, ok, moved, len) { read_csw(&dev, bot.ep_in, &cancel) } else { None };
        let live = pending.lock().unwrap().remove(&c.basic.seqnum).is_some();
        trace!("done   seq={} status={} bytes={} live={} step={:?} csw={}", c.basic.seqnum, status, data.len(), live, step, csw.is_some());
        if cancel.load(Ordering::Relaxed) || !live {
            continue;
        }
        delivered = csw;
        sess.tx.fetch_add((data.len() + if csw.is_some() { CSW_LEN } else { 0 }) as u64, Ordering::Relaxed);
        let mut m = make_ret(&c, status, &data);
        if let Some(csw) = csw {
            m[40..44].copy_from_slice(&csw_mark(bot.ep_in));
            m.extend_from_slice(&csw);
        }
        send(&tx, m);
    }
}

/// Build the RET_SUBMIT for a URB: `status` is bytes written (OUT bulk), or the IN data length is `data.len()`.
fn make_ret(c: &CmdSubmit, status: i32, data: &[u8]) -> Vec<u8> {
    let is_in = c.basic.direction == DIR_IN;
    let (st, actual) = if is_in || c.basic.ep == 0 {
        (if status < 0 { status } else { 0 }, data.len() as i32)
    } else if status >= 0 {
        (0, status)
    } else {
        (status, 0)
    };
    let ret = RetSubmit { basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, seqnum: c.basic.seqnum }, status: st, actual_length: actual, start_frame: 0, number_of_packets: 0, error_count: 0 };
    let mut m = ret.encode();
    if (is_in || c.basic.ep == 0) && st == 0 && c.basic.direction == DIR_IN {
        m.extend_from_slice(data);
    }
    m
}

/// How `urb_loop` ended: the outcome, why the connection ended (see `resume`), and the device if it is free to be reused.
struct LoopEnd {
    result: Result<()>,
    cause: crate::resume::Cause,
    dev: Option<Dev>,
}

/// One URB header from the importer. A properly closed connection (TLS close_notify) reads as end-of-stream before the
/// first byte; anything else that stops the stream is a loss.
enum Hdr {
    Got,
    Closed,
    Lost(std::io::Error),
}

async fn read_header<R: AsyncReadExt + Unpin>(r: &mut R, h: &mut [u8; URB_HEADER_LEN]) -> Hdr {
    match r.read(&mut h[..1]).await {
        Ok(0) => Hdr::Closed,
        Ok(_) => match r.read_exact(&mut h[1..]).await {
            Ok(_) => Hdr::Got,
            Err(e) => Hdr::Lost(e),
        },
        Err(e) => Hdr::Lost(e),
    }
}

async fn urb_loop<R: AsyncReadExt + Unpin>(r: &mut R, tx: UnboundedSender<Vec<u8>>, dev: Arc<RwLock<Dev>>, mut shutdown: watch::Receiver<bool>, mut kick: watch::Receiver<bool>, sess: Arc<Session>, accel: bool, patches: Vec<Patch>) -> LoopEnd {
    use crate::resume::Cause;
    let mut cause = Cause::Failed; // until the loop says otherwise: errors with `?` below are protocol errors
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    // Read-only mode: write-protect USB storage on this session (the caller already refused non-storage devices).
    let guard: Option<Arc<Mutex<Guard>>> = if sess.read_only {
        dev.read().unwrap().bot.map(|b| {
            let mut g = Guard::new(b.ep_out, b.ep_in);
            g.set_patches(patches);
            Arc::new(Mutex::new(g))
        })
    } else {
        None
    };
    // Round-trip savings for storage (FLAG_ACCEL): its bulk transfers run one at a time, in order, on a worker that
    // can attach the status block to the last data reply. Everything else stays concurrent.
    let bot_q: Option<(tokio::sync::mpsc::UnboundedSender<BotJob>, Bot)> = match (accel, dev.read().unwrap().bot) {
        (true, Some(b)) => {
            let (qtx, qrx) = tokio::sync::mpsc::unbounded_channel::<BotJob>();
            let (tx2, dev2, pend2, sess2, guard2) = (tx.clone(), dev.clone(), pending.clone(), sess.clone(), guard.clone());
            tokio::task::spawn_blocking(move || bot_worker(qrx, b, dev2, tx2, pend2, sess2, guard2));
            Some((qtx, b))
        }
        _ => None,
    };
    let result: Result<()> = async {
        loop {
            let mut h = [0u8; URB_HEADER_LEN];
            let got = tokio::select! {
                r = read_header(r, &mut h) => r,
                _ = shutdown.changed() => { cause = Cause::Clean; return Ok(()) },
                _ = kick.changed() => {
                    if sess.takeover.load(Ordering::SeqCst) {
                        cause = Cause::Lost; // the importer is back on a new connection: this one is dead
                    } else if sess.gone.load(Ordering::SeqCst) {
                        // The device dropped off the bus (unplugged, or an Apple device re-enumerating after the host's mode switch).
                        // Close properly, like a real disconnect: the importer detaches it from its host and, if it asked to be kept
                        // attached, reconnects and presents the device that came back. Nothing to park: the handle is dead.
                        crate::logln!("session {} on {}: the device disconnected itself (unplugged or re-enumerated); ending the session so it can be attached again", sess.id, sess.busid);
                        cause = Cause::Clean;
                    } else {
                        crate::logln!("session {} on {} disconnected by operator", sess.id, sess.busid);
                        cause = Cause::Clean;
                    }
                    return Ok(())
                },
            };
            match got {
                Hdr::Got => {}
                Hdr::Closed => { cause = Cause::Clean; return Ok(()) }
                Hdr::Lost(e) => {
                    cause = Cause::Lost;
                    return if e.kind() == std::io::ErrorKind::UnexpectedEof { Ok(()) } else { Err(e.into()) };
                }
            }
            let basic = peek_basic(&h)?;
            match basic.command {
                USBIP_CMD_SUBMIT => {
                    let c = CmdSubmit::decode(&h)?;
                    let len = c.transfer_buffer_length;
                    if !(0..=MAX_URB_BYTES).contains(&len) {
                        bail!("URB length {len} out of range");
                    }
                    let mut out = vec![];
                    if c.basic.direction == DIR_OUT && len > 0 {
                        out = vec![0u8; len as usize];
                        r.read_exact(&mut out).await?;
                    }
                    let np = c.number_of_packets;
                    if np > 0 {
                        // Isochronous transfers are not supported yet: drain descriptors, fail the URB.
                        let mut iso = vec![0u8; np as usize * 16];
                        r.read_exact(&mut iso).await?;
                        let ret = RetSubmit { basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, ..c.basic },
                            status: EPIPE, actual_length: 0, start_frame: 0, number_of_packets: np, error_count: np };
                        let mut m = ret.encode();
                        m.extend_from_slice(&iso);
                        send(&tx, m);
                        continue;
                    }
                    sess.urbs.fetch_add(1, Ordering::Relaxed);
                    sess.rx.fetch_add(out.len() as u64, Ordering::Relaxed);
                    trace!("submit seq={} ep={} dir={} len={} setup={:02x?}", basic.seqnum, c.basic.ep, c.basic.direction, len, c.setup);
                    if trace_on() && out.len() == 31 && out.starts_with(b"USBC") {
                        // Mass-storage command block wrapper: show what SCSI command the importer's OS is sending.
                        trace!("   CBW tag={:08x} data_len={} flags={:02x} lun={} cdb_len={} cdb={:02x?}", u32::from_le_bytes([out[4], out[5], out[6], out[7]]), u32::from_le_bytes([out[8], out[9], out[10], out[11]]), out[12], out[13], out[14], &out[15..15 + (out[14] as usize).min(16)]);
                    }
                    if let Some(g) = &guard {
                        let verdict = g.lock().unwrap().on_submit(c.basic.ep as u8, c.basic.direction == DIR_IN, len as usize, &out);
                        if let Verdict::Complete { status, data } = verdict {
                            trace!("guard seq={} answered locally status={} bytes={}", basic.seqnum, status, data.len());
                            sess.tx.fetch_add(data.len() as u64, Ordering::Relaxed);
                            send(&tx, make_ret(&c, status, &data));
                            continue;
                        }
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    pending.lock().unwrap().insert(basic.seqnum, cancel.clone());
                    if let Some((q, b)) = &bot_q {
                        let is_in = c.basic.direction == DIR_IN;
                        let ep = c.basic.ep as u8;
                        // Control transfers join the queue: a host clears a stalled endpoint and retries the read at once, and the
                        // two must run in that order (on separate threads the read could overtake the clear and stall again).
                        if ep == 0 || (is_in && ep == b.ep_in & 0x0f) || (!is_in && ep == b.ep_out & 0x0f) {
                            if q.send(BotJob { c, out, cancel }).is_err() {
                                bail!("storage worker stopped");
                            }
                            continue;
                        }
                    }
                    let (tx2, dev2, pend2, sess2, guard2) = (tx.clone(), dev.clone(), pending.clone(), sess.clone(), guard.clone());
                    tokio::task::spawn_blocking(move || {
                        let is_in = c.basic.direction == DIR_IN;
                        let (status, mut data) = run_urb(&dev2, &c, out, &cancel);
                        note_gone(&dev2, &sess2);
                        if let Some(g) = &guard2 {
                            g.lock().unwrap().on_complete(c.basic.ep as u8, is_in, &mut data);
                        }
                        let live = pend2.lock().unwrap().remove(&c.basic.seqnum).is_some();
                        trace!("done   seq={} status={} bytes={} live={}", c.basic.seqnum, status, data.len(), live);
                        if cancel.load(Ordering::Relaxed) || !live {
                            return;
                        }
                        sess2.tx.fetch_add(data.len() as u64, Ordering::Relaxed);
                        let m = make_ret(&c, status, &data);
                        send(&tx2, m);
                    });
                }
                USBIP_CMD_UNLINK => {
                    let u = CmdUnlink::decode(&h)?;
                    trace!("unlink seq={} target={}", u.basic.seqnum, u.unlink_seqnum);
                    let found = pending.lock().unwrap().remove(&u.unlink_seqnum);
                    let status = match found {
                        Some(flag) => { flag.store(true, Ordering::Relaxed); ECONNRESET }
                        None => 0,
                    };
                    send(&tx, encode_ret_unlink(u.basic, status));
                }
                c => bail!("unexpected URB command {c:#x}"),
            }
        }
    }
    .await;
    // Cancel everything still in flight; dropping `dev` afterwards releases the device.
    for (_, f) in pending.lock().unwrap().drain() {
        f.store(true, Ordering::Relaxed);
    }
    drop(bot_q); // ends the storage worker once it has drained
    // Wait for in-flight workers so the device is really free before we report it.
    for _ in 0..100 {
        if Arc::strong_count(&dev) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Hand the device back to the caller, which either releases it (the usual case) or parks it for a reconnect.
    // If a worker is still stuck inside it, it is dropped when that worker finishes.
    let dev = Arc::try_unwrap(dev).ok().and_then(|l| l.into_inner().ok());
    LoopEnd { result, cause, dev }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn only_no_device_means_the_device_vanished() {
        assert!(device_vanished(&rusb::Error::NoDevice));
        for e in [rusb::Error::NotFound, rusb::Error::Pipe, rusb::Error::Timeout, rusb::Error::Io, rusb::Error::Busy, rusb::Error::Other] {
            assert!(!device_vanished(&e), "{e:?} must not end a session");
        }
        assert_eq!(map_err(rusb::Error::NoDevice), ENODEV);
    }

    #[tokio::test]
    async fn a_closed_stream_is_a_clean_end_and_a_cut_header_is_a_loss() {
        // Closed before any byte: what a proper TLS close_notify looks like to the reader.
        let (mut rd, wr) = tokio::io::duplex(256);
        drop(wr);
        let mut h = [0u8; URB_HEADER_LEN];
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Closed));

        // Cut in the middle of a header: the importer vanished mid-message.
        let (mut rd, mut wr) = tokio::io::duplex(256);
        wr.write_all(&[1u8; 10]).await.unwrap();
        drop(wr);
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Lost(_)));

        // A whole header is simply read.
        let (mut rd, mut wr) = tokio::io::duplex(256);
        wr.write_all(&[7u8; URB_HEADER_LEN]).await.unwrap();
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Got));
        assert_eq!(h, [7u8; URB_HEADER_LEN]);
    }
}
