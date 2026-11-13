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

/// How many bulk transfers the macOS importer keeps in flight per endpoint (`PERIWIRE_PIPELINE`, 1 turns read-ahead off), and how many
/// bytes at most. A network round trip per transfer is what limits throughput otherwise; a real host controller also works through
/// every transfer the driver has queued without waiting for the previous one to be acknowledged.
pub const DEFAULT_DEPTH: usize = 8;
pub const MAX_DEPTH: usize = 32;
pub const MAX_WINDOW_BYTES: u64 = 8 * 1024 * 1024;

pub fn depth_from(v: Option<&str>) -> usize {
    v.and_then(|v| v.trim().parse::<usize>().ok()).map(|n| n.clamp(1, MAX_DEPTH)).unwrap_or(DEFAULT_DEPTH)
}

/// Of the ring entries that follow the last one already submitted (`next`, in ring order), how many are plain data transfers that can
/// be submitted now: they stop at the first entry that is not yet valid, is a link (the ring wraps or moves there), a setup/status
/// stage or anything else, and at the depth and byte limits. `already` and `bytes` describe what is in flight.
pub fn run_ahead(next: impl Iterator<Item = Msg>, already: usize, bytes: u64, depth: usize) -> Vec<u32> {
    let mut out = vec![];
    let (mut n, mut b) = (already, bytes);
    for m in next {
        if n >= depth {
            break;
        }
        match decode(&m) {
            Some(Xfer::Data { len }) if b + u64::from(len) <= MAX_WINDOW_BYTES || n == 0 => {
                out.push(len);
                n += 1;
                b += u64::from(len);
            }
            _ => break,
        }
    }
    out
}

struct Slot<M, R> {
    tag: usize,
    seq: u32,
    meta: M,
    res: Option<R>,
}

/// The transfers submitted for one endpoint, oldest first. Results can arrive in any order but are handed out strictly in ring
/// order, because the host controller interface completes the ring's messages one after another.
pub struct Window<M, R> {
    q: std::collections::VecDeque<Slot<M, R>>,
}

impl<M, R> Default for Window<M, R> {
    fn default() -> Self {
        Window { q: Default::default() }
    }
}

impl<M, R> Window<M, R> {
    pub fn len(&self) -> usize {
        self.q.len()
    }
    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }
    pub fn back_tag(&self) -> Option<usize> {
        self.q.back().map(|s| s.tag)
    }
    pub fn metas(&self) -> impl Iterator<Item = &M> {
        self.q.iter().map(|s| &s.meta)
    }
    pub fn push(&mut self, tag: usize, seq: u32, meta: M) {
        self.q.push_back(Slot { tag, seq, meta, res: None });
    }
    /// Stores a result; false if no transfer with that number is waiting (it was unlinked meanwhile).
    pub fn finish(&mut self, seq: u32, res: R) -> bool {
        match self.q.iter_mut().find(|s| s.seq == seq && s.res.is_none()) {
            Some(s) => {
                s.res = Some(res);
                true
            }
            None => false,
        }
    }
    /// The oldest transfer, if its result is in.
    pub fn pop_ready(&mut self) -> Option<(usize, M, R)> {
        if self.q.front().is_some_and(|s| s.res.is_some()) {
            let s = self.q.pop_front().unwrap();
            Some((s.tag, s.meta, s.res.unwrap()))
        } else {
            None
        }
    }
    /// Empties the window and returns the numbers of the transfers that had no result yet (to unlink them).
    pub fn flush(&mut self) -> Vec<u32> {
        self.q.drain(..).filter(|s| s.res.is_none()).map(|s| s.seq).collect()
    }
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
    fn read_ahead_stops_at_anything_but_plain_data() {
        let d = |len| Msg { control: VALID | ty::NORMAL, data0: len, data1: 0x1000 };
        let link = Msg { control: VALID | ty::LINK, data0: 0, data1: 0 };
        let blank = Msg { control: ty::NORMAL, data0: 64, data1: 0 };
        assert_eq!(run_ahead([d(512), d(512), link, d(512)].into_iter(), 0, 0, 8), vec![512, 512], "a link ends the run");
        assert_eq!(run_ahead([d(1), blank, d(1)].into_iter(), 0, 0, 8), vec![1], "an entry the kernel has not finished writing ends it");
        assert_eq!(run_ahead(std::iter::repeat_n(d(16), 20), 3, 0, 8).len(), 5, "depth counts what is already in flight");
        assert_eq!(run_ahead(std::iter::repeat_n(d(16), 20), 8, 0, 8).len(), 0);
        let big = 6 * 1024 * 1024;
        assert_eq!(run_ahead([d(big), d(big)].into_iter(), 0, 0, 8), vec![big], "the byte limit holds back the second one");
        assert_eq!(run_ahead([d(20 * 1024 * 1024)].into_iter(), 0, 0, 8).len(), 1, "a single large transfer is still allowed");
        let setup = Msg { control: VALID | ty::SETUP, data0: 8, data1: 0 };
        assert!(run_ahead([setup].into_iter(), 0, 0, 8).is_empty());
    }

    #[test]
    fn depth_setting() {
        assert_eq!(depth_from(None), DEFAULT_DEPTH);
        assert_eq!(depth_from(Some("1")), 1);
        assert_eq!(depth_from(Some(" 4 ")), 4);
        assert_eq!(depth_from(Some("0")), 1, "never below one");
        assert_eq!(depth_from(Some("1000")), MAX_DEPTH);
        assert_eq!(depth_from(Some("lots")), DEFAULT_DEPTH);
    }

    #[test]
    fn results_leave_the_window_in_ring_order() {
        let mut w: Window<&str, u32> = Window::default();
        w.push(0x100, 1, "a");
        w.push(0x110, 2, "b");
        w.push(0x120, 3, "c");
        assert_eq!((w.back_tag(), w.len()), (Some(0x120), 3));
        assert!(w.finish(3, 33) && w.finish(2, 22));
        assert!(w.pop_ready().is_none(), "the oldest is still out: nothing may complete yet");
        assert!(w.finish(1, 11));
        assert_eq!(w.pop_ready(), Some((0x100, "a", 11)));
        assert_eq!(w.pop_ready(), Some((0x110, "b", 22)));
        assert_eq!(w.pop_ready(), Some((0x120, "c", 33)));
        assert!(w.pop_ready().is_none() && w.is_empty());
    }

    #[test]
    fn a_flush_names_only_the_transfers_still_out() {
        let mut w: Window<(), u8> = Window::default();
        w.push(1, 10, ());
        w.push(2, 11, ());
        w.push(3, 12, ());
        w.finish(11, 0);
        assert_eq!(w.flush(), vec![10, 12], "the one whose result is already here needs no unlink");
        assert!(w.is_empty());
        assert!(!w.finish(10, 0), "a result for a flushed transfer is ignored");
        assert!(!w.finish(99, 0));
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
