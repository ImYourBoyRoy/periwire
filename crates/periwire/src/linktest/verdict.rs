//! Saying in plain words whether a measured link is good enough for a camera, a microphone, a drive.
//!
//! The thresholds are rules of thumb, not guarantees: the operating system's player buffers some data (tens of milliseconds),
//! so a link that delivers its frames within that window sounds and looks fine, and one that does not crackles or drops frames.
//! The raw numbers are always reported next to the verdict so the judgement can be checked.
use super::stats::{Lateness, Rtt};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Poor,
    Marginal,
    Good,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Audio,
    Video,
    Other,
}

/// What a device will ask of the link: the steady data rate each way, and whether timing or bandwidth matters most.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Need {
    pub kind: Kind,
    /// Steady rate the device may use coming to this computer and going from it, in kbit/s.
    pub in_kbps: u32,
    pub out_kbps: u32,
}

/// Round trip (ms, 95th percentile) a stream can ride out. A USB video driver keeps about five transfers of 4 ms in flight (20 ms
/// of video); each replacement transfer needs a whole round trip to be requested, so a link slower than that cannot keep the
/// device's pipe fed, and the data it could not receive is lost. Measured with a webcam on a shaped link: smooth at 10 ms, gone
/// at 30. Audio drivers queue a little less. (The exporter reads ahead, so the device itself is never starved; this is the importer's side.)
const VIDEO_RTT_GOOD_MS: f64 = 8.0;
const VIDEO_RTT_OK_MS: f64 = 16.0;
const AUDIO_RTT_GOOD_MS: f64 = 6.0;
const AUDIO_RTT_OK_MS: f64 = 12.0;
/// A test stream that fell this far behind (the exporter's read-ahead holds a quarter of a second) did not fit through the link.
const CAPACITY_LATE_MS: f64 = 250.0;
/// Spare bandwidth over what the device uses.
const HEADROOM_GOOD: f64 = 2.0;
const HEADROOM_OK: f64 = 1.25;
/// With no particular device in mind: a typical webcam stream, in kbit/s.
const TYPICAL_VIDEO_KBPS: f64 = 30_000.0;

/// The measurements a verdict is made from (0 for a throughput that was not measured).
#[derive(Debug, Clone)]
pub struct Measured<'a> {
    pub rtt: &'a Rtt,
    pub stream: Option<&'a Lateness>,
    pub down_mbps: f64,
    pub up_mbps: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Verdict {
    pub level: Level,
    pub audio: Level,
    pub video: Level,
    pub headline: String,
    pub notes: Vec<String>,
}

fn rtt_level(ms: f64, good: f64, ok: f64) -> Level {
    if ms <= good {
        Level::Good
    } else if ms <= ok {
        Level::Marginal
    } else {
        Level::Poor
    }
}

fn headroom_level(capacity_kbps: f64, need_kbps: f64) -> Level {
    if need_kbps <= 0.0 {
        return Level::Good;
    }
    let ratio = capacity_kbps / need_kbps;
    if ratio >= HEADROOM_GOOD {
        Level::Good
    } else if ratio >= HEADROOM_OK {
        Level::Marginal
    } else {
        Level::Poor
    }
}

fn stall_cap(stalls: usize, limit_poor: usize) -> Level {
    match stalls {
        0 => Level::Good,
        n if n < limit_poor => Level::Marginal,
        _ => Level::Poor,
    }
}

/// Did the test stream fall further behind than the exporter's read-ahead can absorb? Then the link could not carry that rate.
fn capacity_level(m: &Measured) -> Level {
    match m.stream {
        Some(s) if s.received > 0 && s.p99_ms > CAPACITY_LATE_MS => Level::Poor,
        Some(s) if s.received < s.expected => Level::Poor,
        _ => Level::Good,
    }
}

pub fn judge(m: &Measured, need: Option<&Need>) -> Verdict {
    let rtt = m.rtt.p95_ms;
    let capacity = capacity_level(m);
    let audio = rtt_level(rtt, AUDIO_RTT_GOOD_MS, AUDIO_RTT_OK_MS).min(stall_cap(m.rtt.stalls, 3)).min(capacity);
    let video_timing = rtt_level(rtt, VIDEO_RTT_GOOD_MS, VIDEO_RTT_OK_MS).min(stall_cap(m.rtt.stalls, 6)).min(capacity);
    let (cap_in, cap_out) = (m.down_mbps * 1000.0, m.up_mbps * 1000.0);
    let video = if m.down_mbps > 0.0 { video_timing.min(headroom_level(cap_in, TYPICAL_VIDEO_KBPS)) } else { video_timing };
    let mut notes = vec![];
    if capacity == Level::Poor {
        let s = m.stream.expect("capacity is only poor when a stream was measured");
        if s.received < s.expected {
            notes.push("Some of the test stream never arrived, so the link dropped data.".to_string());
        } else {
            notes.push(format!("The link could not carry the test stream without falling behind: it ended up {:.0} ms late and moved {:.1} Mbit/s.", s.p99_ms, f64::from(s.delivered_kbps) / 1000.0));
        }
    }
    // Say which limit matters for what this is for (a drive does not care how steady the link is).
    let timing_limit = match need.map(|n| n.kind) {
        None | Some(Kind::Audio) => Some((AUDIO_RTT_GOOD_MS, "Audio")),
        Some(Kind::Video) => Some((VIDEO_RTT_GOOD_MS, "Video")),
        Some(Kind::Other) => None,
    };
    if let Some((limit, what)) = timing_limit.filter(|(limit, _)| rtt > *limit) {
        notes.push(format!("A round trip takes up to {rtt:.0} ms (95th percentile). {what} stays smooth when that is under {limit:.0} ms: the computer using it only has a few milliseconds of data queued, and each replacement needs a full round trip."));
    }
    if m.rtt.stalls > 0 {
        notes.push(format!("The link paused {} time(s) during the test (worst round trip {:.0} ms): each pause is a glitch in sound or video.", m.rtt.stalls, m.rtt.max_ms));
    }
    let level = match need {
        None => {
            if video == Level::Poor && audio == Level::Poor {
                Level::Poor
            } else {
                audio.max(video)
            }
        }
        Some(n) => {
            let timing = match n.kind {
                Kind::Audio => audio,
                Kind::Video => video_timing,
                Kind::Other => Level::Good,
            };
            let mut bw = Level::Good;
            // A camera's highest setting asks for far more than most people use; judge against a typical stream and say so for the rest.
            let in_need = if n.kind == Kind::Video { n.in_kbps.min(TYPICAL_VIDEO_KBPS as u32) } else { n.in_kbps };
            if m.down_mbps > 0.0 && in_need > 0 {
                bw = bw.min(headroom_level(cap_in, f64::from(in_need)));
            }
            if m.up_mbps > 0.0 && n.out_kbps > 0 {
                bw = bw.min(headroom_level(cap_out, f64::from(n.out_kbps)));
            }
            if bw < Level::Good || (n.kind == Kind::Video && m.down_mbps > 0.0 && f64::from(n.in_kbps) > cap_in) {
                notes.push(format!(
                    "At its highest setting the device can use up to {} Mbit/s coming here and {} Mbit/s going out; the link carried {:.0} down and {:.0} up{}.",
                    n.in_kbps / 1000,
                    n.out_kbps / 1000,
                    m.down_mbps,
                    m.up_mbps,
                    if n.kind == Kind::Video { " (a lower resolution or frame rate needs less)" } else { "" }
                ));
            }
            timing.min(bw)
        }
    };
    if level < Level::Good {
        notes.push("A wired connection, or 5 GHz Wi-Fi close to the router, usually fixes this.".to_string());
    }
    let headline = headline(level, audio, video, m, need);
    Verdict { level, audio, video, headline, notes }
}

fn word(l: Level) -> &'static str {
    match l {
        Level::Good => "good",
        Level::Marginal => "marginal",
        Level::Poor => "poor",
    }
}

fn headline(level: Level, audio: Level, video: Level, m: &Measured, need: Option<&Need>) -> String {
    match need {
        Some(n) => {
            let what = match n.kind {
                Kind::Audio => "audio",
                Kind::Video => "video",
                Kind::Other => "this device",
            };
            match level {
                Level::Good => format!("This connection is good for {what}."),
                Level::Marginal => format!("This connection is marginal for {what}: expect occasional glitches."),
                Level::Poor => format!("This connection is too weak for {what}: expect dropouts."),
            }
        }
        None => {
            let room = if m.down_mbps > 0.0 { format!("; the link carries about {:.0} Mbit/s", m.down_mbps) } else { String::new() };
            format!("Audio: {}. Video: {}{}.", word(audio), word(video), room)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtt(min: f64, p95: f64, jitter: f64, stalls: usize) -> Rtt {
        Rtt { samples: 50, min_ms: min, avg_ms: min, p50_ms: min, p95_ms: p95, max_ms: p95, jitter_ms: jitter, stalls }
    }

    fn lat(p99: f64) -> Lateness {
        Lateness { expected: 100, received: 100, p50_ms: 1.0, p95_ms: p99 / 2.0, p99_ms: p99, max_ms: p99, delivered_kbps: 8000 }
    }

    #[test]
    fn a_wired_link_is_good_for_everything() {
        let (r, l) = (rtt(0.3, 0.8, 0.1, 0), lat(2.0));
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 900.0, up_mbps: 900.0 }, None);
        assert_eq!((v.level, v.audio, v.video), (Level::Good, Level::Good, Level::Good));
        assert!(v.notes.is_empty(), "{:?}", v.notes);
    }

    #[test]
    fn a_wifi_link_is_fine_for_video_but_not_for_the_tighter_audio_queue() {
        // 7 ms round trips: inside the video budget (8), outside audio's (6).
        let (r, l) = (rtt(3.0, 7.0, 2.0, 0), lat(12.0));
        let m = Measured { rtt: &r, stream: Some(&l), down_mbps: 150.0, up_mbps: 150.0 };
        let v = judge(&m, None);
        assert_eq!((v.audio, v.video), (Level::Marginal, Level::Good));
        let audio = judge(&m, Some(&Need { kind: Kind::Audio, in_kbps: 1500, out_kbps: 1500 }));
        assert_eq!(audio.level, Level::Marginal);
        assert!(audio.headline.contains("marginal for audio"));
        assert!(audio.notes.iter().any(|n| n.contains("Audio stays smooth when that is under 6 ms")), "{:?}", audio.notes);
    }

    #[test]
    fn a_30_ms_round_trip_is_poor_for_video_as_measured_with_a_real_webcam() {
        let (r, l) = (rtt(30.0, 31.0, 0.1, 0), lat(5.0));
        let cam = Need { kind: Kind::Video, in_kbps: 20_000, out_kbps: 0 };
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 500.0, up_mbps: 500.0 }, Some(&cam));
        assert_eq!(v.level, Level::Poor);
        assert!(v.headline.contains("too weak for video"));
        assert!(v.notes.iter().any(|n| n.contains("Video stays smooth when that is under 8 ms")), "{:?}", v.notes);
    }

    #[test]
    fn a_link_slower_than_the_camera_is_poor_whatever_its_timing() {
        let (r, l) = (rtt(2.0, 3.0, 0.3, 0), lat(4.0));
        let cam = Need { kind: Kind::Video, in_kbps: 48_000, out_kbps: 0 };
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 30.0, up_mbps: 30.0 }, Some(&cam));
        assert_eq!(v.level, Level::Poor);
        assert!(v.notes.iter().any(|n| n.contains("can use up to")));
        let ok = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 120.0, up_mbps: 120.0 }, Some(&cam));
        assert_eq!(ok.level, Level::Good);
    }

    #[test]
    fn a_stream_that_fell_behind_further_than_the_read_ahead_holds_is_poor() {
        let (r, l) = (rtt(2.0, 3.0, 0.3, 0), lat(900.0));
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 0.0, up_mbps: 0.0 }, None);
        assert_eq!((v.audio, v.video), (Level::Poor, Level::Poor));
        assert!(v.notes.iter().any(|n| n.contains("could not carry the test stream")), "{:?}", v.notes);
    }

    #[test]
    fn repeated_pauses_ruin_audio_and_are_named() {
        let (r, l) = (rtt(2.0, 4.0, 0.5, 4), lat(8.0));
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 200.0, up_mbps: 200.0 }, None);
        assert_eq!(v.audio, Level::Poor);
        assert!(v.notes.iter().any(|n| n.contains("paused 4 time")));
    }

    #[test]
    fn a_stream_that_lost_data_says_so() {
        let (r, mut l) = (rtt(2.0, 3.0, 0.3, 0), lat(4.0));
        l.received = 90;
        let v = judge(&Measured { rtt: &r, stream: Some(&l), down_mbps: 0.0, up_mbps: 0.0 }, None);
        assert!(v.notes.iter().any(|n| n.contains("never arrived")));
    }

    #[test]
    fn a_drive_does_not_need_a_quick_link() {
        let (r, l) = (rtt(30.0, 60.0, 9.0, 0), lat(70.0));
        let m = Measured { rtt: &r, stream: Some(&l), down_mbps: 300.0, up_mbps: 300.0 };
        let disk = judge(&m, Some(&Need { kind: Kind::Other, in_kbps: 0, out_kbps: 0 }));
        assert_eq!(disk.level, Level::Good);
        assert!(!disk.notes.iter().any(|n| n.contains("round trip takes")), "{:?}", disk.notes);
    }
}
