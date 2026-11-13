#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
//! Platform-independent pieces of the macOS importer: decoding of the messages that
//! `IOUSBHostControllerInterface` exchanges with the kernel, device-address allocation,
//! endpoint-descriptor parsing and status mapping. Bit layouts follow
//! `IOUSBHostControllerInterfaceDefinitions.h` (checked against the SDK in `mac.rs` tests).

pub const TYPE_MASK: u32 = 0x3f;
pub const NO_RESPONSE: u32 = 1 << 14;
pub const VALID: u32 = 1 << 15;
pub const LEN_MASK: u32 = 0x0fff_ffff;

/// Message type codes (the low six bits of `control`).
pub mod ty {
    pub const CONTROLLER_POWER_ON: u32 = 0x10;
    pub const CONTROLLER_PAUSE: u32 = 0x13;
    pub const CONTROLLER_FRAME_NUMBER: u32 = 0x14;
    pub const PORT_POWER_ON: u32 = 0x18;
    pub const PORT_RESET: u32 = 0x1c;
    pub const PORT_STATUS: u32 = 0x1e;
    pub const DEVICE_CREATE: u32 = 0x20;
    pub const DEVICE_DESTROY: u32 = 0x21;
    pub const DEVICE_PAUSE: u32 = 0x23;
    pub const DEVICE_UPDATE: u32 = 0x24;
    pub const ENDPOINT_CREATE: u32 = 0x28;
    pub const ENDPOINT_DESTROY: u32 = 0x29;
    pub const ENDPOINT_PAUSE: u32 = 0x2b;
    pub const ENDPOINT_UPDATE: u32 = 0x2c;
    pub const ENDPOINT_RESET: u32 = 0x2d;
    pub const ENDPOINT_SET_NEXT_TRANSFER: u32 = 0x2e;
    pub const SETUP: u32 = 0x38;
    pub const NORMAL: u32 = 0x39;
    pub const STATUS: u32 = 0x3a;
    pub const ISOCHRONOUS: u32 = 0x3b;
    pub const LINK: u32 = 0x3c;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Msg {
    pub control: u32,
    pub data0: u32,
    pub data1: u64,
}

impl Msg {
    pub fn kind(&self) -> u32 {
        self.control & TYPE_MASK
    }
    pub fn valid(&self) -> bool {
        self.control & VALID != 0
    }
    pub fn wants_completion(&self) -> bool {
        self.control & NO_RESPONSE == 0
    }
    /// Device address of a command (data0 bits 0-7).
    pub fn device(&self) -> u8 {
        self.data0 as u8
    }
    /// Endpoint address of a command (data0 bits 8-15).
    pub fn endpoint(&self) -> u8 {
        (self.data0 >> 8) as u8
    }
    pub fn length(&self) -> u32 {
        self.data0 & LEN_MASK
    }
    pub fn buffer(&self) -> u64 {
        self.data1
    }
}

/// One decoded transfer-ring entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Xfer {
    Setup([u8; 8]),
    Data { len: u32 },
    Status,
    Iso,
    Link { respond: bool },
    Unknown(u32),
}

/// `None` when the slot is not (yet) a valid message.
pub fn decode(m: &Msg) -> Option<Xfer> {
    if !m.valid() {
        return None;
    }
    Some(match m.kind() {
        ty::SETUP => Xfer::Setup(m.data1.to_le_bytes()),
        ty::NORMAL => Xfer::Data { len: m.length() },
        ty::STATUS => Xfer::Status,
        ty::ISOCHRONOUS => Xfer::Iso,
        ty::LINK => Xfer::Link { respond: m.wants_completion() },
        k => Xfer::Unknown(k),
    })
}

/// Doorbell layout: device address in bits 0-7, endpoint address in bits 8-15.
pub fn doorbell_target(d: u32) -> (u8, u8) {
    (d as u8, (d >> 8) as u8)
}

/// Completion status as reported to the host controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Offline,
    Timeout,
    Stopped,
    Protocol,
    Transaction,
    Overrun,
    Stall,
    Error,
}

/// Maps the (negative) Linux errno carried in a RET_SUBMIT status.
pub fn outcome_from_urb_status(status: i32) -> Outcome {
    match status.unsigned_abs() {
        0 | 18 | 121 => Outcome::Success, // short transfers are not failures here
        32 => Outcome::Stall,
        75 => Outcome::Overrun,
        71 | 84 => Outcome::Protocol,
        62 | 110 => Outcome::Timeout,
        19 | 108 => Outcome::Offline,
        2 | 104 => Outcome::Stopped,
        63 | 70 => Outcome::Transaction,
        _ => Outcome::Error,
    }
}

/// Allocator for USB device addresses 1..=127.
#[derive(Debug, Default)]
pub struct Addresses(u128);

impl Addresses {
    pub fn take(&mut self) -> Option<u8> {
        let a = (1u8..128).find(|a| self.0 & (1u128 << a) == 0)?;
        self.0 |= 1u128 << a;
        Some(a)
    }
    pub fn give_back(&mut self, a: u8) {
        if a < 128 {
            self.0 &= !(1u128 << a);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Control,
    Iso,
    Bulk,
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpDesc {
    pub kind: Kind,
    pub max_packet: u16,
    pub interval: u8,
}

impl EpDesc {
    pub const EP0: Self = Self { kind: Kind::Control, max_packet: 64, interval: 0 };

    /// Parses a 7+ byte endpoint descriptor, or the first endpoint descriptor inside the synthetic
    /// configuration descriptor that macOS passes for non-default endpoints.
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.len() >= 4 && d[0] >= 2 && d[1] == 2 {
            let total = (u16::from_le_bytes([d[2], d[3]]) as usize).min(d.len());
            let mut i = d[0] as usize;
            while i + 2 <= total && d[i] >= 2 {
                if d[i + 1] == 5 {
                    return Self::parse(&d[i..total]);
                }
                i += d[i] as usize;
            }
            return None;
        }
        if d.len() < 7 || d[1] != 5 {
            return None;
        }
        Some(Self {
            kind: match d[3] & 3 {
                0 => Kind::Control,
                1 => Kind::Iso,
                2 => Kind::Bulk,
                _ => Kind::Interrupt,
            },
            max_packet: u16::from_le_bytes([d[4], d[5]]),
            interval: d[6],
        })
    }

    /// The URB `interval` field a Linux exporter expects: 2^(bInterval-1) microframes at high
    /// speed and above, frames otherwise; zero for bulk and control.
    pub fn urb_interval(&self, speed: u32) -> u32 {
        let b = u32::from(self.interval.max(1));
        match self.kind {
            Kind::Bulk | Kind::Control => 0,
            Kind::Iso => 1 << (b.min(16) - 1),
            Kind::Interrupt if speed >= 3 => 1 << (b.min(16) - 1),
            Kind::Interrupt => b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_ring_entries() {
        let setup = Msg { control: VALID | ty::SETUP, data0: 8, data1: u64::from_le_bytes([0x80, 6, 0, 1, 0, 0, 18, 0]) };
        assert_eq!(decode(&setup), Some(Xfer::Setup([0x80, 6, 0, 1, 0, 0, 18, 0])));
        let data = Msg { control: VALID | ty::NORMAL, data0: 512, data1: 0x1000 };
        assert_eq!(decode(&data), Some(Xfer::Data { len: 512 }));
        assert_eq!(decode(&Msg { control: ty::NORMAL, ..data }), None, "not valid yet");
        let link = Msg { control: VALID | NO_RESPONSE | ty::LINK, data0: 0, data1: 0x2000 };
        assert_eq!(decode(&link), Some(Xfer::Link { respond: false }));
        assert_eq!(decode(&Msg { control: VALID | ty::LINK, ..link }), Some(Xfer::Link { respond: true }));
    }

    #[test]
    fn doorbell_and_command_fields() {
        assert_eq!(doorbell_target(0x0000_8105), (5, 0x81));
        let m = Msg { control: VALID, data0: 0x8105, data1: 0 };
        assert_eq!((m.device(), m.endpoint()), (5, 0x81));
        assert_eq!(Msg { data0: 0xfff_ffff | 0xf000_0000, ..Default::default() }.length(), 0x0fff_ffff);
    }

    #[test]
    fn maps_linux_errnos() {
        assert_eq!(outcome_from_urb_status(0), Outcome::Success);
        assert_eq!(outcome_from_urb_status(-32), Outcome::Stall);
        assert_eq!(outcome_from_urb_status(-110), Outcome::Timeout);
        assert_eq!(outcome_from_urb_status(-19), Outcome::Offline);
        assert_eq!(outcome_from_urb_status(-104), Outcome::Stopped);
        assert_eq!(outcome_from_urb_status(-75), Outcome::Overrun);
        assert_eq!(outcome_from_urb_status(-1234), Outcome::Error);
    }

    #[test]
    fn address_allocation_is_lowest_free() {
        let mut a = Addresses::default();
        assert_eq!((a.take(), a.take(), a.take()), (Some(1), Some(2), Some(3)));
        a.give_back(2);
        assert_eq!(a.take(), Some(2));
        for _ in 0..124 {
            a.take();
        }
        assert_eq!(a.take(), None, "only 1..=127 exist");
    }

    #[test]
    fn parses_endpoint_descriptors() {
        let bulk = [7, 5, 0x81, 2, 0, 4, 0];
        assert_eq!(EpDesc::parse(&bulk), Some(EpDesc { kind: Kind::Bulk, max_packet: 1024, interval: 0 }));
        let intr = [7, 5, 0x83, 3, 8, 0, 4];
        let d = EpDesc::parse(&intr).unwrap();
        assert_eq!((d.kind, d.max_packet, d.interval), (Kind::Interrupt, 8, 4));
        // inside a synthetic configuration descriptor
        let mut cfg = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 50];
        cfg.extend_from_slice(&[9, 4, 0, 0, 1, 3, 0, 0, 0]);
        cfg.extend_from_slice(&intr);
        let total = cfg.len() as u16;
        cfg[2..4].copy_from_slice(&total.to_le_bytes());
        assert_eq!(EpDesc::parse(&cfg), Some(d));
        assert_eq!(EpDesc::parse(&[4, 3, 0, 0]), None);
    }

    #[test]
    fn urb_interval_follows_speed() {
        let i = EpDesc { kind: Kind::Interrupt, max_packet: 8, interval: 4 };
        assert_eq!(i.urb_interval(3), 8); // high speed: 2^(4-1) microframes
        assert_eq!(i.urb_interval(2), 4); // full speed: frames
        assert_eq!(EpDesc { kind: Kind::Bulk, max_packet: 512, interval: 0 }.urb_interval(5), 0);
        assert_eq!(EpDesc { interval: 0, ..i }.urb_interval(2), 1, "never zero for periodic endpoints");
    }
}
