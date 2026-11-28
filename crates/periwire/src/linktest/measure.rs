//! The two halves of the exchange described in `usbip_proto::probe`: the computer that answers, and the one that measures.
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use usbip_proto::*;

/// No single read may wait longer than this: a peer that goes quiet in the middle must not hold the exchange open.
const IDLE: Duration = Duration::from_secs(5);

async fn read_exact<R: AsyncRead + Unpin>(r: &mut R, buf: &mut [u8]) -> Result<()> {
    tokio::time::timeout(IDLE, r.read_exact(buf)).await.context("the other computer stopped answering")?.context("connection lost during the check")?;
    Ok(())
}

/// What the measuring side saw, before it is turned into the numbers people read.
#[derive(Debug, Default)]
pub struct Raw {
    pub rtts_ms: Vec<f64>,
    /// (sequence number, arrival in ms since the first frame's due time)
    pub frames: Vec<(u32, f64)>,
    pub down_bytes: u64,
    pub down_us: u64,
    pub up_bytes: u64,
    pub up_us: u64,
}

// ---- the computer that answers -------------------------------------------------------------------------------------------

/// Answer a validated request: echo pings, send the stream on its schedule, send bulk data, then receive the other direction's.
pub async fn answer<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(r: &mut R, w: &mut W, req: &ProbeRequest) -> Result<()> {
    let mut ping = [0u8; PING_LEN];
    for _ in 0..req.pings {
        read_exact(r, &mut ping).await?;
        w.write_all(&ping).await?;
        w.flush().await?;
    }
    let frames = req.stream_frames();
    if frames > 0 {
        let payload = vec![0u8; req.frame_bytes() - STREAM_HEAD_LEN];
        let interval = Duration::from_micros(u64::from(req.stream_interval_us));
        let start = Instant::now();
        for k in 0..frames {
            tokio::time::sleep_until((start + interval * k).into()).await;
            let mut m = encode_stream_head(k, payload.len()).to_vec();
            m.extend_from_slice(&payload);
            w.write_all(&m).await?;
            w.flush().await?;
        }
    }
    if req.bulk_ms > 0 {
        let chunk = vec![0u8; BULK_CHUNK];
        let end = Instant::now() + Duration::from_millis(u64::from(req.bulk_ms));
        while Instant::now() < end {
            w.write_all(&(BULK_CHUNK as u32).to_be_bytes()).await?;
            w.write_all(&chunk).await?;
        }
        w.write_all(&0u32.to_be_bytes()).await?;
        w.flush().await?;
        let (bytes, us) = receive_bulk(r).await?;
        w.write_all(&bytes.to_be_bytes()).await?;
        w.write_all(&us.to_be_bytes()).await?;
        w.flush().await?;
    }
    Ok(())
}

/// Read chunks until the empty one; returns the bytes received and the time from the first chunk to the end marker.
async fn receive_bulk<R: AsyncRead + Unpin>(r: &mut R) -> Result<(u64, u64)> {
    let (mut total, mut began) = (0u64, None::<Instant>);
    let mut buf = vec![0u8; BULK_CHUNK];
    loop {
        let mut len = [0u8; 4];
        read_exact(r, &mut len).await?;
        let n = u32::from_be_bytes(len) as usize;
        if n == 0 {
            break;
        }
        if n > BULK_CHUNK {
            bail!("the other computer sent a {n}-byte chunk; the limit is {BULK_CHUNK}");
        }
        began.get_or_insert_with(Instant::now);
        read_exact(r, &mut buf[..n]).await?;
        total += n as u64;
    }
    Ok((total, began.map(|b| b.elapsed().as_micros() as u64).unwrap_or(0)))
}

// ---- the computer that measures -------------------------------------------------------------------------------------------

/// Run the request against an answering peer on `s` (the request and its acceptance are already exchanged).
pub async fn measure<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, req: &ProbeRequest) -> Result<Raw> {
    let mut raw = Raw::default();
    ping_phase(s, req, &mut raw).await?;
    stream_phase(s, req, &mut raw).await?;
    if req.bulk_ms > 0 {
        let (bytes, us) = receive_bulk(s).await?;
        raw.down_bytes = bytes;
        raw.down_us = us;
        let chunk = vec![0u8; BULK_CHUNK];
        let end = Instant::now() + Duration::from_millis(u64::from(req.bulk_ms));
        while Instant::now() < end {
            s.write_all(&(BULK_CHUNK as u32).to_be_bytes()).await?;
            s.write_all(&chunk).await?;
        }
        s.write_all(&0u32.to_be_bytes()).await?;
        s.flush().await?;
        let mut rep = [0u8; 16];
        read_exact(s, &mut rep).await?;
        raw.up_bytes = u64::from_be_bytes(rep[..8].try_into().unwrap());
        raw.up_us = u64::from_be_bytes(rep[8..].try_into().unwrap());
    }
    Ok(raw)
}

async fn ping_phase<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, req: &ProbeRequest, raw: &mut Raw) -> Result<()> {
    let interval = Duration::from_millis(u64::from(req.ping_interval_ms));
    let start = Instant::now();
    for k in 0..req.pings {
        tokio::time::sleep_until((start + interval * k).into()).await;
        let mut ping = [0u8; PING_LEN];
        ping[..4].copy_from_slice(&k.to_be_bytes());
        let sent = Instant::now();
        s.write_all(&ping).await?;
        s.flush().await?;
        let mut echo = [0u8; PING_LEN];
        read_exact(s, &mut echo).await?;
        if echo != ping {
            bail!("the other computer answered a ping with something else");
        }
        raw.rtts_ms.push(sent.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(())
}

async fn stream_phase<S: AsyncRead + Unpin>(s: &mut S, req: &ProbeRequest, raw: &mut Raw) -> Result<()> {
    let frames = req.stream_frames();
    if frames == 0 {
        return Ok(());
    }
    let (want, start) = (req.frame_bytes() - STREAM_HEAD_LEN, Instant::now());
    let mut payload = vec![0u8; want];
    for _ in 0..frames {
        let mut head = [0u8; STREAM_HEAD_LEN];
        read_exact(s, &mut head).await?;
        let (seq, len) = decode_stream_head(&head);
        if len != want || seq >= frames {
            bail!("the other computer sent a stream frame that does not match the request");
        }
        read_exact(s, &mut payload).await?;
        raw.frames.push((seq, start.elapsed().as_secs_f64() * 1000.0));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ProbeRequest {
        ProbeRequest { pings: 5, ping_interval_ms: 2, stream_kbps: 4000, stream_interval_us: 4000, stream_ms: 100, bulk_ms: 60 }
    }

    #[tokio::test]
    async fn a_full_exchange_over_an_in_memory_link_measures_every_phase() {
        let (mut a, mut b) = tokio::io::duplex(256 * 1024);
        let r = req();
        let server = tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(&mut b);
            answer(&mut rd, &mut wr, &r).await
        });
        let raw = measure(&mut a, &r).await.unwrap();
        server.await.unwrap().unwrap();
        assert_eq!(raw.rtts_ms.len(), 5);
        assert_eq!(raw.frames.len(), r.stream_frames() as usize, "every frame the schedule promised arrived");
        assert!(raw.frames.windows(2).all(|w| w[0].0 < w[1].0), "in order");
        assert!(raw.down_bytes > 0 && raw.up_bytes > 0 && raw.up_us > 0);
    }

    #[tokio::test]
    async fn a_peer_that_stops_in_the_middle_ends_the_check_with_an_error_not_a_hang() {
        let (mut a, b) = tokio::io::duplex(1024);
        drop(b);
        assert!(measure(&mut a, &req()).await.is_err());
    }

    #[tokio::test]
    async fn an_oversized_bulk_chunk_is_refused() {
        let mut data: &[u8] = &[0, 1, 0, 1]; // announces 65537 bytes
        assert!(receive_bulk(&mut data).await.is_err());
    }
}
