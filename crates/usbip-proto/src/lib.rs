//! USB/IP wire protocol (the format Linux's `usbip`/`vhci_hcd` speak).
//! All integers are big-endian. Layouts follow Documentation/usb/usbip_protocol.rst.

use thiserror::Error;

pub const USBIP_VERSION: u16 = 0x0111;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;
/// PeriWire extension (not part of USB/IP): ask an exporter for human-readable info about the devices it shares.
/// Reply: `OpHeader(OP_REP_DEVINFO, status)`, then a big-endian u32 length and that many bytes of JSON.
pub const OP_REQ_DEVINFO: u16 = 0x8f01;
pub const OP_REP_DEVINFO: u16 = 0x0f01;
/// PeriWire extension: before importing, tell the exporter how this session should behave. Body: big-endian u32 flags.
/// Reply: `OpHeader(OP_REP_FLAGS, accepted)` where `accepted` holds only the optional-feature bits the exporter agreed to
/// (`FLAG_ACCEL`, `FLAG_NTFS_JOURNAL`); it is 0 otherwise. The import request follows on the same connection.
pub const OP_REQ_FLAGS: u16 = 0x8f02;
pub const OP_REP_FLAGS: u16 = 0x0f02;
/// Session flag: write-protect the device (the exporter blocks every write to USB storage and reports it write-protected).
pub const FLAG_READ_ONLY: u32 = 1;
/// Session flag: round-trip savings for USB mass storage. The importer answers the command-block write itself (the
/// exporter still performs it, in order) and the exporter appends the status block to the reply of the last data
/// transfer, so a storage command costs one network round trip instead of three. The exporter confirms by returning
/// `FLAG_ACCEL` in the `status` field of `OP_REP_FLAGS` (an older exporter returns 0 and the importer then does not use it).
pub const FLAG_ACCEL: u32 = 4;
/// Session flag: NTFS drives shared read-only are shown with a valid, clean journal header when their own journal was
/// emptied. Windows must write to start such a journal and a write-protected disk refuses, so it would not mount the
/// volume at all. Only the journal's first two pages are altered, only in what this session reads; nothing on the drive
/// changes. The exporter confirms with `FLAG_NTFS_JOURNAL` in `OP_REP_FLAGS` (an older exporter ignores the request).
pub const FLAG_NTFS_JOURNAL: u32 = 8;
/// Session flag: "I reconnect by myself, and I close the connection cleanly (TLS close_notify) when I mean to detach."
/// If the connection ends any other way (the network dropped, the importer crashed), the exporter keeps the device claimed
/// for a grace period instead of handing it back to its own desktop, so the same importer can resume before anything
/// else mounts the drive. The exporter confirms with `FLAG_RESUME` in `OP_REP_FLAGS` (an older exporter ignores the request).
pub const FLAG_RESUME: u32 = 16;
/// Marks a `RET_SUBMIT` that carries a Bulk-Only status block after its data. Bytes 40..44 of the (otherwise zero) padding:
/// `b'P', b'W', <status endpoint number>, 1`. Only ever sent after `FLAG_ACCEL` was agreed, and removed by the importer
/// before the message reaches the operating system.
pub const CSW_MARK: [u8; 2] = *b"PW";
pub const CSW_LEN: usize = 13;
/// Marker bytes for a status block read from endpoint `ep` (number, no direction bit).
pub fn csw_mark(ep: u8) -> [u8; 4] {
    [CSW_MARK[0], CSW_MARK[1], ep & 0x0f, 1]
}
/// The endpoint number if `hdr` (a 48-byte `RET_SUBMIT`) carries the marker.
pub fn csw_mark_ep(hdr: &[u8]) -> Option<u8> {
    (hdr.len() >= URB_HEADER_LEN && hdr[40..42] == CSW_MARK && hdr[43] == 1).then(|| hdr[42])
}

/// `OP_REP_IMPORT` status when read-only was requested but cannot be enforced for this kind of device.
pub const IMPORT_READ_ONLY_UNSUPPORTED: u32 = 2;

/// The `OP_REQ_FLAGS` message (header plus flags).
pub fn encode_flags_request(flags: u32) -> Vec<u8> {
    let mut out = OpHeader::new(OP_REQ_FLAGS, 0).encode().to_vec();
    out.extend_from_slice(&flags.to_be_bytes());
    out
}

pub const USBIP_CMD_SUBMIT: u32 = 0x0001;
pub const USBIP_CMD_UNLINK: u32 = 0x0002;
pub const USBIP_RET_SUBMIT: u32 = 0x0003;
pub const USBIP_RET_UNLINK: u32 = 0x0004;

pub const DEVICE_LEN: usize = 312;
pub const URB_HEADER_LEN: usize = 48;

#[derive(Debug, Error, PartialEq)]
pub enum ProtoError {
    #[error("buffer too short: need {need}, have {have}")]
    Short { need: usize, have: usize },
    #[error("unexpected version {0:#06x}")]
    Version(u16),
    #[error("unexpected command {0:#x}")]
    Command(u32),
    #[error("{what} {value} is outside 0..={max}")]
    OutOfRange { what: &'static str, value: i64, max: i64 },
}

fn need(buf: &[u8], n: usize) -> Result<(), ProtoError> {
    if buf.len() < n {
        Err(ProtoError::Short { need: n, have: buf.len() })
    } else {
        Ok(())
    }
}
fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
fn put_cstr(out: &mut Vec<u8>, s: &str, len: usize) {
    let mut f = vec![0u8; len];
    let n = s.len().min(len - 1);
    f[..n].copy_from_slice(&s.as_bytes()[..n]);
    out.extend_from_slice(&f);
}

/// 8-byte header that starts every OP_* message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpHeader {
    pub version: u16,
    pub code: u16,
    pub status: u32,
}

impl OpHeader {
    pub fn new(code: u16, status: u32) -> Self {
        Self { version: USBIP_VERSION, code, status }
    }
    pub fn encode(&self) -> [u8; 8] {
        let mut o = [0u8; 8];
        o[0..2].copy_from_slice(&self.version.to_be_bytes());
        o[2..4].copy_from_slice(&self.code.to_be_bytes());
        o[4..8].copy_from_slice(&self.status.to_be_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        need(b, 8)?;
        Ok(Self { version: be16(b, 0), code: be16(b, 2), status: be32(b, 4) })
    }
}

/// One exported device descriptor (312 bytes on the wire).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsbDevice {
    pub path: String,
    pub busid: String,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub configuration_value: u8,
    pub num_configurations: u8,
    pub num_interfaces: u8,
}

impl UsbDevice {
    pub fn encode(&self, out: &mut Vec<u8>) {
        put_cstr(out, &self.path, 256);
        put_cstr(out, &self.busid, 32);
        out.extend_from_slice(&self.busnum.to_be_bytes());
        out.extend_from_slice(&self.devnum.to_be_bytes());
        out.extend_from_slice(&self.speed.to_be_bytes());
        out.extend_from_slice(&self.id_vendor.to_be_bytes());
        out.extend_from_slice(&self.id_product.to_be_bytes());
        out.extend_from_slice(&self.bcd_device.to_be_bytes());
        out.extend_from_slice(&[self.device_class, self.device_subclass, self.device_protocol, self.configuration_value, self.num_configurations, self.num_interfaces]);
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        need(b, DEVICE_LEN)?;
        Ok(Self {
            path: cstr(&b[0..256]),
            busid: cstr(&b[256..288]),
            busnum: be32(b, 288),
            devnum: be32(b, 292),
            speed: be32(b, 296),
            id_vendor: be16(b, 300),
            id_product: be16(b, 302),
            bcd_device: be16(b, 304),
            device_class: b[306],
            device_subclass: b[307],
            device_protocol: b[308],
            configuration_value: b[309],
            num_configurations: b[310],
            num_interfaces: b[311],
        })
    }
}

/// Interface triple listed after each device in OP_REP_DEVLIST (4 bytes, last is padding).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsbInterface {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

pub fn encode_devlist_reply(devs: &[(UsbDevice, Vec<UsbInterface>)]) -> Vec<u8> {
    let mut out = OpHeader::new(OP_REP_DEVLIST, 0).encode().to_vec();
    out.extend_from_slice(&(devs.len() as u32).to_be_bytes());
    for (d, ifs) in devs {
        d.encode(&mut out);
        for i in ifs {
            out.extend_from_slice(&[i.class, i.subclass, i.protocol, 0]);
        }
    }
    out
}

pub fn encode_import_request(busid: &str) -> Vec<u8> {
    let mut out = OpHeader::new(OP_REQ_IMPORT, 0).encode().to_vec();
    put_cstr(&mut out, busid, 32);
    out
}

pub fn decode_import_request_busid(body: &[u8]) -> Result<String, ProtoError> {
    need(body, 32)?;
    Ok(cstr(&body[..32]))
}

/// Direction as carried in URB headers.
pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

/// Fields common to all URB messages (20 bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UrbBasic {
    pub command: u32,
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CmdSubmit {
    pub basic: UrbBasic,
    pub transfer_flags: u32,
    pub transfer_buffer_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub interval: i32,
    pub setup: [u8; 8],
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetSubmit {
    pub basic: UrbBasic,
    pub status: i32,
    pub actual_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub error_count: i32,
}

/// Largest data stage of one transfer either side will buffer. Without a bound a peer could make us allocate gigabytes with one header.
pub const MAX_URB_BYTES: i32 = 16 * 1024 * 1024;
/// Most isochronous packet descriptors one transfer may carry.
pub const MAX_ISO_PACKETS: i32 = 4096;

fn in_range(what: &'static str, value: i32, max: i32) -> Result<(), ProtoError> {
    if (0..=max).contains(&value) {
        Ok(())
    } else {
        Err(ProtoError::OutOfRange { what, value: value.into(), max: max.into() })
    }
}

impl CmdSubmit {
    /// Reject lengths and packet counts no honest peer sends, before anything is allocated for them.
    pub fn validate(&self) -> Result<(), ProtoError> {
        in_range("endpoint number", i32::try_from(self.basic.ep).unwrap_or(i32::MAX), 15)?;
        in_range("URB length", self.transfer_buffer_length, MAX_URB_BYTES)?;
        in_range("isochronous packet count", self.number_of_packets, MAX_ISO_PACKETS)
    }
}

impl RetSubmit {
    /// Same for a reply: the length it announces and the packet descriptors that would follow it.
    pub fn validate(&self) -> Result<(), ProtoError> {
        in_range("reply length", self.actual_length, MAX_URB_BYTES)?;
        in_range("isochronous packet count", self.number_of_packets, MAX_ISO_PACKETS)
    }
}

fn put_basic(out: &mut Vec<u8>, b: &UrbBasic) {
    for v in [b.command, b.seqnum, b.devid, b.direction, b.ep] {
        out.extend_from_slice(&v.to_be_bytes());
    }
}
fn get_basic(b: &[u8]) -> UrbBasic {
    UrbBasic { command: be32(b, 0), seqnum: be32(b, 4), devid: be32(b, 8), direction: be32(b, 12), ep: be32(b, 16) }
}
pub fn peek_basic(b: &[u8]) -> Result<UrbBasic, ProtoError> {
    need(b, 20)?;
    Ok(get_basic(b))
}

impl CmdSubmit {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(URB_HEADER_LEN);
        put_basic(&mut o, &self.basic);
        o.extend_from_slice(&self.transfer_flags.to_be_bytes());
        o.extend_from_slice(&self.transfer_buffer_length.to_be_bytes());
        o.extend_from_slice(&self.start_frame.to_be_bytes());
        o.extend_from_slice(&self.number_of_packets.to_be_bytes());
        o.extend_from_slice(&self.interval.to_be_bytes());
        o.extend_from_slice(&self.setup);
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        need(b, URB_HEADER_LEN)?;
        let basic = get_basic(b);
        if basic.command != USBIP_CMD_SUBMIT {
            return Err(ProtoError::Command(basic.command));
        }
        let mut setup = [0u8; 8];
        setup.copy_from_slice(&b[40..48]);
        Ok(Self {
            basic,
            transfer_flags: be32(b, 20),
            transfer_buffer_length: be32(b, 24) as i32,
            start_frame: be32(b, 28) as i32,
            number_of_packets: be32(b, 32) as i32,
            interval: be32(b, 36) as i32,
            setup,
        })
    }
}

impl RetSubmit {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(URB_HEADER_LEN);
        put_basic(&mut o, &self.basic);
        for v in [self.status, self.actual_length, self.start_frame, self.number_of_packets, self.error_count] {
            o.extend_from_slice(&v.to_be_bytes());
        }
        o.extend_from_slice(&[0u8; 8]);
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        need(b, URB_HEADER_LEN)?;
        let basic = get_basic(b);
        if basic.command != USBIP_RET_SUBMIT {
            return Err(ProtoError::Command(basic.command));
        }
        Ok(Self { basic, status: be32(b, 20) as i32, actual_length: be32(b, 24) as i32, start_frame: be32(b, 28) as i32, number_of_packets: be32(b, 32) as i32, error_count: be32(b, 36) as i32 })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CmdUnlink {
    pub basic: UrbBasic,
    pub unlink_seqnum: u32,
}

impl CmdUnlink {
    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        need(b, URB_HEADER_LEN)?;
        let basic = get_basic(b);
        if basic.command != USBIP_CMD_UNLINK {
            return Err(ProtoError::Command(basic.command));
        }
        Ok(Self { basic, unlink_seqnum: be32(b, 20) })
    }
}

pub fn encode_ret_unlink(basic: UrbBasic, status: i32) -> Vec<u8> {
    let mut o = Vec::with_capacity(URB_HEADER_LEN);
    put_basic(&mut o, &UrbBasic { command: USBIP_RET_UNLINK, ..basic });
    o.extend_from_slice(&status.to_be_bytes());
    o.extend_from_slice(&[0u8; 24]);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UsbDevice {
        UsbDevice {
            path: "/sys/devices/pci0000:00/usb2/2-2".into(),
            busid: "2-2".into(),
            busnum: 2,
            devnum: 7,
            speed: 5,
            id_vendor: 0x18a5,
            id_product: 0x0243,
            bcd_device: 0x0110,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 1,
        }
    }

    #[test]
    fn op_header_roundtrip_and_size() {
        let h = OpHeader::new(OP_REQ_DEVLIST, 0);
        assert_eq!(h.encode(), [0x01, 0x11, 0x80, 0x05, 0, 0, 0, 0]);
        assert_eq!(OpHeader::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn device_is_312_bytes_and_roundtrips() {
        let mut v = Vec::new();
        sample().encode(&mut v);
        assert_eq!(v.len(), DEVICE_LEN);
        assert_eq!(UsbDevice::decode(&v).unwrap(), sample());
    }

    #[test]
    fn devlist_reply_layout() {
        let r = encode_devlist_reply(&[(sample(), vec![UsbInterface { class: 8, subclass: 6, protocol: 0x50 }])]);
        assert_eq!(r.len(), 8 + 4 + DEVICE_LEN + 4);
        assert_eq!(&r[8..12], &[0, 0, 0, 1]);
    }

    #[test]
    fn import_request_roundtrip() {
        let r = encode_import_request("2-2");
        assert_eq!(r.len(), 8 + 32);
        assert_eq!(decode_import_request_busid(&r[8..]).unwrap(), "2-2");
    }

    #[test]
    fn urb_headers_are_48_bytes_and_roundtrip() {
        let c = CmdSubmit {
            basic: UrbBasic { command: USBIP_CMD_SUBMIT, seqnum: 9, devid: 0x20007, direction: DIR_IN, ep: 1 },
            transfer_flags: 0,
            transfer_buffer_length: 512,
            start_frame: 0,
            number_of_packets: -1,
            interval: 0,
            setup: [0x80, 6, 0, 1, 0, 0, 0x12, 0],
        };
        let e = c.encode();
        assert_eq!(e.len(), URB_HEADER_LEN);
        assert_eq!(CmdSubmit::decode(&e).unwrap(), c);
        let r = RetSubmit { basic: UrbBasic { command: USBIP_RET_SUBMIT, ..c.basic }, status: 0, actual_length: 18, start_frame: 0, number_of_packets: -1, error_count: 0 };
        assert_eq!(r.encode().len(), URB_HEADER_LEN);
        assert_eq!(RetSubmit::decode(&r.encode()).unwrap(), r);
        assert_eq!(encode_ret_unlink(c.basic, -104).len(), URB_HEADER_LEN);
    }

    #[test]
    fn short_buffers_error() {
        assert!(matches!(UsbDevice::decode(&[0u8; 10]), Err(ProtoError::Short { .. })));
        assert!(matches!(CmdSubmit::decode(&[0u8; 47]), Err(ProtoError::Short { .. })));
    }

    #[test]
    fn status_marker_round_trips_and_ignores_plain_replies() {
        let mut r =
            RetSubmit { basic: UrbBasic { command: USBIP_RET_SUBMIT, seqnum: 7, devid: 0, direction: 0, ep: 0 }, status: 0, actual_length: 512, start_frame: 0, number_of_packets: -1, error_count: 0 }
                .encode();
        assert_eq!(csw_mark_ep(&r), None, "an ordinary reply carries no marker");
        r[40..44].copy_from_slice(&csw_mark(0x81));
        assert_eq!(csw_mark_ep(&r), Some(1), "the endpoint number loses the direction bit");
        assert_eq!(csw_mark_ep(&r[..40]), None, "a truncated header never matches");
    }

    #[test]
    fn session_flags_are_distinct_bits() {
        let all = [FLAG_READ_ONLY, FLAG_ACCEL, FLAG_NTFS_JOURNAL, FLAG_RESUME];
        let mut seen = 0u32;
        for f in all {
            assert_eq!(f.count_ones(), 1, "each flag is a single bit");
            assert_eq!(seen & f, 0, "no two flags share a bit");
            seen |= f;
        }
        let m = OpHeader::decode(&encode_flags_request(FLAG_READ_ONLY | FLAG_RESUME)[..8]).unwrap();
        assert_eq!(m.code, OP_REQ_FLAGS);
    }

    #[test]
    fn sizes_and_counts_no_honest_peer_sends_are_rejected_before_allocation() {
        let mut c = CmdSubmit {
            basic: UrbBasic { command: USBIP_CMD_SUBMIT, seqnum: 1, devid: 0, direction: DIR_OUT, ep: 1 },
            transfer_flags: 0,
            transfer_buffer_length: 4096,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        };
        assert!(c.validate().is_ok());
        c.transfer_buffer_length = MAX_URB_BYTES;
        assert!(c.validate().is_ok(), "the largest allowed transfer");
        for bad in [-1, MAX_URB_BYTES + 1, i32::MAX] {
            c.transfer_buffer_length = bad;
            assert!(c.validate().is_err(), "length {bad}");
        }
        c.transfer_buffer_length = 0;
        for bad in [-1, MAX_ISO_PACKETS + 1, i32::MAX] {
            c.number_of_packets = bad;
            assert!(c.validate().is_err(), "packets {bad}");
        }
        c.number_of_packets = 0;
        c.basic.ep = 16;
        assert!(c.validate().is_err(), "no such endpoint number");
        c.basic.ep = 0x100;
        assert!(c.validate().is_err(), "the endpoint number must not be truncated to 0");
    }

    #[test]
    fn a_reply_may_not_announce_more_than_a_transfer_can_hold() {
        let r = |len, np| RetSubmit {
            basic: UrbBasic { command: USBIP_RET_SUBMIT, seqnum: 1, devid: 0, direction: 0, ep: 0 },
            status: 0,
            actual_length: len,
            start_frame: 0,
            number_of_packets: np,
            error_count: 0,
        };
        assert!(r(MAX_URB_BYTES, 0).validate().is_ok());
        assert!(r(MAX_URB_BYTES + 1, 0).validate().is_err());
        assert!(r(-5, 0).validate().is_err());
        assert!(r(0, MAX_ISO_PACKETS + 1).validate().is_err());
    }
}
