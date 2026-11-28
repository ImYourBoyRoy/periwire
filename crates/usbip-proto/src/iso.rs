//! Isochronous packet descriptors: the 16-byte records that follow the data of an isochronous `CMD_SUBMIT` / `RET_SUBMIT`.
//!
//! Layout (big-endian): offset, length, actual_length, status. A submit carries the length each packet may use and where it
//! sits in the transfer buffer; the reply fills in how much each packet really moved and its own status. For an IN transfer
//! the reply's data is *compacted* (only the `actual_length` bytes of each packet, one after the other); the receiver puts
//! each packet back at its `offset`.
use crate::ProtoError;

pub const ISO_DESC_LEN: usize = 16;

/// Largest single isochronous packet either side accepts (a SuperSpeed burst endpoint can reach about 48 KiB).
pub const MAX_ISO_PACKET_BYTES: u32 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IsoPacket {
    pub offset: u32,
    pub length: u32,
    pub actual_length: u32,
    pub status: i32,
}

impl IsoPacket {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_be_bytes());
        out.extend_from_slice(&self.length.to_be_bytes());
        out.extend_from_slice(&self.actual_length.to_be_bytes());
        out.extend_from_slice(&self.status.to_be_bytes());
    }

    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        if b.len() < ISO_DESC_LEN {
            return Err(ProtoError::Short { need: ISO_DESC_LEN, have: b.len() });
        }
        let u = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Ok(Self { offset: u(0), length: u(4), actual_length: u(8), status: u(12) as i32 })
    }
}

/// Decode `n` descriptors from `b` (which must hold exactly that many).
pub fn decode_iso(b: &[u8], n: usize) -> Result<Vec<IsoPacket>, ProtoError> {
    let need = n.checked_mul(ISO_DESC_LEN).ok_or(ProtoError::Short { need: usize::MAX, have: b.len() })?;
    if b.len() < need {
        return Err(ProtoError::Short { need, have: b.len() });
    }
    (0..n).map(|i| IsoPacket::decode(&b[i * ISO_DESC_LEN..])).collect()
}

pub fn encode_iso(packets: &[IsoPacket], out: &mut Vec<u8>) {
    out.reserve(packets.len() * ISO_DESC_LEN);
    for p in packets {
        p.encode(out);
    }
}

fn out_of_range(what: &'static str, value: u64, max: u64) -> ProtoError {
    ProtoError::OutOfRange { what, value: i64::try_from(value).unwrap_or(i64::MAX), max: i64::try_from(max).unwrap_or(i64::MAX) }
}

/// Every packet must lie inside a transfer buffer of `buffer_len` bytes, and none may be larger than any real endpoint allows.
/// A peer that sends offsets past the end would otherwise make us index outside the buffer.
pub fn validate_iso_layout(packets: &[IsoPacket], buffer_len: usize) -> Result<(), ProtoError> {
    for p in packets {
        if p.length > MAX_ISO_PACKET_BYTES {
            return Err(out_of_range("isochronous packet length", p.length.into(), MAX_ISO_PACKET_BYTES.into()));
        }
        let end = u64::from(p.offset) + u64::from(p.length);
        if end > buffer_len as u64 {
            return Err(out_of_range("isochronous packet end", end, buffer_len as u64));
        }
    }
    Ok(())
}

/// For a reply to an IN transfer: put the compacted `data` back at each packet's offset in a buffer of `buffer_len` bytes
/// (what an importer does before handing the transfer to its USB stack). Packets that moved fewer bytes leave zeros.
pub fn spread_in_data(packets: &[IsoPacket], data: &[u8], buffer_len: usize) -> Result<Vec<u8>, ProtoError> {
    validate_iso_layout(packets, buffer_len)?;
    let mut out = vec![0u8; buffer_len];
    let mut at = 0usize;
    for p in packets {
        let n = (p.actual_length.min(p.length)) as usize;
        let src = data.get(at..at + n).ok_or(ProtoError::Short { need: at + n, have: data.len() })?;
        out[p.offset as usize..p.offset as usize + n].copy_from_slice(src);
        at += n;
    }
    Ok(out)
}

/// The reverse, for the sender of an IN reply: take each packet's `actual_length` bytes from `buffer` (laid out by offset) one after another.
pub fn compact_in_data(packets: &[IsoPacket], buffer: &[u8]) -> Result<Vec<u8>, ProtoError> {
    validate_iso_layout(packets, buffer.len())?;
    let mut out = Vec::with_capacity(packets.iter().map(|p| p.actual_length.min(p.length) as usize).sum());
    for p in packets {
        let n = p.actual_length.min(p.length) as usize;
        out.extend_from_slice(&buffer[p.offset as usize..p.offset as usize + n]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(offset: u32, length: u32, actual: u32) -> IsoPacket {
        IsoPacket { offset, length, actual_length: actual, status: 0 }
    }

    #[test]
    fn descriptors_roundtrip_in_big_endian() {
        let p = IsoPacket { offset: 0x0102_0304, length: 5, actual_length: 6, status: -71 };
        let mut v = vec![];
        p.encode(&mut v);
        assert_eq!(v.len(), ISO_DESC_LEN);
        assert_eq!(&v[..4], &[1, 2, 3, 4]);
        assert_eq!(IsoPacket::decode(&v).unwrap(), p);
        assert_eq!(decode_iso(&v, 1).unwrap(), vec![p]);
        assert!(decode_iso(&v, 2).is_err(), "asking for more descriptors than the bytes hold is an error, not a panic");
    }

    #[test]
    fn a_packet_outside_the_buffer_or_too_large_is_refused() {
        assert!(validate_iso_layout(&[pk(0, 10, 0), pk(10, 10, 0)], 20).is_ok());
        assert!(validate_iso_layout(&[pk(10, 11, 0)], 20).is_err(), "ends past the buffer");
        assert!(validate_iso_layout(&[pk(u32::MAX, 10, 0)], 20).is_err(), "offset + length must not wrap around");
        assert!(validate_iso_layout(&[pk(0, MAX_ISO_PACKET_BYTES + 1, 0)], usize::MAX).is_err());
    }

    #[test]
    fn compacting_and_spreading_are_inverses_even_with_short_packets() {
        let packets = [pk(0, 4, 4), pk(4, 4, 2), pk(8, 4, 0), pk(12, 4, 3)];
        let buffer: Vec<u8> = (1..=16).collect();
        let packed = compact_in_data(&packets, &buffer).unwrap();
        assert_eq!(packed, vec![1, 2, 3, 4, 5, 6, 13, 14, 15]);
        let back = spread_in_data(&packets, &packed, 16).unwrap();
        assert_eq!(back, vec![1, 2, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 13, 14, 15, 0]);
        assert!(spread_in_data(&packets, &packed[..5], 16).is_err(), "a reply that is shorter than its descriptors say is refused");
    }
}
