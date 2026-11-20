//! USB devices on this computer: listing, describing, naming, and the filesystems mounted from them.
use super::*;

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

pub(super) fn speed_code(d: &Device<Context>) -> u32 {
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
        Some(c) => {
            (c.number(), c.interfaces().filter_map(|i| i.descriptors().next()).map(|a| UsbInterface { class: a.class_code(), subclass: a.sub_class_code(), protocol: a.protocol_code() }).collect())
        }
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

pub(super) fn shared_devices(specs: &[String]) -> Result<Vec<(UsbDevice, Vec<UsbInterface>)>> {
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

pub(super) fn clean(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()).filter(|s| !s.is_empty())
}

/// Strings from the device itself (short-lived handle; falls back to sysfs on Linux when we may not open it).
pub(super) fn read_strings(dev: &Device<Context>) -> Strings {
    let Ok(d) = dev.device_descriptor() else { return Strings::default() };
    if let Ok(h) = dev.open() {
        return Strings {
            manufacturer: clean(h.read_manufacturer_string_ascii(&d).ok()),
            product: clean(h.read_product_string_ascii(&d).ok()),
            serial: clean(h.read_serial_number_string_ascii(&d).ok()),
        };
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
    pub(super) fn strings(&self, dev: &Device<Context>, u: &UsbDevice) -> Strings {
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
        (false, false) => {
            if p.to_lowercase().contains(&m.to_lowercase()) {
                p.to_string()
            } else {
                format!("{m} {p}")
            }
        }
    }
}

/// Everything the UIs show about one local device.
pub(super) fn view_device(shared: &Shared, specs: &[String], dev: &Device<Context>, u: &UsbDevice, ifs: &[UsbInterface]) -> Value {
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
    match code {
        1 => "low (1.5 Mb/s)",
        2 => "full (12 Mb/s)",
        3 => "high (480 Mb/s)",
        5 => "super (5 Gb/s)",
        6 => "super+ (10 Gb/s)",
        _ => "unknown",
    }
}

pub fn class_name(c: u8) -> &'static str {
    match c {
        0x01 => "audio",
        0x02 => "communications",
        0x03 => "hid",
        0x05 => "physical",
        0x06 => "imaging",
        0x07 => "printer",
        0x08 => "storage",
        0x09 => "hub",
        0x0a => "cdc-data",
        0x0b => "smart-card",
        0x0d => "security",
        0x0e => "video",
        0x0f => "healthcare",
        0x10 => "av",
        0xe0 => "wireless",
        0xef => "misc",
        0xfe => "application",
        0xff => "vendor",
        _ => "other",
    }
}

/// Mount points of any filesystems on block devices that belong to this USB device (Linux only).
/// Forwarding a device out from under a mounted filesystem risks data loss, so we refuse.
#[cfg(target_os = "linux")]
pub fn mounts_for(busid: &str) -> Vec<(String, String)> {
    pub(super) fn walk(dir: &std::path::Path, depth: u32, names: &mut Vec<String>) {
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
    let names = with_holders(names);
    matching_mounts(&std::fs::read_to_string("/proc/self/mounts").or_else(|_| std::fs::read_to_string("/proc/mounts")).unwrap_or_default(), &names)
}

/// Block devices stacked on top of these (LUKS, LVM and other device-mapper layers), by their `dm-N` and `mapper/<name>` spellings.
#[cfg(target_os = "linux")]
pub(super) fn with_holders(names: Vec<String>) -> Vec<String> {
    let mut all = names.clone();
    let mut todo: Vec<String> = names;
    let mut seen = 0;
    while let Some(n) = todo.pop() {
        seen += 1;
        if seen > 64 {
            break;
        }
        let mut dirs = vec![std::path::PathBuf::from(format!("/sys/class/block/{n}/holders"))];
        // the partitions of a disk (sdb1, nvme0n1p1) list their own holders
        if let Ok(rd) = std::fs::read_dir(format!("/sys/class/block/{n}")) {
            dirs.extend(rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&n)).map(|e| e.path().join("holders")));
        }
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            for h in rd.flatten() {
                let dm = h.file_name().to_string_lossy().into_owned();
                if all.contains(&dm) {
                    continue;
                }
                if let Ok(name) = std::fs::read_to_string(format!("/sys/class/block/{dm}/dm/name")) {
                    all.push(format!("mapper/{}", name.trim()));
                }
                all.push(dm.clone());
                todo.push(dm);
            }
        }
    }
    all
}

/// `/proc/mounts` escapes a space, tab, newline or backslash in a field as a three-digit octal sequence (`My\040Drive`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn unescape_mount_field(f: &str) -> String {
    let b = f.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..=i + 3].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is `base` (a name under /dev) one of `names`, or a partition of it (`sdb` -> `sdb1`, `nvme0n1` -> `nvme0n1p1`)? `sdb` is not `sdba1`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn is_name_or_partition(base: &str, names: &[String]) -> bool {
    names.iter().any(|n| match base.strip_prefix(n.as_str()) {
        Some("") => true,
        Some(rest) => {
            // a name that ends in a digit takes `p` before the partition number (nvme0n1p1); one that does not takes the number directly (sdb1)
            let digits = if n.ends_with(|c: char| c.is_ascii_digit()) { rest.strip_prefix('p') } else { Some(rest) };
            digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
        }
        None => false,
    })
}

/// (device, mount point) of every mount whose device is one of `names` or a partition of one; spaces in names are restored.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn matching_mounts(mounts: &str, names: &[String]) -> Vec<(String, String)> {
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let (dev, mp) = (unescape_mount_field(f.next()?), unescape_mount_field(f.next()?));
            let base = dev.strip_prefix("/dev/")?;
            is_name_or_partition(base, names).then(|| (dev.clone(), mp))
        })
        .collect()
}
#[cfg(not(target_os = "linux"))]
pub fn mounts_for(_busid: &str) -> Vec<(String, String)> {
    vec![]
}
pub(super) fn mounted_filesystems(busid: &str) -> Vec<String> {
    mounts_for(busid).into_iter().map(|(d, m)| format!("{d} on {m}")).collect()
}
