//! Exporter: shares local USB devices over USB/IP (inside mutual TLS).
//! Devices are driven from userspace through libusb, so the same code runs on Linux, macOS and Windows.
use crate::guard::{Guard, Patch, Verdict};
use crate::{identity, tls};
use anyhow::{anyhow, bail, Result};
use rusb::{Context, Device, DeviceHandle, TransferType, UsbContext};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::watch;
use usbip_proto::*;

static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn trace_on() -> bool {
    *TRACE.get_or_init(|| std::env::var_os("PERIWIRE_TRACE").map(|v| !v.is_empty() && v != "0").unwrap_or(false))
}
macro_rules! trace {
    ($($a:tt)*) => { if trace_on() { eprintln!("[trace {:?}] {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() % 100_000_000, format!($($a)*)); } };
}

/// State shared between the exporter and the daemon/control API.
pub struct Shared {
    /// Busids or `vid:pid` that may be imported by trusted peers.
    pub specs: RwLock<Vec<String>>,
    /// Shares the owner write-protects for every importer (subset of `specs`).
    pub ro_specs: RwLock<Vec<String>>,
    pub sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// User-chosen names, keyed by `vid:pid[:serial]` (see `device_key`).
    pub labels: RwLock<std::collections::BTreeMap<String, String>>,
    cache: Mutex<HashMap<String, Strings>>,
    next: AtomicU64,
    /// Devices whose importer lost its connection, kept claimed for a grace period so it can resume (see `resume`).
    parked: Mutex<crate::resume::Parking<Dev>>,
}

/// Manufacturer, product and serial strings read from a device.
#[derive(Clone, Default)]
pub struct Strings {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
}

/// One peer currently using one of our devices.
pub struct Session {
    pub id: u64,
    pub peer: String,
    pub peer_name: String,
    pub busid: String,
    pub vid: u16,
    pub pid: u16,
    /// Human-readable device info (display name, kind, label) captured when the session opened.
    pub info: Value,
    pub since: u64,
    pub urbs: AtomicU64,
    /// The importer asked for write protection (enforced by `guard`).
    pub read_only: bool,
    /// Certificate fingerprint of the importer, and whether it asked to be waited for after a lost connection.
    fp: String,
    resume: bool,
    /// The session flags that must match for a parked device to be resumed.
    options: u32,
    /// Set when the same importer reconnected on a new connection: this one is dead, and the device should be parked, not released.
    takeover: AtomicBool,
    /// Set when the shared device itself vanished (unplugged or re-enumerated): the session is over and the importer should attach it again.
    gone: AtomicBool,
    /// Bytes received from the importer (OUT data) and sent to it (IN data).
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    kick: watch::Sender<bool>,
}

impl Session {
    pub fn view(&self) -> Value {
        json!({"id": self.id, "peer": self.peer, "peer_name": self.peer_name, "busid": self.busid,
               "vid": format!("{:04x}", self.vid), "pid": format!("{:04x}", self.pid), "info": self.info, "since": self.since, "read_only": self.read_only,
               "urbs": self.urbs.load(Ordering::Relaxed), "bytes_in": self.rx.load(Ordering::Relaxed), "bytes_out": self.tx.load(Ordering::Relaxed)})
    }
}

impl Shared {
    pub fn new(specs: Vec<String>, ro_specs: Vec<String>, labels: std::collections::BTreeMap<String, String>) -> Arc<Self> {
        Arc::new(Shared {
            specs: RwLock::new(specs),
            ro_specs: RwLock::new(ro_specs),
            sessions: Mutex::new(HashMap::new()),
            labels: RwLock::new(labels),
            cache: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            parked: Mutex::new(Default::default()),
        })
    }
    pub fn specs(&self) -> Vec<String> {
        self.specs.read().unwrap().clone()
    }
    /// Force-disconnect a session: the importer loses the device and it returns to this machine.
    pub fn kick(&self, id: u64) -> bool {
        match self.sessions.lock().unwrap().get(&id) {
            Some(s) => {
                let _ = s.kick.send(true);
                true
            }
            None => false,
        }
    }
    /// Kick every session on the device a spec (busid or vid:pid) names; the lock is taken once, so callers may not hold it.
    pub fn kick_matching(&self, spec: &str) -> usize {
        let mut n = 0;
        for s in self.sessions.lock().unwrap().values() {
            let dev = format!("{:04x}:{:04x}", s.vid, s.pid);
            if spec == s.busid || spec.eq_ignore_ascii_case(&dev) {
                let _ = s.kick.send(true);
                n += 1;
            }
        }
        n
    }
    pub fn kick_busid(&self, busid: &str) -> usize {
        let g = self.sessions.lock().unwrap();
        let mut n = 0;
        for s in g.values().filter(|s| s.busid == busid) {
            let _ = s.kick.send(true);
            n += 1;
        }
        drop(g);
        // A device waiting for its importer to come back is released too: the operator asked for it back.
        if let Some(d) = self.parked.lock().unwrap().remove(busid) {
            drop(d);
            n += 1;
        }
        n
    }
    /// Is this device being held for an importer that lost its connection?
    pub fn is_parked(&self, busid: &str) -> bool {
        self.parked.lock().unwrap().contains(busid)
    }
    pub fn session_for(&self, busid: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().values().find(|s| s.busid == busid).cloned()
    }
}

use crate::accel::{Seq, Step};
// Linux errno values carried in RET_SUBMIT status (negated).
const EPIPE: i32 = -32;
const ENODEV: i32 = -19;
const ETIMEDOUT: i32 = -110;
const EIO: i32 = -5;
const EPROTO: i32 = -71;
const ECONNRESET: i32 = -104;

mod dev;
mod devices;
pub(crate) mod hostprep;
mod import;
mod journal;
mod listener;
mod session;
mod transfer;
mod workers;

use dev::*;
pub use devices::*;
use import::*;
use journal::*;
pub use listener::*;
use session::*;
use transfer::*;
use workers::*;

pub(super) fn send(tx: &UnboundedSender<Vec<u8>>, v: Vec<u8>) {
    let _ = tx.send(v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn mount_points_with_spaces_are_restored_and_names_match_exactly() {
        let names = vec!["sdb".to_string(), "mapper/luks-1".to_string(), "dm-0".to_string(), "nvme1n1".to_string()];
        let table = "/dev/sdb1 /media/u/My\\040Drive vfat rw 0 0\n/dev/sdba1 /mnt/other ext4 rw 0 0\n/dev/sdb /mnt/whole ext4 rw 0 0\n\
                     /dev/mapper/luks-1 /mnt/secret ext4 rw 0 0\n/dev/nvme1n1p2 /mnt/nv ext4 rw 0 0\n/dev/nvme1n10 /mnt/x ext4 rw 0 0\ntmpfs /run tmpfs rw 0 0\n";
        let got: Vec<String> = matching_mounts(table, &names).into_iter().map(|(_, m)| m).collect();
        assert_eq!(got, ["/media/u/My Drive", "/mnt/whole", "/mnt/secret", "/mnt/nv"], "sdba1 is not a partition of sdb, nvme1n10 not of nvme1n1");
        assert_eq!(unescape_mount_field("a\\134b\\011c"), "a\\b\tc");
        assert_eq!(unescape_mount_field("trailing\\04"), "trailing\\04", "an incomplete escape stays as written");
    }

    #[test]
    fn only_no_device_means_the_device_vanished() {
        assert!(device_vanished(&rusb::Error::NoDevice));
        for e in [rusb::Error::NotFound, rusb::Error::Pipe, rusb::Error::Timeout, rusb::Error::Io, rusb::Error::Busy, rusb::Error::Other] {
            assert!(!device_vanished(&e), "{e:?} must not end a session");
        }
        assert_eq!(map_err(rusb::Error::NoDevice), ENODEV);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_of_one_endpoint_run_in_order_and_different_endpoints_side_by_side() {
        use std::sync::mpsc;
        let (done_tx, done_rx) = mpsc::channel::<(u8, u32)>();
        let done_tx = Mutex::new(done_tx);
        // The first job of each endpoint is the slowest: without ordering the later ones would finish first.
        let run: Arc<dyn Fn((u8, u32)) + Send + Sync> = Arc::new(move |(ep, n)| {
            std::thread::sleep(Duration::from_millis(if n == 0 { 250 } else { 150 })); // generous: the two endpoints must start within 150 ms of each other, however loaded the machine
            done_tx.lock().unwrap().send((ep, n)).unwrap();
        });
        let mut o: Ordered<u8, (u8, u32)> = Ordered::new(run);
        for n in 0..4 {
            assert!(o.send(1, (1, n)));
            assert!(o.send(2, (2, n)));
        }
        drop(o);
        let mut got = vec![];
        while let Ok(x) = done_rx.recv_timeout(Duration::from_secs(3)) {
            got.push(x);
            if got.len() == 8 {
                break;
            }
        }
        assert_eq!(got.len(), 8);
        for ep in [1u8, 2] {
            let seen: Vec<u32> = got.iter().filter(|x| x.0 == ep).map(|x| x.1).collect();
            assert_eq!(seen, vec![0, 1, 2, 3], "endpoint {ep} must run its jobs in the order given");
        }
        let first_two: Vec<u8> = got.iter().take(2).map(|x| x.0).collect();
        assert!(first_two.contains(&1) && first_two.contains(&2), "the two endpoints' slow first jobs overlap instead of queueing: {got:?}");
    }

    #[tokio::test]
    async fn a_peer_that_never_finishes_the_handshake_is_cut_off() {
        let ck = rcgen::generate_simple_self_signed(vec!["periwire".to_string()]).unwrap();
        let cert = ck.cert.der().to_vec();
        let id = identity::Identity { fingerprint: identity::fingerprint(&cert), cert, key: ck.key_pair.serialize_der() };
        let cfg = tls::server_config(&id, Default::default()).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
        // the other end connects and then says nothing at all
        let (server_end, _silent_client) = tokio::io::duplex(1024);
        let t = std::time::Instant::now();
        let r = accept_tls(&acceptor, server_end, Duration::from_millis(150)).await;
        assert!(r.is_err(), "a handshake that never starts must fail");
        assert!(t.elapsed() < Duration::from_secs(2), "and quickly: the limit is what stops a stalled peer holding a connection forever");
    }

    #[tokio::test]
    async fn a_closed_stream_is_a_clean_end_and_a_cut_header_is_a_loss() {
        // Closed before any byte: what a proper TLS close_notify looks like to the reader.
        let (mut rd, wr) = tokio::io::duplex(256);
        drop(wr);
        let mut h = [0u8; URB_HEADER_LEN];
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Closed));

        // Cut in the middle of a header: the importer vanished mid-message.
        let (mut rd, mut wr) = tokio::io::duplex(256);
        wr.write_all(&[1u8; 10]).await.unwrap();
        drop(wr);
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Lost(_)));

        // A whole header is simply read.
        let (mut rd, mut wr) = tokio::io::duplex(256);
        wr.write_all(&[7u8; URB_HEADER_LEN]).await.unwrap();
        assert!(matches!(read_header(&mut rd, &mut h).await, Hdr::Got));
        assert_eq!(h, [7u8; URB_HEADER_LEN]);
    }
}
