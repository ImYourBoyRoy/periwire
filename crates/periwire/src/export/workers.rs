//! Threads that run URBs for a session: the storage command worker, per-endpoint ordering, and reply building.
use super::*;

pub(super) struct BotJob {
    pub(super) c: CmdSubmit,
    pub(super) out: Vec<u8>,
    pub(super) cancel: Arc<AtomicBool>,
}

/// Read the status block that follows a finished data stage. `None` leaves it for the host to read the ordinary way
/// (nothing was consumed unless the device returned something unexpected).
pub(super) fn read_csw(dev: &RwLock<Dev>, ep_in: u8, cancel: &AtomicBool) -> Option<[u8; CSW_LEN]> {
    let start = std::time::Instant::now();
    let mut buf = [0u8; CSW_LEN];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let r = dev.read().unwrap().h.read_bulk(ep_in, &mut buf, Duration::from_millis(500));
        match r {
            Ok(CSW_LEN) if buf.starts_with(b"USBS") => return Some(buf),
            Ok(n) => {
                trace!("status block read returned {n} unexpected bytes; leaving recovery to the host");
                return None;
            }
            Err(rusb::Error::Timeout) if start.elapsed() < Duration::from_secs(5) => {}
            Err(e) => {
                trace!("status block read failed ({e}); the host will read it itself");
                return None;
            }
        }
    }
}

/// One URB waiting to run.
pub(super) struct UrbJob {
    pub(super) c: CmdSubmit,
    pub(super) out: Vec<u8>,
    pub(super) cancel: Arc<AtomicBool>,
}

/// Runs the jobs of each key one after another, in the order they were given, on a worker thread per key; different keys run side by
/// side. An importer that keeps several transfers in flight on one endpoint (the macOS one reads ahead) needs them to reach the
/// device in the order it queued them: on separate threads the second could overtake the first and the data would arrive swapped.
pub(super) struct Ordered<K, J> {
    pub(super) queues: HashMap<K, UnboundedSender<J>>,
    pub(super) run: Arc<dyn Fn(J) + Send + Sync>,
}

impl<K: std::hash::Hash + Eq, J: Send + 'static> Ordered<K, J> {
    pub(super) fn new(run: Arc<dyn Fn(J) + Send + Sync>) -> Self {
        Ordered { queues: HashMap::new(), run }
    }

    /// False if the worker is gone.
    pub(super) fn send(&mut self, key: K, job: J) -> bool {
        let run = self.run.clone();
        self.queues
            .entry(key)
            .or_insert_with(|| {
                let (tx, mut rx) = unbounded_channel::<J>();
                tokio::task::spawn_blocking(move || {
                    while let Some(j) = rx.blocking_recv() {
                        run(j);
                    }
                });
                tx
            })
            .send(job)
            .is_ok()
    }
}

/// Runs a storage device's bulk transfers in submission order. A Bulk-Only command is strictly sequential anyway; doing it
/// on one thread lets the last data transfer's reply carry the status block (see `accel`).
pub(super) fn bot_worker(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<BotJob>,
    bot: Bot,
    dev: Arc<RwLock<Dev>>,
    tx: UnboundedSender<Vec<u8>>,
    pending: Pending,
    sess: Arc<Session>,
    guard: Option<Arc<Mutex<Guard>>>,
) {
    let mut seq = Seq::default();
    // The status block last sent along with a data reply. Some hosts (Windows) queue the status read right behind the data
    // read instead of waiting for it, so it can reach us after the device has nothing left to say: answer it from here.
    let mut delivered: Option<[u8; CSW_LEN]> = None;
    while let Some(BotJob { c, out, cancel }) = rx.blocking_recv() {
        let (is_in, ep, len) = (c.basic.direction == DIR_IN, c.basic.ep as u8, c.transfer_buffer_length.max(0) as usize);
        if cancel.load(Ordering::Relaxed) {
            pending.lock().unwrap().remove(&c.basic.seqnum);
            continue;
        }
        let step = if ep == 0 {
            seq.control(&c.setup);
            Step::Other
        } else {
            seq.begin(is_in, ep, &out, bot.ep_in & 0x0f, bot.ep_out & 0x0f)
        };
        let repeat = if is_in && ep == bot.ep_in & 0x0f && len >= CSW_LEN && step == Step::Other {
            delivered.take()
        } else {
            delivered = None;
            None
        };
        if let Some(csw) = repeat {
            let live = pending.lock().unwrap().remove(&c.basic.seqnum).is_some();
            trace!("done   seq={} status=0 bytes={} live={} step=Status (answered from the block already sent)", c.basic.seqnum, CSW_LEN, live);
            if !cancel.load(Ordering::Relaxed) && live {
                sess.tx.fetch_add(CSW_LEN as u64, Ordering::Relaxed);
                send(&tx, make_ret(&c, 0, &csw));
            }
            continue;
        }
        let (status, mut data) = if step == Step::Fail { (EPIPE, vec![]) } else { run_urb(&dev, &c, out, &cancel) };
        note_gone(&dev, &sess);
        if let Some(g) = &guard {
            g.lock().unwrap().on_complete(ep, is_in, &mut data);
        }
        let ok = if is_in { status == 0 } else { status >= 0 };
        let moved = if !ok {
            0
        } else if is_in {
            data.len()
        } else {
            status as usize
        };
        let csw = if seq.finish(step, ok, moved, len) { read_csw(&dev, bot.ep_in, &cancel) } else { None };
        let live = pending.lock().unwrap().remove(&c.basic.seqnum).is_some();
        trace!("done   seq={} status={} bytes={} live={} step={:?} csw={}", c.basic.seqnum, status, data.len(), live, step, csw.is_some());
        if cancel.load(Ordering::Relaxed) || !live {
            continue;
        }
        delivered = csw;
        sess.tx.fetch_add((data.len() + if csw.is_some() { CSW_LEN } else { 0 }) as u64, Ordering::Relaxed);
        let mut m = make_ret(&c, status, &data);
        if let Some(csw) = csw {
            m[40..44].copy_from_slice(&csw_mark(bot.ep_in));
            m.extend_from_slice(&csw);
        }
        send(&tx, m);
    }
}

/// Build the RET_SUBMIT for a URB: `status` is bytes written (OUT bulk), or the IN data length is `data.len()`.
pub(super) fn make_ret(c: &CmdSubmit, status: i32, data: &[u8]) -> Vec<u8> {
    let is_in = c.basic.direction == DIR_IN;
    let (st, actual) = if is_in || c.basic.ep == 0 {
        (if status < 0 { status } else { 0 }, data.len() as i32)
    } else if status >= 0 {
        (0, status)
    } else {
        (status, 0)
    };
    let ret = RetSubmit {
        basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, seqnum: c.basic.seqnum },
        status: st,
        actual_length: actual,
        start_frame: 0,
        number_of_packets: 0,
        error_count: 0,
    };
    let mut m = ret.encode();
    if (is_in || c.basic.ep == 0) && st == 0 && c.basic.direction == DIR_IN {
        m.extend_from_slice(data);
    }
    m
}
