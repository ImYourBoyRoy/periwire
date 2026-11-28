//! The connection check (PeriWire extension, not part of USB/IP): one computer asks another to take part in a short, timed
//! exchange so it can tell whether the link between them is good enough for a given job (a camera, a microphone, a drive).
//!
//! After the TLS handshake the asking side sends `OpHeader(OP_REQ_PROBE)` and a [`ProbeRequest`]; the answering side replies
//! `OpHeader(OP_REP_PROBE, status)` (0 = going ahead, 1 = refused) and the exchange runs in up to three phases, each skipped
//! when its length is 0:
//!
//! 1. **Ping**: `pings` messages of [`PING_LEN`] bytes, one every `ping_interval_ms`, each echoed back at once.
//! 2. **Stream**: the answerer sends frames on a fixed schedule, one every `stream_interval_us` of `stream_kbps` worth of data,
//!    for `stream_ms`: the same traffic pattern as a camera or microphone. Frames start with [`STREAM_HEAD_LEN`] bytes (sequence
//!    number, payload length); the asker records when each arrives.
//! 3. **Bulk**: the answerer sends as fast as the link allows for `bulk_ms`, then the asker does the same the other way and the
//!    answerer reports what it received. Both directions are chunks of `u32 length` + data, ended by a chunk of length 0.
//!
//! Every number here comes from the other computer, so each is checked against a limit before anything is allocated or waited for.
use crate::ProtoError;

pub const OP_REQ_PROBE: u16 = 0x8f03;
pub const OP_REP_PROBE: u16 = 0x0f03;

/// Answer `OpHeader::status` when the check is refused (too many already running, or the request is outside the limits).
pub const PROBE_REFUSED: u32 = 1;

pub const PING_LEN: usize = 16;
pub const STREAM_HEAD_LEN: usize = 8;
pub const BULK_CHUNK: usize = 16 * 1024;

pub const PROBE_MAX_PINGS: u32 = 1000;
pub const PROBE_MAX_PHASE_MS: u32 = 10_000;
pub const PROBE_MIN_STREAM_INTERVAL_US: u32 = 250;
/// 2 Gbit/s: above any realistic USB camera and still a sane bound on the frame size we would allocate.
pub const PROBE_MAX_STREAM_KBPS: u32 = 2_000_000;
pub const PROBE_MAX_FRAME_BYTES: usize = 256 * 1024;
/// The whole check, all phases, never takes longer than this.
pub const PROBE_MAX_TOTAL_MS: u64 = 25_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeRequest {
    pub pings: u32,
    pub ping_interval_ms: u32,
    pub stream_kbps: u32,
    pub stream_interval_us: u32,
    pub stream_ms: u32,
    pub bulk_ms: u32,
}

pub const PROBE_REQUEST_LEN: usize = 24;

impl ProbeRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = crate::OpHeader::new(OP_REQ_PROBE, 0).encode().to_vec();
        for v in [self.pings, self.ping_interval_ms, self.stream_kbps, self.stream_interval_us, self.stream_ms, self.bulk_ms] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self, ProtoError> {
        if b.len() < PROBE_REQUEST_LEN {
            return Err(ProtoError::Short { need: PROBE_REQUEST_LEN, have: b.len() });
        }
        let u = |i: usize| u32::from_be_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        Ok(Self { pings: u(0), ping_interval_ms: u(1), stream_kbps: u(2), stream_interval_us: u(3), stream_ms: u(4), bulk_ms: u(5) })
    }

    /// The size in bytes of one stream frame, or 0 when there is no stream phase.
    pub fn frame_bytes(&self) -> usize {
        if self.stream_ms == 0 || self.stream_kbps == 0 {
            return 0;
        }
        let bytes = u64::from(self.stream_kbps) * 1000 / 8 * u64::from(self.stream_interval_us) / 1_000_000;
        (bytes as usize).max(STREAM_HEAD_LEN)
    }

    /// How many frames the stream phase sends (0 when there is none).
    pub fn stream_frames(&self) -> u32 {
        if self.frame_bytes() == 0 || self.stream_interval_us == 0 {
            return 0;
        }
        (u64::from(self.stream_ms) * 1000 / u64::from(self.stream_interval_us)).min(u64::from(u32::MAX)) as u32
    }

    /// Reject a request no honest peer sends, before the answerer allocates or sleeps for it.
    pub fn validate(&self) -> Result<(), ProtoError> {
        let chk = |what: &'static str, v: u32, max: u32| {
            if v <= max {
                Ok(())
            } else {
                Err(ProtoError::OutOfRange { what, value: v.into(), max: max.into() })
            }
        };
        chk("probe pings", self.pings, PROBE_MAX_PINGS)?;
        chk("probe ping interval", self.ping_interval_ms, PROBE_MAX_PHASE_MS)?;
        chk("probe stream rate", self.stream_kbps, PROBE_MAX_STREAM_KBPS)?;
        chk("probe stream time", self.stream_ms, PROBE_MAX_PHASE_MS)?;
        chk("probe bulk time", self.bulk_ms, PROBE_MAX_PHASE_MS)?;
        if self.stream_ms > 0 && self.stream_kbps > 0 {
            if self.stream_interval_us < PROBE_MIN_STREAM_INTERVAL_US {
                return Err(ProtoError::OutOfRange { what: "probe stream interval", value: self.stream_interval_us.into(), max: i64::from(PROBE_MIN_STREAM_INTERVAL_US) });
            }
            if self.frame_bytes() > PROBE_MAX_FRAME_BYTES {
                return Err(ProtoError::OutOfRange { what: "probe stream frame", value: self.frame_bytes() as i64, max: PROBE_MAX_FRAME_BYTES as i64 });
            }
        }
        let pings_ms = u64::from(self.pings) * u64::from(self.ping_interval_ms);
        if pings_ms > PROBE_MAX_TOTAL_MS {
            return Err(ProtoError::OutOfRange { what: "probe ping time", value: pings_ms as i64, max: PROBE_MAX_TOTAL_MS as i64 });
        }
        let total = pings_ms + u64::from(self.stream_ms) + 2 * u64::from(self.bulk_ms);
        if total > PROBE_MAX_TOTAL_MS {
            return Err(ProtoError::OutOfRange { what: "probe total time", value: total as i64, max: PROBE_MAX_TOTAL_MS as i64 });
        }
        Ok(())
    }
}

/// A stream frame's header: sequence number and payload length that follows.
pub fn encode_stream_head(seq: u32, payload: usize) -> [u8; STREAM_HEAD_LEN] {
    let mut h = [0u8; STREAM_HEAD_LEN];
    h[..4].copy_from_slice(&seq.to_be_bytes());
    h[4..].copy_from_slice(&(payload as u32).to_be_bytes());
    h
}

pub fn decode_stream_head(h: &[u8; STREAM_HEAD_LEN]) -> (u32, usize) {
    (u32::from_be_bytes([h[0], h[1], h[2], h[3]]), u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProbeRequest {
        ProbeRequest { pings: 30, ping_interval_ms: 20, stream_kbps: 8000, stream_interval_us: 4000, stream_ms: 1500, bulk_ms: 1000 }
    }

    #[test]
    fn a_request_roundtrips_and_a_reasonable_one_is_accepted() {
        let r = sample();
        let wire = r.encode();
        assert_eq!(wire.len(), 8 + PROBE_REQUEST_LEN);
        assert_eq!(ProbeRequest::decode(&wire[8..]).unwrap(), r);
        assert!(r.validate().is_ok());
        assert_eq!(r.frame_bytes(), 4000, "8 Mbit/s every 4 ms is 4000 bytes");
        assert_eq!(r.stream_frames(), 375, "1.5 s of 4 ms frames");
        assert_eq!(ProbeRequest { stream_ms: 0, ..r }.stream_frames(), 0);
    }

    #[test]
    fn absurd_requests_are_refused_before_anything_is_allocated() {
        let base = sample();
        for bad in [
            ProbeRequest { pings: PROBE_MAX_PINGS + 1, ..base },
            ProbeRequest { stream_kbps: u32::MAX, ..base },
            ProbeRequest { stream_ms: PROBE_MAX_PHASE_MS + 1, ..base },
            ProbeRequest { stream_interval_us: 1, ..base },
            ProbeRequest { stream_kbps: PROBE_MAX_STREAM_KBPS, stream_interval_us: 1_000_000, ..base },
            ProbeRequest { pings: 1000, ping_interval_ms: 1000, ..base },
            ProbeRequest { bulk_ms: PROBE_MAX_PHASE_MS, stream_ms: PROBE_MAX_PHASE_MS, pings: 100, ping_interval_ms: 100, ..base },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be refused");
        }
        assert!(ProbeRequest::decode(&[0u8; 5]).is_err());
    }

    #[test]
    fn a_stream_frame_header_roundtrips() {
        assert_eq!(decode_stream_head(&encode_stream_head(7, 4000)), (7, 4000));
    }
}
