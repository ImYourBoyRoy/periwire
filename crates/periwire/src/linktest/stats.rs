//! Turning raw timings into the numbers people read: latency, jitter, and how late a steady stream's frames arrive.
use serde::Serialize;

/// The value `p` (0..=100) of the way up an ascending list; 0 for an empty one.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn sorted(v: &[f64]) -> Vec<f64> {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Round-trip time of small messages sent at a steady pace.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Rtt {
    pub samples: usize,
    pub min_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Mean change between one round trip and the next (the usual definition of jitter).
    pub jitter_ms: f64,
    /// Round trips far slower than the typical one: the link paused (a retransmit, a Wi-Fi roam, a busy radio).
    pub stalls: usize,
}

pub fn rtt_stats(ms: &[f64]) -> Rtt {
    if ms.is_empty() {
        return Rtt::default();
    }
    let s = sorted(ms);
    let p50 = percentile(&s, 50.0);
    let jitter = if ms.len() > 1 { ms.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (ms.len() - 1) as f64 } else { 0.0 };
    let stall_at = (p50 * 3.0).max(p50 + 40.0);
    Rtt {
        samples: ms.len(),
        min_ms: round1(s[0]),
        avg_ms: round1(ms.iter().sum::<f64>() / ms.len() as f64),
        p50_ms: round1(p50),
        p95_ms: round1(percentile(&s, 95.0)),
        max_ms: round1(*s.last().unwrap()),
        jitter_ms: round1(jitter),
        stalls: ms.iter().filter(|x| **x > stall_at).count(),
    }
}

/// How late the frames of a fixed-rate stream arrived compared with the best-case frame: what a player's buffer must absorb.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Lateness {
    pub expected: u32,
    pub received: u32,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Data actually delivered per second over the stream, in kbit/s.
    pub delivered_kbps: u32,
}

/// `frames` are (sequence number, arrival time in ms since the stream began); frame `k` was due at `k * interval_ms` plus a constant delay.
pub fn lateness_stats(frames: &[(u32, f64)], expected: u32, interval_ms: f64, frame_bytes: usize) -> Lateness {
    if frames.is_empty() {
        return Lateness { expected, ..Default::default() };
    }
    let delays: Vec<f64> = frames.iter().map(|(k, at)| at - f64::from(*k) * interval_ms).collect();
    let base = delays.iter().copied().fold(f64::INFINITY, f64::min);
    let late = sorted(&delays.iter().map(|d| d - base).collect::<Vec<_>>());
    // Rate between the first and the last frame to arrive (the first one only starts the clock).
    let (first, last) = (frames.iter().map(|f| f.1).fold(f64::INFINITY, f64::min), frames.iter().map(|f| f.1).fold(0.0, f64::max));
    let span = (last - first).max(1.0);
    Lateness {
        expected,
        received: frames.len() as u32,
        p50_ms: round1(percentile(&late, 50.0)),
        p95_ms: round1(percentile(&late, 95.0)),
        p99_ms: round1(percentile(&late, 99.0)),
        max_ms: round1(*late.last().unwrap()),
        delivered_kbps: (frames.len().saturating_sub(1) as f64 * frame_bytes as f64 * 8.0 / span) as u32,
    }
}

/// Megabits per second for `bytes` moved in `micros`.
pub fn mbps(bytes: u64, micros: u64) -> f64 {
    if micros == 0 {
        return 0.0;
    }
    round1(bytes as f64 * 8.0 / micros as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_jitter_of_a_steady_link_are_small() {
        let rtts: Vec<f64> = (0..50).map(|i| 4.0 + (i % 3) as f64 * 0.2).collect();
        let r = rtt_stats(&rtts);
        assert_eq!(r.samples, 50);
        assert_eq!(r.min_ms, 4.0);
        assert!(r.jitter_ms < 0.5 && r.stalls == 0, "{r:?}");
        assert_eq!(rtt_stats(&[]), Rtt::default());
    }

    #[test]
    fn a_pause_is_counted_as_a_stall_and_shows_in_the_tail() {
        let mut rtts = vec![5.0; 40];
        rtts[10] = 400.0;
        let r = rtt_stats(&rtts);
        assert_eq!(r.stalls, 1);
        assert_eq!(r.max_ms, 400.0);
        assert_eq!(r.p50_ms, 5.0, "one pause does not move the typical value");
    }

    #[test]
    fn lateness_is_measured_against_the_best_frame_not_against_the_clock() {
        // 4 ms frames arriving 30 ms after they were due, steadily: nothing is late relative to the others.
        let steady: Vec<(u32, f64)> = (0..100).map(|k| (k, f64::from(k) * 4.0 + 30.0)).collect();
        let l = lateness_stats(&steady, 100, 4.0, 4000);
        assert_eq!((l.received, l.max_ms), (100, 0.0));
        assert!((7900..=8100).contains(&l.delivered_kbps), "8 Mbit/s of 4000-byte frames every 4 ms: {}", l.delivered_kbps);
        // A link that cannot keep up falls further behind with every frame.
        let slow: Vec<(u32, f64)> = (0..100).map(|k| (k, f64::from(k) * 8.0)).collect();
        let l = lateness_stats(&slow, 100, 4.0, 4000);
        assert!(l.max_ms > 390.0 && l.p50_ms > 150.0, "{l:?}");
    }

    #[test]
    fn throughput_in_megabits() {
        assert_eq!(mbps(1_250_000, 1_000_000), 10.0);
        assert_eq!(mbps(5, 0), 0.0);
    }
}
