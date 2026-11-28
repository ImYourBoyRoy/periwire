//! The connection check: measure whether the link to another computer is good enough for a camera, a microphone or a drive.
//!
//! `run` asks the other computer's PeriWire (over the same mutual-TLS connection everything else uses) to take part in a short
//! timed exchange (see `usbip_proto::probe`), and turns what it saw into numbers and a plain verdict. The answering side is
//! `serve`, called from the exporter for `OP_REQ_PROBE`.
mod device;
mod measure;
mod stats;
mod verdict;

pub use device::{add_to_attach, api_test, before_attach};
pub use verdict::{Kind, Need, Verdict};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use usbip_proto::*;

/// How many checks one computer answers at the same time. A check is deliberately a burst of traffic.
const MAX_CONCURRENT: usize = 2;

fn slots() -> &'static tokio::sync::Semaphore {
    static S: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    S.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT))
}

/// A short check (about 1.5 s): pings and a stream shaped like the device's own traffic, no bulk transfer. Cheap enough to run
/// before every attach of a camera or microphone.
pub fn quick(need: Option<&Need>) -> ProbeRequest {
    ProbeRequest { pings: 20, ping_interval_ms: 20, bulk_ms: 0, ..stream_for(need, 1000) }
}

/// The full check (about 6 s): the same, a longer stream, and a bulk transfer each way for the throughput.
pub fn full(need: Option<&Need>) -> ProbeRequest {
    ProbeRequest { pings: 50, ping_interval_ms: 20, bulk_ms: 1000, ..stream_for(need, 2500) }
}

/// A stream at the device's steady rate (or a typical webcam's) in frames every 4 ms.
fn stream_for(need: Option<&Need>, ms: u32) -> ProbeRequest {
    // A camera's highest setting is far above what is normally used: test at a typical stream, not at the ceiling.
    let ceiling = if need.map(|n| n.kind == Kind::Video).unwrap_or(false) { 40_000 } else { 400_000 };
    let kbps = need.map(|n| n.in_kbps.max(n.out_kbps)).filter(|k| *k > 0).unwrap_or(8_000).clamp(64, ceiling);
    ProbeRequest { stream_kbps: kbps, stream_interval_us: 4000, stream_ms: ms, ..Default::default() }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub addr: String,
    pub seconds: f64,
    pub rtt: stats::Rtt,
    pub stream: Option<StreamReport>,
    /// Throughput from the other computer to this one, and back, in Mbit/s; absent when no bulk transfer ran.
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
    pub verdict: Verdict,
}

#[derive(Debug, Serialize)]
pub struct StreamReport {
    pub rate_kbps: u32,
    pub interval_ms: f64,
    #[serde(flatten)]
    pub lateness: stats::Lateness,
}

impl Report {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Check the link to `addr`. `need` (from the device the check is for, if any) shapes the stream and the verdict.
pub async fn run(addr: &str, req: ProbeRequest, need: Option<&Need>) -> Result<Report> {
    req.validate().map_err(|e| anyhow::anyhow!("invalid check settings: {e}"))?;
    let began = std::time::Instant::now();
    let mut s = crate::import::connect(addr).await?;
    s.write_all(&req.encode()).await?;
    s.flush().await?;
    let mut h = [0u8; 8];
    s.read_exact(&mut h).await.map_err(|_| anyhow::anyhow!("{addr} does not support the connection check: update PeriWire there (the connection check came after 0.1.5)"))?;
    let rep = OpHeader::decode(&h)?;
    if rep.code != OP_REP_PROBE {
        bail!("{addr} does not support the connection check: update PeriWire there (the connection check came after 0.1.5)");
    }
    if rep.status != 0 {
        bail!("{addr} is busy with other connection checks; try again in a moment");
    }
    let limit = Duration::from_millis(PROBE_MAX_TOTAL_MS + 10_000);
    let raw = tokio::time::timeout(limit, measure::measure(&mut s, &req)).await.context("the connection check took too long")??;
    let _ = s.shutdown().await;
    Ok(report(addr, &req, raw, need, began.elapsed().as_secs_f64()))
}

fn report(addr: &str, req: &ProbeRequest, raw: measure::Raw, need: Option<&Need>, seconds: f64) -> Report {
    let rtt = stats::rtt_stats(&raw.rtts_ms);
    let interval_ms = f64::from(req.stream_interval_us) / 1000.0;
    let lateness = (req.stream_frames() > 0).then(|| stats::lateness_stats(&raw.frames, req.stream_frames(), interval_ms, req.frame_bytes()));
    let (down, up) = if req.bulk_ms > 0 { (Some(stats::mbps(raw.down_bytes, raw.down_us)), Some(stats::mbps(raw.up_bytes, raw.up_us))) } else { (None, None) };
    let verdict = verdict::judge(&verdict::Measured { rtt: &rtt, stream: lateness.as_ref(), down_mbps: down.unwrap_or(0.0), up_mbps: up.unwrap_or(0.0) }, need);
    let stream = lateness.map(|lateness| StreamReport { rate_kbps: req.stream_kbps, interval_ms, lateness });
    Report { addr: addr.to_string(), seconds: (seconds * 10.0).round() / 10.0, rtt, stream, down_mbps: down, up_mbps: up, verdict }
}

/// The answering side: the request body has not been read yet. Replies, then takes part in the exchange.
pub async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(r: &mut R, w: &mut W) -> Result<()> {
    let mut body = [0u8; PROBE_REQUEST_LEN];
    r.read_exact(&mut body).await?;
    let req = ProbeRequest::decode(&body)?;
    let permit = match (req.validate(), slots().try_acquire()) {
        (Ok(()), Ok(p)) => p,
        (e, _) => {
            w.write_all(&OpHeader::new(OP_REP_PROBE, PROBE_REFUSED).encode()).await?;
            w.flush().await?;
            if let Err(e) = e {
                bail!("refused a connection check: {e}");
            }
            return Ok(());
        }
    };
    w.write_all(&OpHeader::new(OP_REP_PROBE, 0).encode()).await?;
    w.flush().await?;
    let r = tokio::time::timeout(Duration::from_millis(PROBE_MAX_TOTAL_MS + 5_000), measure::answer(r, w, &req)).await;
    drop(permit);
    r.context("the connection check ran too long")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stock_checks_are_within_the_limits_a_peer_would_enforce() {
        let cam = Need { kind: Kind::Video, in_kbps: 48_000, out_kbps: 0 };
        for n in [None, Some(&cam)] {
            assert!(quick(n).validate().is_ok());
            assert!(full(n).validate().is_ok());
        }
        assert_eq!(quick(Some(&cam)).stream_kbps, 40_000, "a camera is tested at a typical stream, not at its ceiling");
        let mic = Need { kind: Kind::Audio, in_kbps: 1_536, out_kbps: 0 };
        assert_eq!(quick(Some(&mic)).stream_kbps, 1_536, "other devices are tested at their own rate");
        assert_eq!(full(None).stream_kbps, 8_000);
        assert!(full(Some(&Need { kind: Kind::Video, in_kbps: u32::MAX, out_kbps: 0 })).validate().is_ok(), "a silly device rate is clamped, not sent");
    }

    #[tokio::test]
    async fn the_answering_side_refuses_a_request_outside_the_limits() {
        let bad = ProbeRequest { pings: PROBE_MAX_PINGS + 1, ..Default::default() };
        let wire = bad.encode();
        let mut input: &[u8] = &wire[8..];
        let mut out = vec![];
        assert!(serve(&mut input, &mut out).await.is_err());
        assert_eq!(OpHeader::decode(&out).unwrap().status, PROBE_REFUSED, "and says so before closing");
    }

    #[tokio::test]
    async fn a_whole_check_through_serve_and_the_report() {
        let req = ProbeRequest { pings: 6, ping_interval_ms: 2, stream_kbps: 2000, stream_interval_us: 4000, stream_ms: 80, bulk_ms: 40 };
        let (mut a, mut b) = tokio::io::duplex(256 * 1024);
        let server = tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(&mut b);
            let mut op = [0u8; 8];
            rd.read_exact(&mut op).await.unwrap();
            assert_eq!(OpHeader::decode(&op).unwrap().code, OP_REQ_PROBE);
            serve(&mut rd, &mut wr).await
        });
        a.write_all(&req.encode()).await.unwrap();
        let mut h = [0u8; 8];
        a.read_exact(&mut h).await.unwrap();
        assert_eq!(OpHeader::decode(&h).unwrap().status, 0);
        let raw = measure::measure(&mut a, &req).await.unwrap();
        server.await.unwrap().unwrap();
        let rep = report("test:1", &req, raw, None, 0.3);
        assert_eq!(rep.rtt.samples, 6);
        assert_eq!(rep.stream.as_ref().unwrap().lateness.received, req.stream_frames());
        assert!(rep.down_mbps.unwrap() > 0.0 && rep.up_mbps.unwrap() > 0.0);
        let j = rep.to_json();
        assert!(j["verdict"]["headline"].is_string() && j["rtt"]["p50_ms"].is_number());
    }
}
