//! Turns raw USB identifiers into things a person recognises: "Android device", "iPad in DFU mode", "Storage drive".

pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
}

const fn k(id: &'static str, label: &'static str) -> Kind {
    Kind { id, label }
}

/// Phone vendors whose USB ids are almost always Android devices.
const ANDROID_VIDS: &[u16] = &[0x18d1, 0x04e8, 0x22b8, 0x12d1, 0x2717, 0x0bb4, 0x1004, 0x0fce, 0x2a70, 0x05c6, 0x0489, 0x1bbb, 0x2916, 0x19d2, 0x0502, 0x1949, 0x2d95, 0x22d9, 0x2ae5];
const KEY_VIDS: &[u16] = &[0x1050, 0x096e, 0x2581, 0x20a0, 0x1209];
const SERIAL_VIDS: &[u16] = &[0x0403, 0x10c4, 0x067b, 0x1a86, 0x2341, 0x239a];

/// `ifaces` are (class, subclass, protocol) of each interface in the active configuration.
pub fn classify(vid: u16, pid: u16, dev_class: u8, ifaces: &[(u8, u8, u8)], product: &str) -> Kind {
    let p = product.to_lowercase();
    // Apple: normal mode vs the special boot modes people flash devices from.
    if vid == 0x05ac {
        match pid {
            0x1227 | 0x1222 => return k("apple-dfu", "Apple device in DFU mode"),
            0x1280..=0x1283 => return k("apple-recovery", "Apple device in Recovery mode"),
            0x1290..=0x12af => {
                return if p.contains("ipad") {
                    k("apple-ipad", "iPad")
                } else if p.contains("ipod") {
                    k("apple-iphone", "iPod")
                } else {
                    k("apple-iphone", "iPhone / iPad")
                };
            }
            _ => {}
        }
        if p.contains("ipad") {
            return k("apple-ipad", "iPad");
        }
        if p.contains("iphone") {
            return k("apple-iphone", "iPhone");
        }
    }
    let has = |c: u8| ifaces.iter().any(|i| i.0 == c) || dev_class == c;
    // Android: ADB (ff/42/01) and fastboot (ff/42/03) interfaces, or a known phone vendor.
    if ifaces.iter().any(|i| i.0 == 0xff && i.1 == 0x42 && i.2 == 0x03) {
        return k("android-fastboot", "Android device (bootloader)");
    }
    if ifaces.iter().any(|i| i.0 == 0xff && i.1 == 0x42 && i.2 == 0x01) || p.contains("android") {
        return k("android", "Android device");
    }
    if ANDROID_VIDS.contains(&vid) && (has(0x06) || has(0xff) || has(0x08)) {
        return k("android", "Android device");
    }
    if KEY_VIDS.contains(&vid) && (has(0x0b) || has(0x03) || p.contains("key")) {
        return k("security-key", "Security key");
    }
    if has(0x08) {
        return k("storage", "Storage drive");
    }
    if has(0x0e) {
        return k("webcam", "Camera / webcam");
    }
    if has(0x01) && !has(0x03) {
        return k("audio", "Audio device");
    }
    if has(0x07) {
        return k("printer", "Printer");
    }
    if has(0xe0) && ifaces.iter().any(|i| i.1 == 0x01 && i.2 == 0x01) {
        return k("bluetooth", "Bluetooth adapter");
    }
    if has(0x06) {
        return k("camera", "Camera / phone (photo transfer)");
    }
    if has(0x0b) {
        return k("security-key", "Smart-card reader");
    }
    if ifaces.iter().any(|i| i.0 == 0x03 && i.1 == 1 && i.2 == 1) {
        return k("keyboard", "Keyboard");
    }
    if ifaces.iter().any(|i| i.0 == 0x03 && i.1 == 1 && i.2 == 2) {
        return k("mouse", "Mouse / pointer");
    }
    if SERIAL_VIDS.contains(&vid) || has(0x0a) || ifaces.iter().any(|i| i.0 == 0x02 && i.1 == 0x02) {
        return k("serial", "Serial / debug adapter");
    }
    if ifaces.iter().any(|i| i.0 == 0x02 && i.1 == 0x06) || p.contains("ethernet") || p.contains("lan") {
        return k("network", "Network adapter");
    }
    if has(0x03) {
        return k("hid", "Input device");
    }
    k("other", "USB device")
}

#[cfg(test)]
mod tests {
    use super::classify;
    #[test]
    fn recognises_common_devices() {
        assert_eq!(classify(0x05ac, 0x12ab, 0, &[(6, 1, 1)], "iPad").id, "apple-ipad");
        assert_eq!(classify(0x05ac, 0x1227, 0, &[], "").id, "apple-dfu");
        assert_eq!(classify(0x05ac, 0x1281, 0, &[], "").id, "apple-recovery");
        assert_eq!(classify(0x05ac, 0x12a8, 0, &[(6, 1, 1)], "iPhone").id, "apple-iphone");
        assert_eq!(classify(0x18a5, 0x0243, 0, &[(8, 6, 0x50)], "").id, "storage");
        assert_eq!(classify(0x18d1, 0x4ee7, 0, &[(0xff, 0x42, 1)], "Pixel").id, "android");
        assert_eq!(classify(0x18d1, 0x4ee0, 0, &[(0xff, 0x42, 3)], "").id, "android-fastboot");
        assert_eq!(classify(0x1050, 0x0407, 0, &[(3, 0, 0), (0x0b, 0, 0)], "YubiKey").id, "security-key");
        assert_eq!(classify(0x046d, 0x085e, 0xef, &[(0x0e, 1, 0), (0x01, 1, 0)], "BRIO").id, "webcam");
        assert_eq!(classify(0x04d9, 0x0006, 0, &[(3, 1, 1)], "").id, "keyboard");
        assert_eq!(classify(0x1234, 0x5678, 0, &[(0x77, 0, 0)], "").id, "other");
    }
}
