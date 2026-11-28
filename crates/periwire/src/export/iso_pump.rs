//! Read-ahead for isochronous IN endpoints (camera video, microphones).
//!
//! A device sends on a fixed schedule whether or not anyone is listening. When each transfer is queued only after the importer
//! asks for it (see `iso`), the device's pipe has data to fill only the few transfers the importer has in flight: about 20 ms
//! for a typical webcam. A network round trip longer than that leaves the pipe with no transfer to fill, and the data of those
//! milliseconds is lost: at 10 ms of round trip plus jitter, 640x480 video was down to a frame a second.
//!
//! So the exporter keeps its own transfers queued on the endpoint, always, and holds what they receive in a short queue of
//! packets. A transfer from the importer is answered from that queue as soon as it holds enough packets (a transfer's packets
//! are consecutive service intervals, so the queue length is time). The importer still sees an ordinary stream; what changed is
//! that a late request no longer loses data, it receives older data, up to the queue's bound (a quarter of a second).
use super::iso::{errno_of, lock, TransferPtr};
use super::*;
use rusb::constants::*;
use rusb::ffi;
use std::collections::VecDeque;
use std::os::raw::c_void;
use usbip_proto::{encode_iso, IsoPacket};

/// Roughly how long one read-ahead transfer covers, how much of the stream is kept queued on the device, and how much the queue may hold.
const TRANSFER_US: u32 = 8_000;
const AHEAD_US: u32 = 64_000;
const QUEUE_US: u32 = 250_000;
/// libusb/usbfs refuse more packets than this in one isochronous transfer.
const MAX_PACKETS: u32 = 32;
/// In a row: transfers that failed as a whole before the endpoint is given up on.
const MAX_FAILURES: u32 = 20;

/// One service interval's worth of data from the device.
struct Slot {
    data: Vec<u8>,
    status: i32,
}

/// An importer transfer waiting for its packets.
pub(super) struct Waiter {
    pub(super) seq: u32,
    pub(super) packets: Vec<IsoPacket>,
    pub(super) start_frame: i32,
    pub(super) cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Slot>,
    waiters: VecDeque<Waiter>,
    dropped: u64,
}

/// Where answers go: the importer's writer queue and the table of its transfers still waiting for an answer.
struct Sink {
    tx: UnboundedSender<Vec<u8>>,
    pending: Pending,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
    /// Transfers libusb owns; a callback removes its entry (under this lock) before freeing.
    transfers: Mutex<Vec<TransferPtr>>,
    cap: usize,
    sink: Sink,
    sess: Arc<Session>,
    dev: Arc<RwLock<Dev>>,
    failures: std::sync::atomic::AtomicU32,
}

/// What a read-ahead transfer needs when libusb calls back.
struct Ctx {
    shared: Arc<Shared>,
    buf: Vec<u8>,
    packets: usize,
    packet_len: usize,
}

pub(super) struct Pump {
    shared: Arc<Shared>,
}

/// Size and period of an isochronous endpoint in the interface setting that is active now: (bytes per interval, microseconds between intervals).
pub(super) fn endpoint_shape(dev: &Dev, addr: u8, alts: &HashMap<u8, u8>) -> Option<(u32, u32)> {
    let cfg = dev.dev.active_config_descriptor().ok()?;
    let base_us = match dev.dev.speed() {
        rusb::Speed::Low | rusb::Speed::Full => 1000u32,
        _ => 125,
    };
    for iface in cfg.interfaces() {
        let want = alts.get(&iface.number()).copied().unwrap_or(0);
        for alt in iface.descriptors().filter(|a| a.setting_number() == want) {
            if let Some(ep) = alt.endpoint_descriptors().find(|e| e.address() == addr && e.transfer_type() == TransferType::Isochronous) {
                let raw = ep.max_packet_size();
                let len = u32::from(raw & 0x7ff) * (1 + u32::from((raw >> 11) & 3));
                let period = base_us << (ep.interval().clamp(1, 16) - 1);
                return (len > 0).then_some((len, period));
            }
        }
    }
    None
}

impl Pump {
    /// Start reading `addr` ahead. Fails (so the caller falls back to answering transfers one by one) if the endpoint cannot be set up.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(dev: &Arc<RwLock<Dev>>, addr: u8, packet_len: u32, period_us: u32, tx: &UnboundedSender<Vec<u8>>, pending: &Pending, sess: &Arc<Session>) -> Result<Pump> {
        let packets = (TRANSFER_US / period_us).clamp(1, MAX_PACKETS);
        let depth = (AHEAD_US / (packets * period_us)).max(2) + 1;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            stop: AtomicBool::new(false),
            transfers: Mutex::new(vec![]),
            cap: ((QUEUE_US / period_us) as usize).max(packets as usize * 2),
            sink: Sink { tx: tx.clone(), pending: pending.clone() },
            sess: sess.clone(),
            dev: dev.clone(),
            failures: Default::default(),
        });
        let pump = Pump { shared };
        for _ in 0..depth {
            if let Err(code) = submit_new(&pump.shared, addr, packets as usize, packet_len as usize) {
                pump.stop();
                bail!("could not start reading endpoint {addr:#04x} ahead (libusb error {code})");
            }
        }
        crate::logln!("session {}: reading {} ahead ({} transfers of {} packets, up to {} ms queued)", sess.id, sess.busid, depth, packets, QUEUE_US / 1000);
        Ok(pump)
    }

    /// An importer transfer wants `packets`; answer it now if the queue already holds them, else when it does.
    pub(super) fn wait_for(&self, w: Waiter) {
        let mut st = lock(&self.shared.state);
        if w.packets.len() > self.shared.cap {
            drop(st);
            send(&self.shared.sink.tx, super::iso::fail_reply_for(w.seq, w.start_frame, &w.packets, EPROTO));
            lock(&self.shared.sink.pending).remove(&w.seq);
            return;
        }
        st.waiters.push_back(w);
        let sent = deliver(&self.shared.sink, &mut st);
        self.shared.sess.tx.fetch_add(sent, Ordering::Relaxed);
    }

    pub(super) fn stop(&self) {
        let list = lock(&self.shared.transfers);
        self.shared.stop.store(true, Ordering::SeqCst); // under the lock: a callback deciding whether to resubmit sees it
        for t in list.iter() {
            // SAFETY: entries are removed under this lock before their transfer is freed, so each pointer is live here.
            unsafe { ffi::libusb_cancel_transfer(t.0) };
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop();
        let (kept, dropped) = {
            let st = lock(&self.shared.state);
            (st.queue.len(), st.dropped)
        };
        if dropped > 0 {
            trace!("read-ahead ended with {kept} packets queued, {dropped} dropped because the importer fell behind");
        }
    }
}

fn submit_new(shared: &Arc<Shared>, addr: u8, packets: usize, packet_len: usize) -> std::result::Result<(), i32> {
    // SAFETY: as in `iso::start`: a fresh transfer, every field set before the submit; the boxed context is leaked to libusb and taken
    // back in `on_done`, which either resubmits it or frees everything.
    unsafe {
        let t = ffi::libusb_alloc_transfer(packets as i32);
        if t.is_null() {
            return Err(LIBUSB_ERROR_NO_MEM);
        }
        let mut ctx = Box::new(Ctx { shared: shared.clone(), buf: vec![0u8; packets * packet_len], packets, packet_len });
        (*t).dev_handle = shared.dev.read().unwrap().h.as_raw();
        (*t).flags = 0;
        (*t).endpoint = addr;
        (*t).transfer_type = LIBUSB_TRANSFER_TYPE_ISOCHRONOUS;
        (*t).timeout = 0;
        (*t).length = ctx.buf.len() as i32;
        (*t).buffer = ctx.buf.as_mut_ptr();
        (*t).num_iso_packets = packets as i32;
        (*t).callback = on_done;
        let descs = (*t).iso_packet_desc.as_mut_ptr();
        for i in 0..packets {
            (*descs.add(i)).length = packet_len as u32;
        }
        let raw = Box::into_raw(ctx);
        (*t).user_data = raw as *mut c_void;
        let mut list = lock(&shared.transfers);
        let rc = ffi::libusb_submit_transfer(t);
        if rc != 0 {
            drop(list);
            drop(Box::from_raw(raw));
            ffi::libusb_free_transfer(t);
            return Err(rc);
        }
        list.push(TransferPtr(t));
    }
    Ok(())
}

extern "system" fn on_done(t: *mut ffi::libusb_transfer) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `user_data` is the leaked `Box<Ctx>` of this transfer; libusb calls this once per submit.
        let ctx: Box<Ctx> = unsafe { Box::from_raw((*t).user_data as *mut Ctx) };
        let shared = ctx.shared.clone();
        let status = unsafe { (*t).status };
        if status == LIBUSB_TRANSFER_NO_DEVICE {
            shared.dev.read().unwrap().gone.store(true, Ordering::SeqCst);
            note_gone(&shared.dev, &shared.sess);
        }
        if !shared.stop.load(Ordering::SeqCst) {
            let mut st = lock(&shared.state);
            unsafe { collect(&shared, &mut st, &ctx, t) };
            let sent = deliver(&shared.sink, &mut st);
            shared.sess.tx.fetch_add(sent, Ordering::Relaxed);
        }
        // Again, or done with it. The decision and the resubmit are made under the registry lock, which `stop` also takes.
        let mut list = lock(&shared.transfers);
        let broken = shared.failures.load(Ordering::Relaxed) >= MAX_FAILURES || status == LIBUSB_TRANSFER_NO_DEVICE;
        if !shared.stop.load(Ordering::SeqCst) && !broken {
            let raw = Box::into_raw(ctx);
            // SAFETY: the transfer is complete and ours again; `raw` stays valid until the next callback.
            let rc = unsafe {
                (*t).user_data = raw as *mut c_void;
                (*t).status = 0;
                ffi::libusb_submit_transfer(t)
            };
            if rc == 0 {
                return;
            }
            drop(unsafe { Box::from_raw(raw) });
        }
        list.retain(|p| p.0 != t);
        let last = list.is_empty();
        drop(list);
        // SAFETY: removed from the registry above, so no `stop` can reach it any more.
        unsafe { ffi::libusb_free_transfer(t) };
        if last && !shared.stop.load(Ordering::SeqCst) {
            // Every transfer is gone but nobody asked to stop: the endpoint failed. Tell the importer's waiting transfers.
            shared.stop.store(true, Ordering::SeqCst);
            crate::logln!("session {}: reading {} ahead stopped (the device stopped answering on that endpoint)", shared.sess.id, shared.sess.busid);
            fail_waiters(&shared);
        }
    }));
}

/// Move what a finished read-ahead transfer received into the queue, oldest data dropped if the importer has fallen far behind.
///
/// # Safety
/// `t` is the completed transfer `ctx` belongs to.
unsafe fn collect(shared: &Shared, st: &mut State, ctx: &Ctx, t: *mut ffi::libusb_transfer) {
    let whole = (*t).status;
    let descs = (*t).iso_packet_desc.as_ptr();
    if whole != LIBUSB_TRANSFER_COMPLETED && whole != LIBUSB_TRANSFER_ERROR {
        shared.failures.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.failures.store(0, Ordering::Relaxed);
    }
    for i in 0..ctx.packets {
        let d = &*descs.add(i);
        let (status, n) = if whole == LIBUSB_TRANSFER_COMPLETED { (errno_of(d.status), (d.actual_length as usize).min(ctx.packet_len)) } else { (errno_of(whole), 0) };
        let from = i * ctx.packet_len;
        if st.queue.len() >= shared.cap {
            st.queue.pop_front();
            st.dropped += 1;
        }
        st.queue.push_back(Slot { data: ctx.buf[from..from + n].to_vec(), status });
    }
}

/// Answer waiting importer transfers, oldest first, while the queue holds enough packets for the first one. Returns the bytes sent.
fn deliver(sink: &Sink, st: &mut State) -> u64 {
    let mut total = 0u64;
    loop {
        let Some(front) = st.waiters.front() else { return total };
        if front.cancel.load(Ordering::Relaxed) {
            st.waiters.pop_front(); // unlinked while it waited
            continue;
        }
        if st.queue.len() < front.packets.len() {
            return total;
        }
        let w = st.waiters.pop_front().unwrap();
        if lock(&sink.pending).remove(&w.seq).is_none() {
            continue; // withdrawn: nothing is waiting for it
        }
        let mut data = vec![];
        let (mut done, mut errors) = (Vec::with_capacity(w.packets.len()), 0i32);
        for p in &w.packets {
            let slot = st.queue.pop_front().expect("length checked above");
            let overflow = slot.data.len() > p.length as usize;
            let n = slot.data.len().min(p.length as usize);
            let status = if overflow { -75 } else { slot.status };
            if status != 0 {
                errors += 1;
            }
            data.extend_from_slice(&slot.data[..n]);
            done.push(IsoPacket { offset: p.offset, length: p.length, actual_length: n as u32, status });
        }
        let ret = RetSubmit {
            basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, seqnum: w.seq },
            status: 0,
            actual_length: data.len() as i32,
            start_frame: w.start_frame,
            number_of_packets: done.len() as i32,
            error_count: errors,
        };
        total += data.len() as u64;
        let mut m = ret.encode();
        m.extend_from_slice(&data);
        encode_iso(&done, &mut m);
        send(&sink.tx, m);
    }
}

fn fail_waiters(shared: &Shared) {
    let mut st = lock(&shared.state);
    while let Some(w) = st.waiters.pop_front() {
        if lock(&shared.sink.pending).remove(&w.seq).is_some() && !w.cancel.load(Ordering::Relaxed) {
            send(&shared.sink.tx, super::iso::fail_reply_for(w.seq, w.start_frame, &w.packets, EIO));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_for_test() -> (Sink, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = unbounded_channel();
        (Sink { tx, pending: Arc::new(Mutex::new(HashMap::new())) }, rx)
    }

    fn waiter(seq: u32, n: u32, len: u32) -> (Waiter, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let packets = (0..n).map(|i| IsoPacket { offset: i * len, length: len, actual_length: 0, status: 0 }).collect();
        (Waiter { seq, packets, start_frame: 0, cancel: cancel.clone() }, cancel)
    }

    fn slot(n: usize, status: i32) -> Slot {
        Slot { data: vec![7u8; n], status }
    }

    #[test]
    fn a_transfer_is_answered_when_the_queue_holds_its_packets_and_not_before() {
        let (sh, mut rx) = shared_for_test();
        let (w, _) = waiter(1, 4, 100);
        lock(&sh.pending).insert(1, Arc::new(AtomicBool::new(false)));
        let mut st = State::default();
        st.waiters.push_back(w);
        st.queue.extend((0..3).map(|_| slot(100, 0)));
        deliver(&sh, &mut st);
        assert!(rx.try_recv().is_err(), "three of four packets: it keeps waiting");
        st.queue.push_back(slot(60, 0));
        deliver(&sh, &mut st);
        let m = rx.try_recv().expect("answered once the fourth packet arrived");
        let r = RetSubmit::decode(&m[..URB_HEADER_LEN]).unwrap();
        assert_eq!((r.status, r.actual_length, r.number_of_packets, r.error_count), (0, 360, 4, 0));
        let d = usbip_proto::decode_iso(&m[URB_HEADER_LEN + 360..], 4).unwrap();
        assert_eq!(d.iter().map(|p| p.actual_length).collect::<Vec<_>>(), vec![100, 100, 100, 60]);
        assert!(st.queue.is_empty() && st.waiters.is_empty());
    }

    #[test]
    fn data_that_arrived_before_the_request_is_not_lost() {
        let (sh, mut rx) = shared_for_test();
        let mut st = State::default();
        st.queue.extend((0..10).map(|_| slot(50, 0)));
        let (w, _) = waiter(5, 4, 100);
        lock(&sh.pending).insert(5, Arc::new(AtomicBool::new(false)));
        st.waiters.push_back(w);
        deliver(&sh, &mut st);
        assert!(rx.try_recv().is_ok(), "answered at once from the queue");
        assert_eq!(st.queue.len(), 6, "and only the packets it needed were taken");
    }

    #[test]
    fn a_withdrawn_transfer_takes_nothing_and_does_not_block_the_next() {
        let (sh, mut rx) = shared_for_test();
        let mut st = State::default();
        let (w1, cancel1) = waiter(1, 2, 10);
        let (w2, _) = waiter(2, 2, 10);
        lock(&sh.pending).insert(1, cancel1.clone());
        lock(&sh.pending).insert(2, Arc::new(AtomicBool::new(false)));
        cancel1.store(true, Ordering::Relaxed); // the importer unlinked the first while it waited
        st.waiters.extend([w1, w2]);
        st.queue.extend((0..2).map(|_| slot(10, 0)));
        deliver(&sh, &mut st);
        let m = rx.try_recv().expect("the second transfer is answered");
        assert_eq!(RetSubmit::decode(&m[..URB_HEADER_LEN]).unwrap().basic.seqnum, 2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_packet_larger_than_the_transfer_allows_is_reported_as_overflow_and_truncated() {
        let (sh, mut rx) = shared_for_test();
        let mut st = State::default();
        st.queue.push_back(slot(120, 0));
        let (w, _) = waiter(9, 1, 100);
        lock(&sh.pending).insert(9, Arc::new(AtomicBool::new(false)));
        st.waiters.push_back(w);
        deliver(&sh, &mut st);
        let m = rx.try_recv().unwrap();
        let r = RetSubmit::decode(&m[..URB_HEADER_LEN]).unwrap();
        assert_eq!((r.actual_length, r.error_count), (100, 1));
    }
}
