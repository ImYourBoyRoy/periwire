//! Managed attachments: remote devices imported on this machine, with automatic reconnect,
//! safe eject and live statistics.
use crate::events;
#[cfg(any(target_os = "linux", windows))]
use crate::import;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(any(target_os = "linux", windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Connecting,
    Attached,
    Reconnecting,
    Detaching,
    Ended,
}
impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Connecting => "connecting",
            State::Attached => "attached",
            State::Reconnecting => "reconnecting",
            State::Detaching => "detaching",
            State::Ended => "ended",
        }
    }
}

#[derive(Default, Clone)]
struct DevInfo {
    vid: u16,
    pid: u16,
    speed: u32,
}

struct Inner {
    state: State,
    error: Option<String>,
    dev: Option<DevInfo>,
    port: Option<u32>,
    attached_at: Option<u64>,
    retries: u32,
    /// What the exporter told us about the device (display name, kind, label); Null until known.
    info: Value,
}

pub struct Attachment {
    pub id: u64,
    pub addr: String,
    pub busid: String,
    pub auto: bool,
    /// Ask the exporter to write-protect the device (USB storage only).
    pub read_only: bool,
    pub created: u64,
    inner: Mutex<Inner>,
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    cancel: watch::Sender<bool>,
    finished: watch::Sender<bool>,
}

impl Attachment {
    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap().error.clone()
    }
    fn set(&self, st: State, err: Option<String>) {
        {
            let mut i = self.inner.lock().unwrap();
            i.state = st;
            i.error = err;
        }
        events::emit("state", json!({}));
    }
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn port(&self) -> Option<u32> {
        self.inner.lock().unwrap().port
    }
    pub fn local_busid(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            return self.port().and_then(crate::import::vhci::local_busid);
        }
        #[allow(unreachable_code)]
        None
    }
    pub fn view(&self) -> Value {
        let i = self.inner.lock().unwrap();
        let (vid, pid, speed) = i.dev.as_ref().map(|d| (format!("{:04x}", d.vid), format!("{:04x}", d.pid), crate::export::speed_name(d.speed))).unwrap_or_else(|| {
            let g = |k: &str| i.info.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            (g("vid"), g("pid"), crate::export::speed_name(0))
        });
        let port = i.port;
        let (state, error, since, retries, info) = (i.state.name(), i.error.clone(), i.attached_at, i.retries, i.info.clone());
        drop(i);
        let local = self.local_busid();
        let mounts: Vec<String> = local.as_deref().map(|b| crate::export::mounts_for(b).into_iter().map(|(_, m)| m).collect()).unwrap_or_default();
        json!({"id": self.id, "addr": self.addr, "busid": self.busid, "auto": self.auto, "read_only": self.read_only, "state": state, "error": error,
               "info": info, "vid": vid, "pid": pid, "speed": speed, "port": port, "local_busid": local, "mounts": mounts,
               "created": self.created, "attached_at": since, "retries": retries,
               "bytes_in": self.rx.load(Ordering::Relaxed), "bytes_out": self.tx.load(Ordering::Relaxed)})
    }
}

#[derive(Default)]
pub struct Registry {
    pub map: Mutex<HashMap<u64, Arc<Attachment>>>,
    next: AtomicU64,
}

impl Registry {
    pub fn list(&self) -> Vec<Arc<Attachment>> {
        let mut v: Vec<_> = self.map.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|a| a.id);
        v
    }
    pub fn find(&self, id: u64) -> Option<Arc<Attachment>> {
        self.map.lock().unwrap().get(&id).cloned()
    }
    pub fn find_target(&self, addr: &str, busid: &str) -> Option<Arc<Attachment>> {
        self.map.lock().unwrap().values().find(|a| a.addr == addr && a.busid == busid).cloned()
    }

    /// Start importing `busid` from `addr` in the background.
    pub fn start(self: &Arc<Self>, addr: &str, busid: &str, auto: bool, read_only: bool) -> Arc<Attachment> {
        let (cancel, _) = watch::channel(false);
        let (finished, _) = watch::channel(false);
        let att = Arc::new(Attachment {
            id: self.next.fetch_add(1, Ordering::SeqCst) + 1,
            addr: addr.into(),
            busid: busid.into(),
            auto,
            read_only,
            created: events::now(),
            inner: Mutex::new(Inner { state: State::Connecting, error: None, dev: None, port: None, attached_at: None, retries: 0, info: Value::Null }),
            rx: AtomicU64::new(0),
            tx: AtomicU64::new(0),
            cancel,
            finished,
        });
        self.map.lock().unwrap().insert(att.id, att.clone());
        events::emit("state", json!({}));
        let (reg, a) = (self.clone(), att.clone());
        tokio::spawn(async move {
            run(&a).await;
            a.set(State::Ended, a.error());
            reg.map.lock().unwrap().remove(&a.id);
            events::emit("attachment.ended", json!({"id": a.id, "addr": a.addr, "busid": a.busid, "error": a.error()}));
            events::emit("state", json!({}));
            let _ = a.finished.send(true);
        });
        att
    }

    /// Wait until the attachment is attached, or has ended (returns the error then).
    pub async fn wait_attached(&self, att: &Arc<Attachment>, max: Duration) -> Result<()> {
        let mut fin = att.finished.subscribe();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            match att.state() {
                State::Attached => return Ok(()),
                State::Ended => bail!("{}", att.error().unwrap_or_else(|| "attachment ended".into())),
                _ => {}
            }
            if *fin.borrow() {
                bail!("{}", att.error().unwrap_or_else(|| "attachment ended".into()));
            }
            if tokio::time::Instant::now() >= deadline {
                // Keep trying in the background for auto attachments, otherwise give up.
                if att.auto {
                    bail!("still connecting after {}s ({}); it keeps retrying in the background", max.as_secs(), att.error().unwrap_or_else(|| "no error yet".into()));
                }
                let _ = att.cancel.send(true);
                bail!("timed out after {}s waiting for the device", max.as_secs());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = fin.changed() => {}
            }
        }
    }

    /// Safely eject: unmount filesystems (aborts if busy unless `force`), then detach and release the device.
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut, unused_variables))]
    pub async fn detach(&self, att: &Arc<Attachment>, force: bool) -> Result<Vec<String>> {
        let mut unmounted = vec![];
        #[cfg(target_os = "linux")]
        if let Some(local) = att.local_busid() {
            att.set(State::Detaching, None);
            match tokio::task::spawn_blocking(move || crate::import::vhci::unmount(&local, force)).await? {
                Ok(m) => unmounted = m,
                Err(e) => {
                    att.set(State::Attached, None);
                    return Err(e);
                }
            }
        }
        att.set(State::Detaching, None);
        let _ = att.cancel.send(true);
        let mut fin = att.finished.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(20), async {
            while !*fin.borrow() {
                if fin.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        Ok(unmounted)
    }

    /// Detach everything (daemon shutdown). Never errors; busy mounts are left to the kernel's disconnect handling.
    pub async fn shutdown_all(&self) {
        let all = self.list();
        for a in &all {
            let _ = a.cancel.send(true);
        }
        for a in all {
            let mut fin = a.finished.subscribe();
            let _ = tokio::time::timeout(Duration::from_secs(10), async {
                while !*fin.borrow() {
                    if fin.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
    }
}

enum End {
    Cancelled,
    Dropped(String),
}

/// Ask the exporter what this device is (best effort, never blocks the attach).
async fn learn_info(att: &Arc<Attachment>) {
    let (a, busid) = (att.clone(), att.busid.clone());
    tokio::spawn(async move {
        let r = tokio::time::timeout(Duration::from_secs(8), crate::import::list_remote(&a.addr)).await;
        if let Ok(Ok(list)) = r {
            if let Some(d) = list.into_iter().find(|d| d["busid"] == busid.as_str()) {
                a.inner.lock().unwrap().info = d;
                events::emit("state", json!({}));
            }
        }
    });
}

async fn run(att: &Arc<Attachment>) {
    let mut cancel = att.cancel.subscribe();
    learn_info(att).await;
    loop {
        att.set(if att.inner.lock().unwrap().retries > 0 { State::Reconnecting } else { State::Connecting }, att.error());
        let reason = match once(att, &mut cancel).await {
            Ok(End::Cancelled) => return,
            Ok(End::Dropped(m)) => m,
            Err(e) => format!("{e:#}"),
        };
        crate::logln!("attachment {} ({} {}): {reason}", att.id, att.addr, att.busid);
        if *cancel.borrow() {
            return;
        }
        if !att.auto {
            att.inner.lock().unwrap().error = Some(reason);
            return;
        }
        let n = {
            let mut i = att.inner.lock().unwrap();
            i.retries += 1;
            i.port = None;
            i.error = Some(reason);
            i.retries
        };
        att.set(State::Reconnecting, att.error());
        let wait = Duration::from_secs((1u64 << n.min(5)).min(30));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = cancel.changed() => return,
        }
    }
}

/// Close the connection to the exporter properly (TLS close_notify). The exporter reads that as "the importer meant to
/// leave" and hands the drive straight back; any other ending is treated as a lost connection and the drive is held for
/// this importer for a while (`FLAG_RESUME`). Best effort: a dead connection just times out.
#[cfg(any(target_os = "linux", windows))]
async fn close_cleanly<W: tokio::io::AsyncWrite + Unpin>(w: &mut W) {
    let _ = tokio::time::timeout(Duration::from_secs(2), tokio::io::AsyncWriteExt::shutdown(w)).await;
}

#[cfg(any(target_os = "linux", windows))]
fn ended_reason(e: crate::accel::Ended) -> String {
    use crate::accel::Ended::*;
    match e {
        Local(None) => "the local USB stack closed the device".into(),
        Local(Some(e)) => e.to_string(),
        Remote(None) => "the exporter closed the connection".into(),
        Remote(Some(e)) => format!("connection lost: {e}"),
    }
}

#[cfg(any(target_os = "linux", windows))]
async fn copy_count<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(r: &mut R, w: &mut W, n: &AtomicU64) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let k = r.read(&mut buf).await?;
        if k == 0 {
            return Ok(());
        }
        w.write_all(&buf[..k]).await?;
        w.flush().await?;
        n.fetch_add(k as u64, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
async fn once(att: &Arc<Attachment>, cancel: &mut watch::Receiver<bool>) -> Result<End> {
    use crate::import::vhci;
    use std::os::fd::AsRawFd;
    use tokio::net::TcpStream;
    use usbip_proto::*;

    vhci::ensure_loaded()?;
    let mut s = tokio::select! {
        r = import::connect(&att.addr) => r?,
        _ = cancel.changed() => return Ok(End::Cancelled),
    };
    let accel_on = import::request_flags(&mut s, import::session_flags(att.read_only, att.auto)).await? & FLAG_ACCEL != 0;
    s.write_all(&encode_import_request(&att.busid)).await?;
    let mut h = [0u8; 8];
    s.read_exact(&mut h).await?;
    let status = OpHeader::decode(&h)?.status;
    if status != 0 {
        return Err(import::refusal(&att.addr, &att.busid, status));
    }
    let mut b = [0u8; DEVICE_LEN];
    s.read_exact(&mut b).await?;
    let d = UsbDevice::decode(&b)?;

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let kernel_side = std::net::TcpStream::connect(listener.local_addr()?)?;
    let (our_side, _) = listener.accept()?;
    kernel_side.set_nodelay(true)?;
    our_side.set_nodelay(true)?;
    let port = vhci::free_port(d.speed)?;
    vhci::attach(port, kernel_side.as_raw_fd(), (d.busnum << 16) | d.devnum, d.speed)?;
    drop(kernel_side); // the kernel holds its own reference now
    {
        let mut i = att.inner.lock().unwrap();
        i.dev = Some(DevInfo { vid: d.id_vendor, pid: d.id_product, speed: d.speed });
        i.port = Some(port);
        i.attached_at = Some(events::now());
        i.retries = 0;
        i.error = None;
    }
    att.set(State::Attached, None);
    crate::logln!("attached {} {:04x}:{:04x} from {} on vhci port {port}", d.busid, d.id_vendor, d.id_product, att.addr);

    our_side.set_nonblocking(true)?;
    let (mut lr, mut lw) = TcpStream::from_std(our_side)?.into_split();
    let (mut rr, mut rw) = tokio::io::split(s);
    let mut clean = false; // did this side decide to leave (detach on purpose, or the local USB stack closed it)?
    let end = if accel_on {
        tokio::select! {
            e = crate::accel::relay(&mut lr, &mut lw, &mut rr, &mut rw, true, false, &att.tx, &att.rx) => { clean = matches!(e, crate::accel::Ended::Local(None)); End::Dropped(ended_reason(e)) },
            _ = cancel.changed() => { clean = true; End::Cancelled },
        }
    } else {
        tokio::select! {
            r = copy_count(&mut lr, &mut rw, &att.tx) => { clean = r.is_ok(); End::Dropped(match r { Ok(()) => "the local USB stack closed the device".into(), Err(e) => e.to_string() }) },
            r = copy_count(&mut rr, &mut lw, &att.rx) => End::Dropped(match r { Ok(()) => "the exporter closed the connection".into(), Err(e) => format!("connection lost: {e}") }),
            _ = cancel.changed() => { clean = true; End::Cancelled },
        }
    };
    if clean {
        close_cleanly(&mut rw).await;
    }
    let _ = vhci::detach(port);
    for _ in 0..30 {
        if vhci::port_free(port) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    crate::logln!("detached vhci port {port}");
    Ok(end)
}

#[cfg(target_os = "macos")]
async fn once(att: &Arc<Attachment>, cancel: &mut watch::Receiver<bool>) -> Result<End> {
    let client = tokio::select! {
        r = crate::urb::UrbClient::import(&att.addr, &att.busid, att.read_only, att.auto) => r?,
        _ = cancel.changed() => return Ok(End::Cancelled),
    };
    let d = client.device.clone();
    let ctrl = crate::mac::Controller::new(client.clone(), tokio::runtime::Handle::current()).map_err(|e| anyhow::anyhow!(e))?;
    ctrl.wait_powered().await;
    ctrl.set_connected(true);
    {
        let mut i = att.inner.lock().unwrap();
        i.dev = Some(DevInfo { vid: d.id_vendor, pid: d.id_product, speed: d.speed });
        i.attached_at = Some(events::now());
        i.retries = 0;
        i.error = None;
    }
    att.set(State::Attached, None);
    crate::logln!("attached {} {:04x}:{:04x} from {}", d.busid, d.id_vendor, d.id_product, att.addr);
    // The macOS importer counts its own traffic (the other importers' relay does it for them): copy it to the attachment once a second.
    let stats = {
        let (att, client) = (att.clone(), client.clone());
        tokio::spawn(async move {
            loop {
                let (tx, rx) = client.traffic();
                att.tx.store(tx, std::sync::atomic::Ordering::Relaxed);
                att.rx.store(rx, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    };
    let end = tokio::select! {
        _ = client.closed() => End::Dropped("the exporter disconnected".into()),
        _ = cancel.changed() => { client.leave_cleanly(); End::Cancelled },
    };
    stats.abort();
    ctrl.set_connected(false);
    tokio::time::sleep(Duration::from_millis(500)).await; // let the kernel process the removal
    Ok(end)
}

/// Windows has no built-in virtual USB host controller, so the device is handed to the open-source usbip-win2
/// driver. Its `usbip.exe attach` speaks plain USB/IP to whatever address it is given; we give it a loopback port
/// and relay that stream over mutual TLS, exactly like the Linux path does for vhci_hcd.
#[cfg(windows)]
async fn once(att: &Arc<Attachment>, cancel: &mut watch::Receiver<bool>) -> Result<End> {
    use tokio::net::TcpListener;
    let usbip = crate::windrv::usbip_exe_or_extract().or_else(crate::winusbip::find).ok_or_else(|| anyhow::anyhow!("the Windows virtual USB host controller driver is not installed: run `periwire driver install` as Administrator"))?;
    let mut remote = tokio::select! {
        r = import::connect(&att.addr) => r?,
        _ = cancel.changed() => return Ok(End::Cancelled),
    };
    // usbip.exe sends the import request itself, through the relay below; the session options go first, here.
    let accel_on = import::request_flags(&mut remote, import::session_flags(att.read_only, att.auto)).await? & usbip_proto::FLAG_ACCEL != 0;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let lport = listener.local_addr()?.port();
    let attach = tokio::spawn(crate::winusbip::attach(usbip.clone(), lport, att.busid.clone()));
    let (local, _) = tokio::time::timeout(Duration::from_secs(15), listener.accept()).await.map_err(|_| anyhow::anyhow!("usbip.exe never connected"))??;
    local.set_nodelay(true)?;
    let (mut lr, mut lw) = local.into_split();
    let (mut rr, mut rw) = tokio::io::split(remote);
    // The driver performs the import handshake through this relay, so the relay must already be running.
    // Resolves to how it ended, and whether this side chose to leave (see `close_cleanly`).
    let relay = async {
        if accel_on {
            let e = crate::accel::relay(&mut lr, &mut lw, &mut rr, &mut rw, true, true, &att.tx, &att.rx).await;
            let local = matches!(e, crate::accel::Ended::Local(None));
            (End::Dropped(ended_reason(e)), local)
        } else {
            tokio::select! {
                r = copy_count(&mut lr, &mut rw, &att.tx) => { let ok = r.is_ok(); (End::Dropped(match r { Ok(()) => "the local USB stack closed the device".into(), Err(e) => e.to_string() }), ok) },
                r = copy_count(&mut rr, &mut lw, &att.rx) => (End::Dropped(match r { Ok(()) => "the exporter closed the connection".into(), Err(e) => format!("connection lost: {e}") }), false),
            }
        }
    };
    let mut relay = Box::pin(relay);
    let port = tokio::select! {
        r = attach => r??,
        e = &mut relay => return Ok(e.0),
        _ = cancel.changed() => {
            drop(relay);
            close_cleanly(&mut rw).await;
            return Ok(End::Cancelled)
        },
    };
    {
        let mut i = att.inner.lock().unwrap();
        i.port = Some(port);
        i.attached_at = Some(events::now());
        i.retries = 0;
        i.error = None;
    }
    att.set(State::Attached, None);
    crate::logln!("attached {} from {} on usbip port {port}", att.busid, att.addr);
    let (end, clean) = tokio::select! {
        e = &mut relay => e,
        _ = cancel.changed() => (End::Cancelled, true),
    };
    drop(relay);
    if clean {
        close_cleanly(&mut rw).await;
    }
    let _ = crate::winusbip::detach(&usbip, port).await;
    crate::logln!("detached usbip port {port}");
    Ok(end)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
async fn once(_: &Arc<Attachment>, _: &mut watch::Receiver<bool>) -> Result<End> {
    bail!("importing devices is not implemented on this OS yet")
}

pub fn can_import() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if !crate::import::vhci::available() {
            return Err("the kernel module vhci-hcd is not available".into());
        }
        if unsafe { libc::geteuid() } != 0 {
            return Err("importing needs root (the daemon must run as root to attach devices)".into());
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        return crate::mac::probe().map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    {
        if crate::windrv::usbip_exe_or_extract().or_else(crate::winusbip::find).is_none() {
            return Err("the Windows virtual USB host controller tools are missing: run `periwire driver install` as Administrator".into());
        }
        return match crate::windrv::device() {
            Some(_) => Ok(()),
            None => Err("the Windows virtual USB host controller driver is not installed: run `periwire driver install` as Administrator".into()),
        };
    }
    #[allow(unreachable_code)]
    Err("importing devices is not implemented on this OS yet".into())
}
