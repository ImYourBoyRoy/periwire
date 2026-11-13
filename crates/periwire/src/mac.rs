//! macOS importer: a virtual USB host controller built on `IOUSBHostControllerInterface`.
//!
//! The kernel drives this controller exactly like real hardware: it sends commands (power the
//! port, create a device, create an endpoint, ...) and rings doorbells when transfer rings have
//! new work. We answer commands through Apple's state-machine helpers, and turn each transfer
//! message into a USB/IP URB sent to the exporter (see `urb.rs`).
//!
//! Threading: every handler runs on one serial dispatch queue we own, and URB results hop back
//! onto that queue before touching any state machine, so the Objective-C objects below are never
//! used concurrently. That is what the `unsafe impl Send/Sync` declarations rely on.
//!
//! Requires either the `com.apple.developer.usb.host-controller-interface` entitlement (with a
//! provisioning profile) or SIP disabled (see docs/MACOS.md).

use crate::hci::{self, ty, Addresses, EpDesc, Msg, Outcome, Window, Xfer};
use crate::urb::{SubmitRequest, UrbClient, UrbResult};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{rc::Retained, AnyThread};
use objc2_foundation::{NSError, NSMutableData};
use objc2_io_usb_host::*;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;

const ROOT_PORT: usize = 1;
const ENODEV: i32 = -19;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn raw(m: &IOUSBHostCIMessage) -> Msg {
    Msg { control: m.control, data0: m.data0, data1: m.data1 }
}

fn why(e: Retained<NSError>) -> String {
    format!("{} ({:#x})", e.localizedDescription(), e.code() as u32)
}

fn ci_status(o: Outcome) -> IOUSBHostCIMessageStatus {
    match o {
        Outcome::Success => IOUSBHostCIMessageStatus::Success,
        Outcome::Offline => IOUSBHostCIMessageStatus::Offline,
        Outcome::Timeout => IOUSBHostCIMessageStatus::Timeout,
        Outcome::Stopped => IOUSBHostCIMessageStatus::EndpointStopped,
        Outcome::Protocol => IOUSBHostCIMessageStatus::ProtocolError,
        Outcome::Transaction => IOUSBHostCIMessageStatus::TransactionError,
        Outcome::Overrun => IOUSBHostCIMessageStatus::OverrunError,
        Outcome::Stall => IOUSBHostCIMessageStatus::StallError,
        Outcome::Error => IOUSBHostCIMessageStatus::Error,
    }
}

/// USB/IP speed code (Linux `enum usb_device_speed`) to the controller's speed.
fn ci_speed(code: u32) -> IOUSBHostCIDeviceSpeed {
    match code {
        1 => IOUSBHostCIDeviceSpeed::Low,
        3 | 4 => IOUSBHostCIDeviceSpeed::High,
        5 => IOUSBHostCIDeviceSpeed::Super,
        6 => IOUSBHostCIDeviceSpeed::SuperPlus,
        _ => IOUSBHostCIDeviceSpeed::Full,
    }
}

#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> i32;
}

/// Maps `mach_absolute_time` to 1 ms USB frames. The kernel rejects two reports of the same frame.
struct FrameClock {
    epoch: u64,
    numer: u128,
    denom: u128,
    last: AtomicU64,
}

impl FrameClock {
    fn new() -> Self {
        let mut tb = Timebase { numer: 1, denom: 1 };
        unsafe { mach_timebase_info(&mut tb) };
        Self { epoch: unsafe { mach_absolute_time() }, numer: tb.numer as u128, denom: tb.denom as u128, last: AtomicU64::new(0) }
    }
    fn frame_of(&self, ticks: u64) -> u64 {
        ((ticks - self.epoch) as u128 * self.numer / (self.denom * 1_000_000)) as u64 + 1
    }
    fn start_of(&self, frame: u64) -> u64 {
        self.epoch + ((frame - 1) as u128 * self.denom * 1_000_000 / self.numer) as u64
    }
    /// A frame number later than any reported before, and the mach time at which it began.
    fn next(&self) -> (u64, u64) {
        loop {
            let f = self.frame_of(unsafe { mach_absolute_time() });
            let last = self.last.load(Ordering::SeqCst);
            if f > last && self.last.compare_exchange(last, f, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                return (f, self.start_of(f));
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    ControlData,
    ControlStatus,
    Bulk,
}

/// What is known about a submitted URB besides its place in the endpoint's window (which records its number and the address of
/// the ring message it was built from: a result for a message that is no longer the ring head, because the endpoint was paused,
/// reset or moved, is dropped so that a reclaimed buffer is never written).
struct InFlight {
    stage: Stage,
    is_in: bool,
    len: u32,
}

struct Ep {
    sm: Retained<IOUSBHostCIEndpointStateMachine>,
    desc: EpDesc,
    /// The URBs in flight for this endpoint, oldest first. Control endpoints have at most one; a bulk endpoint reads ahead along its ring.
    win: Window<InFlight, UrbResult>,
    setup: Option<[u8; 8]>,
    data_done: bool,
    /// Write-behind: OUT transfers already completed to the kernel whose result from the exporter is still out (number -> length), their
    /// total, and whether one of them failed (the next transfer then fails, as the device would have refused it).
    early: HashMap<u32, u32>,
    early_bytes: u64,
    early_failed: bool,
}

struct Shared {
    client: Arc<UrbClient>,
    runtime: tokio::runtime::Handle,
    queue: DispatchRetained<DispatchQueue>,
    speed: IOUSBHostCIDeviceSpeed,
    devices: Mutex<HashMap<u8, Retained<IOUSBHostCIDeviceStateMachine>>>,
    eps: Mutex<HashMap<(u8, u8), Ep>>,
    addrs: Mutex<Addresses>,
    powered: watch::Sender<bool>,
    clock: FrameClock,
    enumerated: AtomicBool,
    /// Bulk URBs kept in flight per endpoint (`PERIWIRE_PIPELINE`).
    depth: usize,
    /// Write-behind limit in bytes per endpoint (`PERIWIRE_WRITE_BEHIND`), the configuration the kernel selected, and for each
    /// configuration descriptor read so far the bulk OUT endpoints of its CDC-NCM (Ethernet over USB) data interfaces.
    behind_cap: u64,
    active_cfg: AtomicU8,
    ncm: Mutex<HashMap<u8, Vec<u8>>>,
}

// SAFETY: see the module documentation; state machines are only used on `queue`.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct ForceSend<T>(T);
unsafe impl<T> Send for ForceSend<T> {}
unsafe impl<T> Sync for ForceSend<T> {}

type NsRes = Result<(), Retained<NSError>>;

/// Inspect, act, respond. The kernel waits for an answer to every command, so one is always sent.
fn answer(inspect: impl FnOnce() -> NsRes, respond: impl Fn(IOUSBHostCIMessageStatus) -> NsRes, act: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    if let Err(e) = inspect() {
        let _ = respond(IOUSBHostCIMessageStatus::Error);
        return Err(format!("rejected: {}", why(e)));
    }
    let r = act();
    respond(if r.is_ok() { IOUSBHostCIMessageStatus::Success } else { IOUSBHostCIMessageStatus::Error }).map_err(why)?;
    r
}

fn complete(ep: &Ep, ptr: NonNull<IOUSBHostCIMessage>, outcome: Outcome, len: u32) -> bool {
    match unsafe { ep.sm.enqueueTransferCompletionForMessage_status_transferLength_error(ptr, ci_status(outcome), len as usize) } {
        Ok(()) => true,
        Err(e) => {
            eprintln!("complete transfer: {}", why(e));
            false
        }
    }
}

impl Shared {
    fn trace() -> bool {
        std::env::var_os("PERIWIRE_TRACE").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
    }

    fn command(self: &Arc<Self>, controller: &IOUSBHostControllerInterface, command: &IOUSBHostCIMessage) -> Result<(), String> {
        let msg = raw(command);
        let ptr = NonNull::from(command);
        let kind = msg.kind();
        if Self::trace() {
            eprintln!("[mac] command {kind:#04x} data0 {:#010x}", msg.data0);
        }
        match kind {
            ty::CONTROLLER_POWER_ON..=ty::CONTROLLER_PAUSE => {
                let sm = unsafe { controller.controllerStateMachine() };
                answer(|| unsafe { sm.inspectCommand_error(ptr) }, |s| unsafe { sm.respondToCommand_status_error(ptr, s) }, || Ok(()))
            }
            ty::CONTROLLER_FRAME_NUMBER => {
                let sm = unsafe { controller.controllerStateMachine() };
                let (frame, ts) = self.clock.next();
                answer(|| unsafe { sm.inspectCommand_error(ptr) }, |s| unsafe { sm.respondToCommand_status_frame_timestamp_error(ptr, s, frame, ts) }, || Ok(()))
            }
            ty::PORT_POWER_ON..=ty::PORT_STATUS => {
                let port = unsafe { controller.getPortStateMachineForCommand_error(ptr) }.map_err(why)?;
                answer(
                    || unsafe { port.inspectCommand_error(ptr) },
                    |s| unsafe { port.respondToCommand_status_error(ptr, s) },
                    || {
                        if kind == ty::PORT_RESET {
                            if self.enumerated.load(Ordering::Relaxed) {
                                // Pass the reset on to the real device (asynchronously: the exporter
                                // holds its transfers until the reset has finished).
                                let _ = self.client.submit(SubmitRequest { ep: 0, setup: [0x23, 0x03, 0x04, 0, 0, 0, 0, 0], length: 0, data: vec![], interval: 0 });
                            }
                            unsafe { port.updateLinkState_speed_inhibitLinkStateChange_error(IOUSBHostCILinkState::U0, self.speed, false) }.map_err(why)?;
                        }
                        Ok(())
                    },
                )?;
                if kind == ty::PORT_POWER_ON {
                    self.powered.send_replace(true);
                }
                Ok(())
            }
            ty::DEVICE_CREATE => {
                let sm = unsafe { IOUSBHostCIDeviceStateMachine::initWithInterface_command_error(IOUSBHostCIDeviceStateMachine::alloc(), controller, ptr) }.map_err(why)?;
                let Some(addr) = lock(&self.addrs).take() else {
                    let _ = unsafe { sm.respondToCommand_status_error(ptr, IOUSBHostCIMessageStatus::NoResources) };
                    return Err("out of device addresses".into());
                };
                if let Err(e) = unsafe { sm.respondToCommand_status_deviceAddress_error(ptr, IOUSBHostCIMessageStatus::Success, addr as usize) } {
                    lock(&self.addrs).give_back(addr);
                    return Err(why(e));
                }
                self.enumerated.store(true, Ordering::Relaxed);
                lock(&self.devices).insert(addr, sm);
                Ok(())
            }
            ty::DEVICE_DESTROY..=ty::DEVICE_UPDATE => {
                let addr = msg.device();
                let sm = lock(&self.devices).get(&addr).cloned().ok_or_else(|| format!("unknown device {addr}"))?;
                answer(
                    || unsafe { sm.inspectCommand_error(ptr) },
                    |s| unsafe { sm.respondToCommand_status_error(ptr, s) },
                    || {
                        match kind {
                            ty::DEVICE_DESTROY => {
                                let mut eps = lock(&self.eps);
                                let keys: Vec<_> = eps.keys().filter(|k| k.0 == addr).copied().collect();
                                for k in keys {
                                    if let Some(mut ep) = eps.remove(&k) {
                                        self.cancel(&mut ep);
                                    }
                                }
                                drop(eps);
                                lock(&self.devices).remove(&addr);
                                lock(&self.addrs).give_back(addr);
                            }
                            ty::DEVICE_PAUSE => {
                                for (k, ep) in lock(&self.eps).iter_mut() {
                                    if k.0 == addr {
                                        self.pause(ep);
                                    }
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    },
                )
            }
            ty::ENDPOINT_CREATE => {
                let sm = unsafe { IOUSBHostCIEndpointStateMachine::initWithInterface_command_error(IOUSBHostCIEndpointStateMachine::alloc(), controller, ptr) }.map_err(why)?;
                unsafe { sm.respondToCommand_status_error(ptr, IOUSBHostCIMessageStatus::Success) }.map_err(why)?;
                let key = unsafe { (sm.deviceAddress() as u8, sm.endpointAddress() as u8) };
                let desc = endpoint_descriptor(&msg);
                if Self::trace() {
                    eprintln!("[mac] endpoint {key:?} {desc:?}");
                }
                lock(&self.eps).insert(key, Ep { sm, desc, win: Window::default(), setup: None, data_done: false, early: HashMap::new(), early_bytes: 0, early_failed: false });
                Ok(())
            }
            ty::ENDPOINT_DESTROY..=ty::ENDPOINT_SET_NEXT_TRANSFER => {
                let key = (msg.device(), msg.endpoint());
                let sm = lock(&self.eps).get(&key).map(|e| e.sm.clone()).ok_or_else(|| format!("unknown endpoint {key:?}"))?;
                answer(
                    || unsafe { sm.inspectCommand_error(ptr) },
                    |s| unsafe { sm.respondToCommand_status_error(ptr, s) },
                    || {
                        let mut eps = lock(&self.eps);
                        match kind {
                            ty::ENDPOINT_DESTROY => {
                                if let Some(mut ep) = eps.remove(&key) {
                                    self.cancel(&mut ep);
                                }
                            }
                            ty::ENDPOINT_PAUSE => {
                                if let Some(ep) = eps.get_mut(&key) {
                                    self.pause(ep);
                                }
                            }
                            ty::ENDPOINT_UPDATE => {
                                if let Some(ep) = eps.get_mut(&key) {
                                    ep.desc = endpoint_descriptor(&msg);
                                }
                            }
                            ty::ENDPOINT_SET_NEXT_TRANSFER => {
                                // The ring position moved: what was read ahead no longer matches it.
                                if let Some(ep) = eps.get_mut(&key) {
                                    self.pause(ep);
                                }
                            }
                            ty::ENDPOINT_RESET => {
                                if let Some(ep) = eps.get_mut(&key) {
                                    self.pause(ep);
                                    ep.setup = None;
                                    ep.data_done = false;
                                    if key.1 & 0x7f != 0 {
                                        // Clear the halt on the real endpoint too.
                                        let _ = self.client.submit(SubmitRequest { ep: 0, setup: [0x02, 0x01, 0, 0, key.1, 0, 0, 0], length: 0, data: vec![], interval: 0 });
                                    }
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    },
                )
            }
            other => {
                eprintln!("[mac] ignoring unsupported command {other:#04x}");
                Ok(())
            }
        }
    }

    /// Unlink whatever is in flight for this endpoint (its results will be dropped).
    fn pause(&self, ep: &mut Ep) {
        for seq in ep.win.flush() {
            self.client.unlink(seq);
        }
        for (seq, _) in ep.early.drain() {
            self.client.unlink(seq);
        }
        ep.early_bytes = 0;
        ep.early_failed = false;
    }

    /// Is this a bulk OUT endpoint of an Ethernet-over-USB (CDC-NCM) link in the configuration the kernel selected?
    fn is_ncm_out(&self, ep_addr: u8) -> bool {
        let cfg = self.active_cfg.load(Ordering::Relaxed);
        lock(&self.ncm).get(&cfg).is_some_and(|v| v.contains(&(ep_addr & 0x0f)))
    }

    fn cancel(&self, ep: &mut Ep) {
        self.pause(ep);
    }

    fn doorbell(self: &Arc<Self>, doorbell: u32) {
        let key = hci::doorbell_target(doorbell);
        let sm = lock(&self.eps).get(&key).map(|e| e.sm.clone());
        let Some(sm) = sm else {
            eprintln!("[mac] doorbell for unknown endpoint {key:?}");
            return;
        };
        if let Err(e) = unsafe { sm.processDoorbell_error(doorbell) } {
            eprintln!("[mac] doorbell {key:?}: {}", why(e));
            return;
        }
        self.pump(key);
    }

    /// Walks the endpoint's ring from its head, completing what needs no I/O and submitting the
    /// first message that does. Called again whenever a URB finishes.
    fn pump(self: &Arc<Self>, key: (u8, u8)) {
        let control = key.1 & 0x7f == 0;
        loop {
            let mut eps = lock(&self.eps);
            let Some(ep) = eps.get_mut(&key) else { return };
            if unsafe { ep.sm.endpointState() } != IOUSBHostCIEndpointState::Active {
                return;
            }
            if !ep.win.is_empty() {
                // Waiting for results; a bulk endpoint meanwhile takes in whatever the driver has queued since.
                if !control && ep.desc.kind == hci::Kind::Bulk {
                    self.read_ahead(ep, key);
                }
                return;
            }
            let ptr = unsafe { ep.sm.currentTransferMessage() };
            let msg = raw(unsafe { ptr.as_ref() });
            let Some(x) = hci::decode(&msg) else { return };
            let ok = match x {
                Xfer::Link { respond: true } => complete(ep, ptr, Outcome::Success, 0),
                Xfer::Link { respond: false } => return,
                Xfer::Setup(b) if control => {
                    ep.setup = Some(b);
                    ep.data_done = false;
                    complete(ep, ptr, Outcome::Success, 0)
                }
                Xfer::Data { len } if control => match ep.setup {
                    Some(s) if !ep.data_done => {
                        let len = len.min(u16::from_le_bytes([s[6], s[7]]) as u32);
                        let is_in = s[0] & 0x80 != 0;
                        let data = if is_in { vec![] } else { read_buffer(&msg, len) };
                        self.submit(ep, key, ptr.as_ptr() as usize, Stage::ControlData, SubmitRequest { ep: 0, setup: s, length: len, data, interval: 0 }, is_in);
                        return;
                    }
                    _ => complete(ep, ptr, Outcome::Error, 0),
                },
                Xfer::Status if control => match ep.setup {
                    // No data stage happened: the status stage carries the whole request.
                    Some(s) if !ep.data_done => {
                        self.submit(ep, key, ptr.as_ptr() as usize, Stage::ControlStatus, SubmitRequest { ep: 0, setup: s, length: 0, data: vec![], interval: 0 }, false);
                        return;
                    }
                    _ => {
                        ep.setup = None;
                        complete(ep, ptr, Outcome::Success, 0)
                    }
                },
                Xfer::Data { len } => {
                    let is_in = key.1 & 0x80 != 0;
                    let interval = ep.desc.urb_interval(self.client.device.speed);
                    if !is_in && len > 0 && ep.desc.kind == hci::Kind::Bulk && self.behind_cap > 0 && self.is_ncm_out(key.1) {
                        // Write-behind. The driver of an Ethernet-over-USB link posts one transfer and waits for it before the next, so each
                        // would cost a network round trip (about 30 KB per round trip). Frames are network traffic that the protocol above
                        // (TCP) already protects, so complete the transfer to the kernel now, send the data on in order, and only hold the
                        // driver back once `behind_cap` bytes are out unacknowledged. Never for storage: there a failed write must be reported.
                        if ep.early_failed {
                            ep.early_failed = false;
                            complete(ep, ptr, Outcome::Error, 0)
                        } else if ep.early_bytes == 0 || ep.early_bytes + u64::from(len) <= self.behind_cap {
                            let data = read_buffer(&msg, len);
                            self.submit_early(ep, key, SubmitRequest { ep: key.1, setup: [0; 8], length: len, data, interval }, len);
                            complete(ep, ptr, Outcome::Success, len)
                        } else {
                            return; // the budget is used up: `finish` runs this again as results come back
                        }
                    } else {
                        let data = if is_in { vec![] } else { read_buffer(&msg, len) };
                        self.submit(ep, key, ptr.as_ptr() as usize, Stage::Bulk, SubmitRequest { ep: key.1, setup: [0; 8], length: len, data, interval }, is_in);
                        if ep.desc.kind == hci::Kind::Bulk {
                            self.read_ahead(ep, key);
                        }
                        return;
                    }
                }
                other => {
                    // Isochronous transfers are not supported yet; fail them so the driver can cope.
                    eprintln!("[mac] endpoint {key:?}: unsupported transfer {other:?}");
                    complete(ep, ptr, Outcome::Error, 0)
                }
            };
            if !ok {
                return;
            }
        }
    }

    fn submit(self: &Arc<Self>, ep: &mut Ep, key: (u8, u8), tag: usize, stage: Stage, req: SubmitRequest, is_in: bool) {
        if Self::trace() {
            eprintln!("[mac] submit {key:?} {stage:?} len={} in flight={} setup={:02x?}", req.length, ep.win.len() + 1, req.setup);
        }
        if stage != Stage::Bulk && req.setup[0] == 0x00 && req.setup[1] == 0x09 {
            self.active_cfg.store(req.setup[2], Ordering::Relaxed); // SET_CONFIGURATION: which configuration's endpoints are live
        }
        let len = req.length;
        let (seq, rx) = self.client.submit(req);
        ep.win.push(tag, seq, InFlight { stage, is_in, len });
        let shared = Arc::clone(self);
        self.runtime.spawn(async move {
            let res = rx.await.unwrap_or(UrbResult { status: ENODEV, data: vec![], actual: 0 });
            let shared2 = Arc::clone(&shared);
            shared.queue.exec_async(move || shared2.finish(key, seq, res));
        });
    }

    /// Sends an OUT transfer that has already been completed to the kernel (write-behind); its result only frees budget.
    fn submit_early(self: &Arc<Self>, ep: &mut Ep, key: (u8, u8), req: SubmitRequest, len: u32) {
        if Self::trace() {
            eprintln!("[mac] submit {key:?} Early len={len} unacknowledged={}", ep.early_bytes + u64::from(len));
        }
        let (seq, rx) = self.client.submit(req);
        ep.early.insert(seq, len);
        ep.early_bytes += u64::from(len);
        let shared = Arc::clone(self);
        self.runtime.spawn(async move {
            let res = rx.await.unwrap_or(UrbResult { status: ENODEV, data: vec![], actual: 0 });
            let shared2 = Arc::clone(&shared);
            shared.queue.exec_async(move || shared2.finish(key, seq, res));
        });
    }

    /// Submits the transfers the driver has queued right behind the ones in flight, without waiting for those to come back: with
    /// one URB at a time, throughput is one transfer per network round trip. Only plain data entries that follow in the same ring
    /// segment are taken (a link, a not yet valid entry or a limit ends it); the results still complete in ring order.
    fn read_ahead(self: &Arc<Self>, ep: &mut Ep, key: (u8, u8)) {
        if self.depth <= 1 {
            return;
        }
        let Some(last) = ep.win.back_tag() else { return };
        let size = std::mem::size_of::<IOUSBHostCIMessage>();
        let bytes: u64 = ep.win.metas().map(|f| u64::from(f.len)).sum();
        // SAFETY: the ring is a run of messages that ends in a link entry, which stops the walk before the end of the memory.
        let at = |i: usize| raw(unsafe { &*((last + i * size) as *const IOUSBHostCIMessage) });
        let lens = hci::run_ahead((1..).map(at), ep.win.len(), bytes, self.depth);
        let is_in = key.1 & 0x80 != 0;
        let interval = ep.desc.urb_interval(self.client.device.speed);
        for (i, len) in lens.into_iter().enumerate() {
            let tag = last + (i + 1) * size;
            let data = if is_in { vec![] } else { read_buffer(&at(i + 1), len) };
            self.submit(ep, key, tag, Stage::Bulk, SubmitRequest { ep: key.1, setup: [0; 8], length: len, data, interval }, is_in);
        }
    }

    fn finish(self: &Arc<Self>, key: (u8, u8), seq: u32, res: UrbResult) {
        {
            let mut eps = lock(&self.eps);
            let Some(ep) = eps.get_mut(&key) else { return };
            if let Some(len) = ep.early.remove(&seq) {
                ep.early_bytes = ep.early_bytes.saturating_sub(u64::from(len));
                if res.status < 0 {
                    eprintln!("[mac] endpoint {key:?}: a write that was completed early failed ({}); the next transfer will fail", res.status);
                    ep.early_failed = true;
                }
                drop(eps);
                self.pump(key); // there may be room in the budget again
                return;
            }
            if !ep.win.finish(seq, res) {
                return; // unlinked/paused/destroyed: nothing may touch the ring any more
            }
            // Hand results to the kernel strictly in ring order: a later URB may well be back before an earlier one.
            while let Some((tag, f, res)) = ep.win.pop_ready() {
                let ptr = unsafe { ep.sm.currentTransferMessage() };
                if ptr.as_ptr() as usize != tag || unsafe { ep.sm.endpointState() } != IOUSBHostCIEndpointState::Active {
                    self.pause(ep);
                    return;
                }
                let msg = raw(unsafe { ptr.as_ref() });
                let outcome = hci::outcome_from_urb_status(res.status);
                if Self::trace() {
                    eprintln!("[mac] done {key:?} {:?} status={} bytes={} still in flight={}", f.stage, res.status, res.data.len(), ep.win.len());
                }
                let mut n = 0u32;
                if outcome == Outcome::Success {
                    if f.stage == Stage::ControlData && f.is_in {
                        // GET_DESCRIPTOR(configuration): remember which endpoints belong to an Ethernet-over-USB link.
                        if ep.setup.is_some_and(|s| s[0] == 0x80 && s[1] == 0x06 && s[3] == 0x02) {
                            if let Some((cfg, outs)) = hci::ncm_out_endpoints(&res.data) {
                                lock(&self.ncm).insert(cfg, outs);
                            }
                        }
                    }
                    if f.is_in {
                        n = write_buffer(&msg, &res.data);
                    } else {
                        n = res.actual.min(msg.length());
                    }
                    match f.stage {
                        Stage::ControlData => ep.data_done = true,
                        Stage::ControlStatus => ep.setup = None,
                        Stage::Bulk => {}
                    }
                } else if f.stage != Stage::Bulk {
                    ep.setup = None;
                }
                if !complete(ep, ptr, outcome, n) {
                    self.pause(ep);
                    return;
                }
                if outcome != Outcome::Success {
                    // The endpoint halts here: what was read ahead beyond the failed transfer is not delivered.
                    self.pause(ep);
                    break;
                }
            }
        }
        self.pump(key);
    }
}

/// Reads an OUT payload from the transfer message's buffer.
fn read_buffer(msg: &Msg, len: u32) -> Vec<u8> {
    let n = len.min(msg.length()) as usize;
    if n == 0 || msg.buffer() == 0 {
        return vec![];
    }
    // SAFETY: the kernel keeps a valid message's buffer mapped until we complete it.
    unsafe { std::slice::from_raw_parts(msg.buffer() as *const u8, n) }.to_vec()
}

/// Copies an IN payload into the transfer message's buffer; returns the byte count.
fn write_buffer(msg: &Msg, data: &[u8]) -> u32 {
    let n = data.len().min(msg.length() as usize);
    if n > 0 && msg.buffer() != 0 {
        // SAFETY: as above; `n` is bounded by the buffer length the kernel gave us.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), msg.buffer() as *mut u8, n) };
    }
    n as u32
}

/// Reads the endpoint descriptor an EndpointCreate/Update command points at.
fn endpoint_descriptor(msg: &Msg) -> EpDesc {
    if msg.data1 == 0 {
        return EpDesc::EP0;
    }
    // SAFETY: the kernel passes a mapped descriptor that outlives the command; its own length
    // fields bound the read.
    let head = unsafe { std::slice::from_raw_parts(msg.data1 as *const u8, 4) };
    let len = if head[1] == 2 { u16::from_le_bytes([head[2], head[3]]).clamp(4, 512) as usize } else { 7 };
    let d = unsafe { std::slice::from_raw_parts(msg.data1 as *const u8, len) };
    EpDesc::parse(d).unwrap_or(EpDesc::EP0)
}

fn capabilities() -> Retained<NSMutableData> {
    let controller = IOUSBHostCIMessage {
        control: (IOUSBHostCIMessageType::ControllerCapabilities.0 << IOUSBHostCIMessageControlTypePhase)
            | IOUSBHostCIMessageControlNoResponse
            | IOUSBHostCIMessageControlValid
            | (1 << IOUSBHostCICapabilitiesMessageControlPortCountPhase),
        data0: (1 << IOUSBHostCICapabilitiesMessageData0CommandTimeoutThresholdPhase) | (2 << IOUSBHostCICapabilitiesMessageData0ConnectionLatencyPhase),
        data1: 0,
    };
    let port = IOUSBHostCIMessage {
        control: (IOUSBHostCIMessageType::PortCapabilities.0 << IOUSBHostCIMessageControlTypePhase)
            | IOUSBHostCIMessageControlNoResponse
            | IOUSBHostCIMessageControlValid
            | ((ROOT_PORT as u32) << IOUSBHostCIPortCapabilitiesMessageControlPortNumberPhase),
        data0: (900 / 8) << IOUSBHostCIPortCapabilitiesMessageData0MaxPowerPhase,
        data1: 0,
    };
    let data = NSMutableData::new();
    for m in [controller, port] {
        unsafe { data.appendBytes_length(NonNull::from(&m).cast::<c_void>(), std::mem::size_of::<IOUSBHostCIMessage>()) };
    }
    data
}

unsafe extern "C-unwind" fn interest(_ref_con: *mut c_void, _service: u32, message_type: u32, _arg: *mut c_void) {
    if Shared::trace() {
        eprintln!("[mac] controller interest notification {message_type:#x}");
    }
}

/// Tries to create (and immediately destroy) a virtual host controller, to find out whether this
/// process is allowed to. Used by `periwire mac-probe`.
pub fn probe() -> Result<(), String> {
    let queue = DispatchQueue::new("periwire.probe", None);
    let command = RcBlock::new(|_: NonNull<IOUSBHostControllerInterface>, _: IOUSBHostCIMessage| {});
    let bell = RcBlock::new(|_: NonNull<IOUSBHostControllerInterface>, _: NonNull<IOUSBHostCIDoorbell>, _: u32| {});
    let caps = capabilities();
    let mut err = None;
    let interface = unsafe {
        IOUSBHostControllerInterface::initWithCapabilities_queue_interruptRateHz_error_commandHandler_doorbellHandler_interestHandler(
            IOUSBHostControllerInterface::alloc(),
            &caps,
            Some(&queue),
            1000,
            Some(&mut err),
            RcBlock::as_ptr(&command),
            RcBlock::as_ptr(&bell),
            Some(interest),
        )
    };
    match interface {
        Some(i) => {
            unsafe { i.destroy() };
            Ok(())
        }
        None => Err(err.map(why).unwrap_or_else(|| "refused (no error given)".into())),
    }
}

pub struct Controller {
    interface: Retained<IOUSBHostControllerInterface>,
    queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
}

// SAFETY: `interface` is only used through `queue`, except `destroy`, which may be called any time.
unsafe impl Send for Controller {}
unsafe impl Sync for Controller {}

impl Controller {
    pub fn new(client: Arc<UrbClient>, runtime: tokio::runtime::Handle) -> Result<Self, String> {
        let queue = DispatchQueue::new("periwire.controller", None);
        let shared = Arc::new(Shared {
            speed: ci_speed(client.device.speed),
            client,
            runtime,
            queue: queue.clone(),
            devices: Mutex::default(),
            eps: Mutex::default(),
            addrs: Mutex::default(),
            powered: watch::channel(false).0,
            clock: FrameClock::new(),
            enumerated: AtomicBool::new(false),
            depth: hci::depth_from(std::env::var("PERIWIRE_PIPELINE").ok().as_deref()),
            behind_cap: hci::behind_from(std::env::var("PERIWIRE_WRITE_BEHIND").ok().as_deref()),
            active_cfg: AtomicU8::new(0),
            ncm: Mutex::default(),
        });
        let cmd_shared = Arc::clone(&shared);
        let command_block = RcBlock::new(move |controller: NonNull<IOUSBHostControllerInterface>, command: IOUSBHostCIMessage| {
            let controller = unsafe { controller.as_ref() };
            if let Err(e) = cmd_shared.command(controller, &command) {
                eprintln!("[mac] command {:#04x}: {e}", raw(&command).kind());
            }
        });
        let bell_shared = Arc::clone(&shared);
        let doorbell_block = RcBlock::new(move |_: NonNull<IOUSBHostControllerInterface>, bells: NonNull<IOUSBHostCIDoorbell>, count: u32| {
            for &b in unsafe { std::slice::from_raw_parts(bells.as_ptr(), count as usize) } {
                bell_shared.doorbell(b);
            }
        });
        let caps = capabilities();
        let mut err = None;
        let interface = unsafe {
            IOUSBHostControllerInterface::initWithCapabilities_queue_interruptRateHz_error_commandHandler_doorbellHandler_interestHandler(
                IOUSBHostControllerInterface::alloc(),
                &caps,
                Some(&queue),
                1000,
                Some(&mut err),
                RcBlock::as_ptr(&command_block),
                RcBlock::as_ptr(&doorbell_block),
                Some(interest),
            )
        };
        let interface = interface.ok_or_else(|| match err {
            Some(e) => format!("could not create the virtual host controller: {} — this needs the host-controller-interface entitlement, or SIP disabled (see docs/MACOS.md)", why(e)),
            None => "could not create the virtual host controller".to_string(),
        })?;
        Ok(Self { interface, queue, shared })
    }

    /// Resolves once the kernel has powered the controller's root port.
    pub async fn wait_powered(&self) {
        let _ = self.shared.powered.subscribe().wait_for(|p| *p).await;
    }

    /// Plugs the (virtual) device into the root port, or unplugs it.
    pub fn set_connected(&self, connected: bool) {
        let interface = ForceSend(self.interface.clone());
        self.queue.exec_sync(move || {
            let interface = interface;
            match unsafe { interface.0.getPortStateMachineForPort_error(ROOT_PORT) } {
                Ok(port) => unsafe { port.setConnected(connected) },
                Err(e) => eprintln!("[mac] root port unavailable: {}", why(e)),
            }
        });
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let interface = ForceSend(self.interface.clone());
        self.queue.exec_sync(move || {
            let interface = interface;
            unsafe { interface.0.destroy() };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The portable message layout in `hci.rs` must match what the SDK exports.
    #[test]
    fn layout_matches_sdk() {
        assert_eq!(hci::TYPE_MASK, IOUSBHostCIMessageControlType);
        assert_eq!(hci::NO_RESPONSE, IOUSBHostCIMessageControlNoResponse);
        assert_eq!(hci::VALID, IOUSBHostCIMessageControlValid);
        assert_eq!(hci::LEN_MASK, IOUSBHostCINormalTransferData0Length);
        assert_eq!(0xff, IOUSBHostCICommandMessageData0DeviceAddress);
        assert_eq!(0xff00, IOUSBHostCICommandMessageData0EndpointAddress);
        assert_eq!(0xff, IOUSBHostCIDoorbellDeviceAddress);
        assert_eq!(0xff00, IOUSBHostCIDoorbellEndpointAddress);
    }

    #[test]
    fn message_types_match_sdk() {
        use IOUSBHostCIMessageType as T;
        for (ours, sdk) in [
            (ty::CONTROLLER_POWER_ON, T::ControllerPowerOn.0),
            (ty::CONTROLLER_PAUSE, T::ControllerPause.0),
            (ty::CONTROLLER_FRAME_NUMBER, T::ControllerFrameNumber.0),
            (ty::PORT_POWER_ON, T::PortPowerOn.0),
            (ty::PORT_RESET, T::PortReset.0),
            (ty::PORT_STATUS, T::PortStatus.0),
            (ty::DEVICE_CREATE, T::DeviceCreate.0),
            (ty::DEVICE_DESTROY, T::DeviceDestroy.0),
            (ty::DEVICE_PAUSE, T::DevicePause.0),
            (ty::DEVICE_UPDATE, T::DeviceUpdate.0),
            (ty::ENDPOINT_CREATE, T::EndpointCreate.0),
            (ty::ENDPOINT_DESTROY, T::EndpointDestroy.0),
            (ty::ENDPOINT_PAUSE, T::EndpointPause.0),
            (ty::ENDPOINT_UPDATE, T::EndpointUpdate.0),
            (ty::ENDPOINT_RESET, T::EndpointReset.0),
            (ty::ENDPOINT_SET_NEXT_TRANSFER, T::EndpointSetNextTransfer.0),
            (ty::SETUP, T::SetupTransfer.0),
            (ty::NORMAL, T::NormalTransfer.0),
            (ty::STATUS, T::StatusTransfer.0),
            (ty::ISOCHRONOUS, T::IsochronousTransfer.0),
            (ty::LINK, T::Link.0),
        ] {
            assert_eq!(ours, sdk);
        }
    }
}
