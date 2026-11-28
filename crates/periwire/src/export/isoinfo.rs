//! What a device's isochronous endpoints may ask of the network, read from its descriptors: shown on the device card and used by
//! the connection check to say whether a link is good enough for *this* device.
use super::*;
use serde_json::{json, Value};

/// Data rate of one isochronous endpoint in kbit/s: bytes per service interval times how often that interval comes round.
/// `raw_mps` is the endpoint's max-packet field (bits 11-12 add up to two more packets per microframe on high speed);
/// `interval` is its bInterval (a power of two, in frames on full speed and microframes above).
pub(super) fn endpoint_kbps(raw_mps: u16, interval: u8, full_speed: bool) -> u32 {
    let per_service = u64::from(raw_mps & 0x7ff) * (1 + u64::from((raw_mps >> 11) & 3));
    let services_per_s = if full_speed { 1000u64 } else { 8000 } >> (interval.clamp(1, 16) - 1);
    (per_service * services_per_s * 8 / 1000) as u32
}

/// `{"kind": "video"|"audio"|"other", "in_kbps": n, "out_kbps": n}` for a device with isochronous endpoints (the most any one
/// alternate setting can use in each direction), or null for one without.
pub(super) fn iso_need(dev: &Device<Context>, speed: u32) -> Value {
    let Ok(cfg) = dev.active_config_descriptor().or_else(|_| dev.config_descriptor(0)) else { return Value::Null };
    let full = speed == 2 || speed == 1;
    let (mut in_max, mut out_max, mut kind) = (0u32, 0u32, "other");
    for i in cfg.interfaces() {
        for alt in i.descriptors() {
            let (mut ain, mut aout) = (0u32, 0u32);
            for ep in alt.endpoint_descriptors().filter(|e| e.transfer_type() == TransferType::Isochronous) {
                let k = endpoint_kbps(ep.max_packet_size(), ep.interval(), full);
                if ep.direction() == rusb::Direction::In {
                    ain += k;
                } else {
                    aout += k;
                }
            }
            if ain + aout == 0 {
                continue;
            }
            kind = match (kind, alt.class_code()) {
                ("other", 0x0e) => "video",
                ("other", 0x01) => "audio",
                (k, _) => k,
            };
            in_max = in_max.max(ain);
            out_max = out_max.max(aout);
        }
    }
    if in_max + out_max == 0 {
        Value::Null
    } else {
        json!({"kind": kind, "in_kbps": in_max, "out_kbps": out_max})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_follow_packet_size_and_interval() {
        assert_eq!(endpoint_kbps(1024, 1, false), 65_536, "1 KiB every microframe (8000/s) is 64 Mbit/s");
        assert_eq!(endpoint_kbps(1024 | (2 << 11), 1, false), 196_608, "three packets per microframe");
        assert_eq!(endpoint_kbps(192, 1, true), 1_536, "48 kHz 16-bit stereo audio on full speed: 192 bytes every ms");
        assert_eq!(endpoint_kbps(1024, 4, false), 8_192, "every 8th microframe (1 ms)");
        assert_eq!(endpoint_kbps(100, 0, true), 800, "a zero interval is treated as the shortest, not as a shift by -1");
    }
}
