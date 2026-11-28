//! Isochronous transfers (webcam video, USB audio): asynchronous libusb transfers answered from a completion callback.
//!
//! Bulk and control transfers can be done one at a time, each waiting for the device. An isochronous endpoint cannot: the device
//! sends (or wants) data on a fixed schedule, so transfers must already be queued behind one another when the previous one ends,
//! or whole stretches of the stream are lost. So each submit from the importer goes straight into libusb, in arrival order,
//! and a small thread drives libusb's event loop; the completion callback builds the reply and queues it for the importer.
use super::iso_pump::{endpoint_shape, Pump, Waiter};
use super::*;
use rusb::constants::*;
use rusb::ffi;
use std::os::raw::c_void;
use usbip_proto::{decode_iso, encode_iso, validate_iso_layout, IsoPacket, ISO_DESC_LEN};

/// Linux's usbfs refuses an isochronous transfer with more packets than this.
const MAX_PACKETS_PER_TRANSFER: usize = 128;
/// Transfers one session may have queued in libusb at once. A camera keeps a handful in flight; more means something is wrong.
const MAX_IN_FLIGHT: usize = 64;

/// `libusb_transfer` pointers are only touched under the `inflight` lock (see `on_done`), which is what makes sharing them sound.
pub(super) struct TransferPtr(pub(super) *mut ffi::libusb_transfer);
// SAFETY: the pointer is only dereferenced by libusb calls made while holding the registry lock, and the completion callback
// removes its entry (under the same lock) before the transfer is freed.
unsafe impl Send for TransferPtr {}

type Inflight = Arc<Mutex<HashMap<u32, TransferPtr>>>;

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner()) // a callback that panicked must not take every later transfer down with it
}

/// What the completion callback needs to answer one transfer; handed to libusb as `user_data` and taken back in `on_done`.
struct Ctx {
    seq: u32,
    is_in: bool,
    start_frame: i32,
    packets: Vec<IsoPacket>,
    /// The data stage libusb works on: the packets back to back, in order (the importer's own layout is in `packets`).
    buf: Vec<u8>,
    dev: Arc<RwLock<Dev>>,
    tx: UnboundedSender<Vec<u8>>,
    pending: Pending,
    sess: Arc<Session>,
    inflight: Inflight,
    cancel: Arc<AtomicBool>,
}

/// The per-session machinery: libusb's event loop on its own thread, and the registry of transfers in flight.
struct Engine {
    inflight: Inflight,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    fn new(ctx: Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("periwire-iso".into())
            .spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    let _ = ctx.handle_events(Some(Duration::from_millis(50)));
                }
            })
            .ok();
        Engine { inflight: Arc::new(Mutex::new(HashMap::new())), stop, thread }
    }

    fn cancel(&self, seq: u32) {
        if let Some(t) = lock(&self.inflight).get(&seq) {
            // SAFETY: the entry is removed under this same lock before its transfer is freed, so the pointer is live here.
            unsafe { ffi::libusb_cancel_transfer(t.0) };
        }
    }

    fn cancel_all(&self) {
        for t in lock(&self.inflight).values() {
            // SAFETY: as in `cancel`.
            unsafe { ffi::libusb_cancel_transfer(t.0) };
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One session's isochronous support. Starts nothing until the first isochronous transfer arrives.
pub(super) struct IsoPath {
    engine: Option<Engine>,
    enabled: bool,
    refused_logged: bool,
    /// Read-ahead per IN endpoint, and the interface settings the importer has selected (a read-ahead is shaped by them).
    pumps: HashMap<u8, Pump>,
    alts: HashMap<u8, u8>,
}

/// One isochronous `CMD_SUBMIT` as read from the importer: the header, any OUT data, and the raw packet descriptors.
pub(super) struct IsoSubmit<'a> {
    pub(super) c: CmdSubmit,
    pub(super) out: Vec<u8>,
    pub(super) raw_packets: Vec<u8>,
    pub(super) dev: &'a Arc<RwLock<Dev>>,
    pub(super) tx: &'a UnboundedSender<Vec<u8>>,
    pub(super) pending: &'a Pending,
    pub(super) sess: &'a Arc<Session>,
}

impl IsoPath {
    pub(super) fn new(enabled: bool) -> Self {
        IsoPath { engine: None, enabled, refused_logged: false, pumps: HashMap::new(), alts: HashMap::new() }
    }

    /// Queue the transfer, or answer it with an error right away. Never blocks.
    pub(super) fn submit(&mut self, s: IsoSubmit<'_>) {
        let np = s.c.number_of_packets.max(0) as usize;
        let packets = match decode_iso(&s.raw_packets, np).and_then(|p| validate_iso_layout(&p, s.c.transfer_buffer_length.max(0) as usize).map(|_| p)) {
            Ok(p) => p,
            Err(e) => {
                crate::logln!("session {}: dropped an isochronous transfer with bad packet descriptors: {e}", s.sess.id);
                send(s.tx, fail_reply(&s.c, &vec![IsoPacket::default(); np], EPROTO));
                return;
            }
        };
        if !self.enabled {
            if !std::mem::replace(&mut self.refused_logged, true) {
                crate::logln!("session {}: {} uses isochronous transfers (camera or audio), which are turned off on this computer (`export.isochronous` in config.json)", s.sess.id, s.sess.busid);
            }
            send(s.tx, fail_reply(&s.c, &packets, EPIPE));
            return;
        }
        if np > MAX_PACKETS_PER_TRANSFER {
            send(s.tx, fail_reply(&s.c, &packets, EPROTO)); // the operating system would refuse it anyway
            return;
        }
        let is_in = s.c.basic.direction == DIR_IN;
        let addr = (s.c.basic.ep as u8) | if is_in { 0x80 } else { 0 };
        if is_in && self.read_ahead(&s, addr, &packets) {
            return;
        }
        let engine = self.engine.get_or_insert_with(|| Engine::new(s.dev.read().unwrap().dev.context().clone()));
        if lock(&engine.inflight).len() >= MAX_IN_FLIGHT {
            send(s.tx, fail_reply(&s.c, &packets, EPROTO));
            return;
        }
        s.sess.urbs.fetch_add(1, Ordering::Relaxed);
        s.sess.rx.fetch_add(s.out.len() as u64, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        lock(s.pending).insert(s.c.basic.seqnum, cancel.clone());
        let ctx = Ctx {
            seq: s.c.basic.seqnum,
            is_in: s.c.basic.direction == DIR_IN,
            start_frame: s.c.start_frame,
            buf: lay_out(&packets, &s.out, s.c.basic.direction == DIR_IN),
            packets,
            dev: s.dev.clone(),
            tx: s.tx.clone(),
            pending: s.pending.clone(),
            sess: s.sess.clone(),
            inflight: engine.inflight.clone(),
            cancel,
        };
        let addr = (s.c.basic.ep as u8) | if ctx.is_in { 0x80 } else { 0 };
        if let Err(code) = start(ctx, addr) {
            // `start` handed the context back through the error path already; answer from what is left.
            lock(s.pending).remove(&s.c.basic.seqnum);
            let packets = decode_iso(&s.raw_packets, np).unwrap_or_default();
            trace!("isochronous submit on {addr:#04x} failed: libusb error {code}");
            send(s.tx, fail_reply(&s.c, &packets, if code == LIBUSB_ERROR_NO_DEVICE { ENODEV } else { EPROTO }));
            if code == LIBUSB_ERROR_NO_DEVICE {
                s.dev.read().unwrap().gone.store(true, Ordering::SeqCst);
                note_gone(s.dev, s.sess);
            }
        }
    }

    /// Answer an IN transfer from the endpoint's read-ahead (starting it if this is the first). False if that is not possible here.
    fn read_ahead(&mut self, s: &IsoSubmit<'_>, addr: u8, packets: &[IsoPacket]) -> bool {
        if std::env::var_os("PERIWIRE_ISO_READAHEAD").map(|v| v == "0").unwrap_or(false) {
            return false; // a switch to compare with answering transfers one at a time
        }
        if !self.pumps.contains_key(&addr) {
            let Some((len, period)) = endpoint_shape(&s.dev.read().unwrap(), addr, &self.alts) else { return false };
            match Pump::start(s.dev, addr, len, period, s.tx, s.pending, s.sess) {
                Ok(p) => {
                    self.pumps.insert(addr, p);
                }
                Err(e) => {
                    trace!("{e:#}; answering transfers one by one");
                    return false;
                }
            }
        }
        s.sess.urbs.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        lock(s.pending).insert(s.c.basic.seqnum, cancel.clone());
        self.pumps[&addr].wait_for(Waiter { seq: s.c.basic.seqnum, packets: packets.to_vec(), start_frame: s.c.start_frame, cancel });
        true
    }

    /// Note a control request on its way to the device: a change of interface setting or configuration, or a reset, ends every
    /// read-ahead (the endpoints it was shaped for are gone); the next transfer starts a new one for the new setting.
    pub(super) fn note_control(&mut self, setup: &[u8; 8]) {
        match (setup[0], setup[1]) {
            (0x01, 0x0b) => {
                self.pumps.clear();
                self.alts.insert(setup[4], setup[2]);
            }
            (0x00, 0x09) => {
                self.pumps.clear();
                self.alts.clear();
            }
            (0x23, 0x03) | (0x02, 0x01) => self.pumps.clear(),
            _ => {}
        }
    }

    /// The importer withdrew a transfer (`CMD_UNLINK`).
    pub(super) fn cancel(&self, seq: u32) {
        if let Some(e) = &self.engine {
            e.cancel(seq);
        }
    }

    /// The session is ending: cancel everything queued. The completions arrive on the event thread, which lives until `finish`.
    pub(super) fn cancel_all(&mut self) {
        self.pumps.clear(); // stops them; their libusb transfers drain on the event thread
        if let Some(e) = &self.engine {
            e.cancel_all();
        }
    }

    /// Stop the event thread. Call after every transfer's completion has run (the device's handle count is back to one).
    pub(super) fn finish(&mut self) {
        self.engine = None;
    }
}

/// The packets back to back, which is how libusb wants them; for OUT the data comes from where the importer put it.
fn lay_out(packets: &[IsoPacket], out: &[u8], is_in: bool) -> Vec<u8> {
    let total: usize = packets.iter().map(|p| p.length as usize).sum();
    let mut buf = vec![0u8; total];
    if !is_in {
        let mut at = 0usize;
        for p in packets {
            let (o, n) = (p.offset as usize, p.length as usize);
            buf[at..at + n].copy_from_slice(&out[o..o + n]); // `validate_iso_layout` already checked every packet lies inside `out`
            at += n;
        }
    }
    buf
}

/// Allocate, fill and submit the libusb transfer. On failure returns libusb's error code and has freed everything.
fn start(ctx: Ctx, addr: u8) -> std::result::Result<(), i32> {
    let np = ctx.packets.len();
    let inflight = ctx.inflight.clone();
    let seq = ctx.seq;
    // SAFETY: `t` is fresh from libusb_alloc_transfer with room for `np` packet descriptors; every field is set before the
    // submit; `ctx` is boxed so its buffer's address is stable, and it is not dropped until `on_done` (or the failure path below).
    unsafe {
        let t = ffi::libusb_alloc_transfer(np as i32);
        if t.is_null() {
            return Err(LIBUSB_ERROR_NO_MEM);
        }
        let mut boxed = Box::new(ctx);
        (*t).dev_handle = boxed.dev.read().unwrap().h.as_raw();
        (*t).flags = 0;
        (*t).endpoint = addr;
        (*t).transfer_type = LIBUSB_TRANSFER_TYPE_ISOCHRONOUS;
        (*t).timeout = 0;
        (*t).length = boxed.buf.len() as i32;
        (*t).buffer = boxed.buf.as_mut_ptr();
        (*t).num_iso_packets = np as i32;
        (*t).callback = on_done;
        let descs = (*t).iso_packet_desc.as_mut_ptr();
        for (i, p) in boxed.packets.iter().enumerate() {
            (*descs.add(i)).length = p.length;
        }
        let raw = Box::into_raw(boxed);
        (*t).user_data = raw as *mut c_void;
        // Registered before the submit: the callback may run before `libusb_submit_transfer` returns.
        lock(&inflight).insert(seq, TransferPtr(t));
        let rc = ffi::libusb_submit_transfer(t);
        if rc != 0 {
            lock(&inflight).remove(&seq);
            drop(Box::from_raw(raw));
            ffi::libusb_free_transfer(t);
            return Err(rc);
        }
    }
    Ok(())
}

extern "system" fn on_done(t: *mut ffi::libusb_transfer) {
    // A panic must not unwind into libusb's C code.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `user_data` is the `Box<Ctx>` leaked in `start`; libusb calls the callback exactly once per submit.
        let ctx: Box<Ctx> = unsafe { Box::from_raw((*t).user_data as *mut Ctx) };
        lock(&ctx.inflight).remove(&ctx.seq); // from here on `cancel` cannot reach the transfer, so freeing it below is safe
        let reply = unsafe { build_reply(&ctx, t) };
        unsafe { ffi::libusb_free_transfer(t) };
        if let Some(m) = reply {
            send(&ctx.tx, m);
        }
    }));
}

/// The libusb status of a whole transfer or one packet, as a (negative) Linux errno.
pub(super) fn errno_of(status: i32) -> i32 {
    match status {
        LIBUSB_TRANSFER_COMPLETED => 0,
        LIBUSB_TRANSFER_TIMED_OUT => ETIMEDOUT,
        LIBUSB_TRANSFER_CANCELLED => ECONNRESET,
        LIBUSB_TRANSFER_STALL => EPIPE,
        LIBUSB_TRANSFER_NO_DEVICE => ENODEV,
        LIBUSB_TRANSFER_OVERFLOW => -75,
        _ => EPROTO,
    }
}

/// # Safety
/// `t` must be the completed transfer `ctx` was submitted with, not yet freed.
unsafe fn build_reply(ctx: &Ctx, t: *mut ffi::libusb_transfer) -> Option<Vec<u8>> {
    let status = (*t).status;
    if status == LIBUSB_TRANSFER_NO_DEVICE {
        ctx.dev.read().unwrap().gone.store(true, Ordering::SeqCst);
        note_gone(&ctx.dev, &ctx.sess);
    }
    let live = lock(&ctx.pending).remove(&ctx.seq).is_some();
    if ctx.cancel.load(Ordering::Relaxed) || !live {
        return None; // unlinked or the session is over: the importer is not waiting for this one
    }
    let np = ctx.packets.len();
    let descs = (*t).iso_packet_desc.as_ptr();
    let mut done = Vec::with_capacity(np);
    let (mut cursor, mut errors) = (0usize, 0i32);
    // The transfer itself only fails as a whole when libusb could not run it at all; a packet that went wrong says so in its own status.
    let whole = if status == LIBUSB_TRANSFER_COMPLETED { 0 } else { errno_of(status) };
    let mut compact = vec![];
    for (i, p) in ctx.packets.iter().enumerate() {
        let d = &*descs.add(i);
        let st = if whole != 0 { whole } else { errno_of(d.status) };
        let actual = if st == 0 || ctx.is_in { d.actual_length.min(p.length) } else { 0 };
        if st != 0 {
            errors += 1;
        }
        if ctx.is_in && whole == 0 {
            compact.extend_from_slice(&ctx.buf[cursor..cursor + actual as usize]);
        }
        done.push(IsoPacket { offset: p.offset, length: p.length, actual_length: actual, status: st });
        cursor += p.length as usize;
    }
    let moved: u32 = done.iter().map(|p| p.actual_length).sum();
    let ret = RetSubmit {
        basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, seqnum: ctx.seq },
        status: whole,
        actual_length: if whole != 0 {
            0
        } else if ctx.is_in {
            compact.len() as i32
        } else {
            moved as i32
        },
        start_frame: ctx.start_frame,
        number_of_packets: np as i32,
        error_count: if whole != 0 { np as i32 } else { errors },
    };
    ctx.sess.tx.fetch_add(compact.len() as u64, Ordering::Relaxed);
    trace!("iso done seq={} packets={} bytes={} errors={} status={}", ctx.seq, np, moved, ret.error_count, whole);
    let mut m = ret.encode();
    m.extend_from_slice(&compact);
    encode_iso(&done, &mut m);
    Some(m)
}

/// A transfer that could not be run: every packet reports `status`, nothing moved.
pub(super) fn fail_reply(c: &CmdSubmit, packets: &[IsoPacket], status: i32) -> Vec<u8> {
    fail_reply_for(c.basic.seqnum, c.start_frame, packets, status)
}

pub(super) fn fail_reply_for(seq: u32, start_frame: i32, packets: &[IsoPacket], status: i32) -> Vec<u8> {
    let ret = RetSubmit {
        basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, seqnum: seq },
        status,
        actual_length: 0,
        start_frame,
        number_of_packets: packets.len() as i32,
        error_count: packets.len() as i32,
    };
    let mut m = ret.encode();
    let failed: Vec<IsoPacket> = packets.iter().map(|p| IsoPacket { actual_length: 0, status, ..*p }).collect();
    encode_iso(&failed, &mut m);
    debug_assert_eq!(m.len(), URB_HEADER_LEN + packets.len() * ISO_DESC_LEN);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(offset: u32, length: u32) -> IsoPacket {
        IsoPacket { offset, length, actual_length: 0, status: 0 }
    }

    #[test]
    fn out_data_is_laid_end_to_end_for_libusb_whatever_offsets_the_importer_used() {
        // The importer left a gap between the packets; libusb wants them back to back.
        let out: Vec<u8> = (0..20).collect();
        let packets = [pk(0, 4), pk(10, 4), pk(16, 4)];
        assert_eq!(lay_out(&packets, &out, false), vec![0, 1, 2, 3, 10, 11, 12, 13, 16, 17, 18, 19]);
        assert_eq!(lay_out(&packets, &[], true), vec![0u8; 12], "an IN transfer starts with an empty buffer of the right size");
    }

    #[test]
    fn libusb_statuses_become_the_errors_a_usb_stack_expects() {
        assert_eq!(errno_of(LIBUSB_TRANSFER_COMPLETED), 0);
        assert_eq!(errno_of(LIBUSB_TRANSFER_STALL), EPIPE);
        assert_eq!(errno_of(LIBUSB_TRANSFER_NO_DEVICE), ENODEV);
        assert_eq!(errno_of(LIBUSB_TRANSFER_TIMED_OUT), ETIMEDOUT);
        assert_eq!(errno_of(LIBUSB_TRANSFER_ERROR), EPROTO);
    }

    #[test]
    fn a_failed_transfer_answers_every_packet_and_has_exactly_the_length_the_importer_will_read() {
        let c = CmdSubmit {
            basic: UrbBasic { command: USBIP_CMD_SUBMIT, seqnum: 9, devid: 0, direction: DIR_IN, ep: 1 },
            transfer_flags: 0,
            transfer_buffer_length: 96,
            start_frame: 5,
            number_of_packets: 3,
            interval: 1,
            setup: [0; 8],
        };
        let m = fail_reply(&c, &[pk(0, 32), pk(32, 32), pk(64, 32)], EPIPE);
        assert_eq!(m.len(), URB_HEADER_LEN + 3 * ISO_DESC_LEN);
        let r = RetSubmit::decode(&m[..URB_HEADER_LEN]).unwrap();
        assert_eq!((r.status, r.actual_length, r.number_of_packets, r.error_count), (EPIPE, 0, 3, 3));
        let d = decode_iso(&m[URB_HEADER_LEN..], 3).unwrap();
        assert!(d.iter().all(|p| p.status == EPIPE && p.actual_length == 0));
        assert_eq!(d[1].offset, 32);
    }
}
