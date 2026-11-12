//! Fewer network round trips for USB mass storage (`FLAG_ACCEL`).
//!
//! USB mass storage (Bulk-Only Transport) runs every SCSI command as three strictly sequential USB transfers: a command
//! block (CBW, host to device), an optional data stage, and a status block (CSW, device to host). Forwarded naively,
//! that is three network round trips per command, and the operating system will not start the next transfer before the
//! previous one has completed. On a 15 ms Wi-Fi link that capped reads at about 3 MB/s.
//!
//! Two safe shortcuts bring it down to one round trip:
//!
//! 1. **The importer completes the command-block transfer itself** and forwards it anyway. The exporter performs it, in
//!    order, before anything that follows. Its real result is checked when it arrives (see `ret`); if it ever fails, the
//!    exporter fails the next transfer so the host's normal reset recovery runs.
//! 2. **The exporter appends the status block to the reply of the last data transfer.** The importer keeps it and answers
//!    the host's status-block read from memory.
//!
//! Nothing here changes what the host sees except time. It is used only when both sides agreed to `FLAG_ACCEL`, and the
//! pieces that cannot be predicted (short data, stalls, errors, non-storage devices) take the ordinary path.
//!
//! `Accel` is the importer's bookkeeping, as a plain state machine so it can be tested without a network. `relay` (below)
//! applies it to a byte stream for the importers that hand the operating system a socket (Linux, Windows); the macOS
//! importer talks to its host controller directly and uses `Accel` from `urb.rs`.
#![cfg_attr(target_os = "macos", allow(dead_code))] // the byte-stream relay below serves Linux and Windows; macOS drives `Accel` directly
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::unbounded_channel;
use usbip_proto::*;
pub const MAX_URB_BYTES: i32 = 16 * 1024 * 1024;

/// What to do with a URB the host's USB stack just submitted.
#[derive(Debug, PartialEq)]
pub enum Submit {
    /// Send it to the exporter and wait for its reply.
    Forward,
    /// Send it to the exporter AND complete it to the host right now (a command-block write).
    Ghost,
    /// Do not send it: complete it to the host with this status block (it was delivered ahead of time).
    Csw([u8; CSW_LEN]),
}

#[derive(Default)]
pub struct Accel {
    on: bool,
    /// Transfers already completed to the host whose real reply is still to come (and must be swallowed).
    ghosts: HashSet<u32>,
    /// Status block delivered ahead of time, with the endpoint number it belongs to.
    csw: Option<(u8, [u8; CSW_LEN])>,
    /// Endpoint numbers of the storage interface, learned from command blocks and markers.
    ep_out: Option<u8>,
}

impl Accel {
    pub fn new(on: bool) -> Self {
        Accel { on, ..Default::default() }
    }

    /// A URB from the host. `ep` is the endpoint number (no direction bit), `payload` the OUT data.
    pub fn submit(&mut self, seq: u32, ep: u8, is_in: bool, len: usize, payload: &[u8], iso: bool) -> Submit {
        if !self.on || iso {
            return Submit::Forward;
        }
        if ep == 0 {
            self.csw = None; // control transfers include the reset recovery: a stale status block must not survive them
            return Submit::Forward;
        }
        if !is_in && len == 31 && payload.starts_with(b"USBC") {
            self.csw = None; // a new command: whatever was left over is gone
            self.ep_out = Some(ep);
            self.ghosts.insert(seq);
            return Submit::Ghost;
        }
        if let Some((cep, c)) = self.csw {
            // Linux asks for exactly 13 bytes; Windows asks for a whole 512-byte packet. Both get the 13 the device would send.
            if is_in && ep == cep && len >= CSW_LEN {
                self.csw = None;
                return Submit::Csw(c);
            }
            if ep == cep || Some(ep) == self.ep_out {
                self.csw = None; // other traffic on the storage endpoints: the host is not reading the status in the usual way
            }
        }
        Submit::Forward
    }

    /// The exporter's reply to `seq` arrived with this status. Returns true when the host was already told (swallow it).
    pub fn ret(&mut self, seq: u32) -> bool {
        self.ghosts.remove(&seq)
    }

    /// The exporter delivered a status block with a data reply.
    pub fn store_csw(&mut self, ep: u8, csw: [u8; CSW_LEN]) {
        self.csw = Some((ep, csw));
    }

    /// A cancelled transfer will never get a reply.
    pub fn forget(&mut self, seq: u32) {
        self.ghosts.remove(&seq);
    }
}

/// What a bulk transfer on the storage endpoints is, as seen by the exporter's ordered worker.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Step {
    /// The previous command block never reached the device: fail this transfer without touching it, so the host's
    /// reset recovery runs (it already believes the command block was delivered).
    Fail,
    Cbw { data_len: u32, dir_in: bool },
    /// The data stage of the current command.
    Data,
    Other,
}

struct Cmd {
    data_len: u32,
    dir_in: bool,
    got: u32,
}

/// The exporter's view of one Bulk-Only command at a time. Decides when the status block can ride along with a data reply.
#[derive(Default)]
pub struct Seq {
    cmd: Option<Cmd>,
    failed: bool,
}

impl Seq {
    /// A control transfer ran in order with the storage transfers. Reset recovery (the class reset, or clearing a halted
    /// endpoint) ends any "the command block never arrived" state; other requests (descriptor reads and so on) leave it.
    pub fn control(&mut self, setup: &[u8; 8]) {
        let class_reset = setup[0] == 0x21 && setup[1] == 0xFF;
        let clear_halt = setup[0] == 0x02 && setup[1] == 0x01;
        if class_reset || clear_halt {
            self.failed = false;
            self.cmd = None;
        }
    }

    /// Classify a transfer about to run. `ep` is the endpoint number; `ep_in`/`ep_out` are the storage endpoint numbers.
    pub fn begin(&mut self, is_in: bool, ep: u8, out: &[u8], ep_in: u8, ep_out: u8) -> Step {
        let is_cbw = !is_in && out.len() == 31 && out.starts_with(b"USBC");
        if self.failed && !is_cbw {
            self.failed = false;
            self.cmd = None;
            return Step::Fail;
        }
        if is_cbw {
            let data_len = u32::from_le_bytes([out[8], out[9], out[10], out[11]]);
            return Step::Cbw { data_len, dir_in: out[12] & 0x80 != 0 };
        }
        if let Some(c) = &self.cmd {
            if c.data_len > 0 && c.got < c.data_len && ((is_in && c.dir_in && ep == ep_in) || (!is_in && !c.dir_in && ep == ep_out)) {
                return Step::Data;
            }
        }
        Step::Other
    }

    /// The transfer finished. `ok`: it succeeded; `moved`: bytes transferred; `len`: bytes it was asked to move.
    /// Returns true when the status block should be read now and attached to this transfer's reply.
    pub fn finish(&mut self, step: Step, ok: bool, moved: usize, len: usize) -> bool {
        match step {
            Step::Cbw { data_len, dir_in } => {
                if ok {
                    self.cmd = Some(Cmd { data_len, dir_in, got: 0 });
                } else {
                    self.cmd = None;
                    self.failed = true;
                }
                false
            }
            Step::Data => {
                let Some(c) = self.cmd.as_mut() else { return false };
                if !ok {
                    self.cmd = None;
                    return false;
                }
                c.got = c.got.saturating_add(moved as u32);
                let last = moved == len && c.got >= c.data_len;
                if last || moved < len {
                    self.cmd = None;
                }
                last
            }
            _ => false,
        }
    }
}

/// A complete `RET_SUBMIT` for the host: `data` follows the header for IN transfers.
pub fn ret_submit(seq: u32, status: i32, actual: usize, data: &[u8]) -> Vec<u8> {
    let r = RetSubmit { basic: UrbBasic { command: USBIP_RET_SUBMIT, seqnum: seq, devid: 0, direction: 0, ep: 0 }, status, actual_length: actual as i32, start_frame: 0, number_of_packets: 0, error_count: 0 };
    let mut m = r.encode();
    m.extend_from_slice(data);
    m
}

struct Inflight {
    is_in: bool,
}

struct State {
    accel: Accel,
    inflight: HashMap<u32, Inflight>,
    unlinks: HashMap<u32, u32>,
}

/// Which side ended the relay, and how.
pub enum Ended {
    /// The host's USB stack closed its end (cleanly if `None`).
    Local(Option<std::io::Error>),
    /// The exporter's connection ended.
    Remote(Option<std::io::Error>),
}

fn eof_ok(e: std::io::Error) -> Option<std::io::Error> {
    (e.kind() != ErrorKind::UnexpectedEof).then_some(e)
}

/// Relay URBs between the host's USB stack (`l*`) and the exporter (`r*`), with the round-trip savings applied when
/// `accel` is on. With `handshake`, first pass the import request/reply through untouched (the Windows driver sends its own).
/// `tx`/`rx` count bytes sent to / received from the exporter.
pub async fn relay<LR, LW, RR, RW>(mut lr: LR, mut lw: LW, mut rr: RR, mut rw: RW, accel: bool, handshake: bool, tx: &AtomicU64, rx: &AtomicU64) -> Ended
where
    LR: AsyncRead + Unpin,
    LW: AsyncWrite + Unpin,
    RR: AsyncRead + Unpin,
    RW: AsyncWrite + Unpin,
{
    if handshake {
        let r: std::io::Result<()> = async {
            let mut req = [0u8; 8 + 32];
            lr.read_exact(&mut req).await?;
            rw.write_all(&req).await?;
            rw.flush().await?;
            tx.fetch_add(req.len() as u64, Ordering::Relaxed);
            let mut h = [0u8; 8];
            rr.read_exact(&mut h).await?;
            let mut rep = h.to_vec();
            if OpHeader::decode(&h).map(|o| o.status == 0).unwrap_or(false) {
                let mut dev = [0u8; DEVICE_LEN];
                rr.read_exact(&mut dev).await?;
                rep.extend_from_slice(&dev);
            }
            rx.fetch_add(rep.len() as u64, Ordering::Relaxed);
            lw.write_all(&rep).await?;
            lw.flush().await
        }
        .await;
        if let Err(e) = r {
            return Ended::Remote(eof_ok(e));
        }
    }
    let st = Arc::new(Mutex::new(State { accel: Accel::new(accel), inflight: HashMap::new(), unlinks: HashMap::new() }));
    let (ltx, mut lrx) = unbounded_channel::<Vec<u8>>();
    let (rtx, mut rrx) = unbounded_channel::<Vec<u8>>();

    let local_writer = async {
        while let Some(m) = lrx.recv().await {
            lw.write_all(&m).await?;
            lw.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let remote_writer = async {
        while let Some(m) = rrx.recv().await {
            rw.write_all(&m).await?;
            rw.flush().await?;
            tx.fetch_add(m.len() as u64, Ordering::Relaxed);
        }
        Ok::<(), std::io::Error>(())
    };

    // host -> exporter
    let st1 = st.clone();
    let (ltx1, rtx1) = (ltx.clone(), rtx.clone());
    let to_remote = async move {
        loop {
            let mut h = [0u8; URB_HEADER_LEN];
            lr.read_exact(&mut h).await?;
            let basic = peek_basic(&h).map_err(std::io::Error::other)?;
            match basic.command {
                USBIP_CMD_SUBMIT => {
                    let c = CmdSubmit::decode(&h).map_err(std::io::Error::other)?;
                    let len = c.transfer_buffer_length;
                    if !(0..=MAX_URB_BYTES).contains(&len) || c.number_of_packets > 4096 {
                        return Err(std::io::Error::other(format!("URB length {len} out of range")));
                    }
                    let is_in = c.basic.direction == DIR_IN;
                    let mut m = h.to_vec();
                    if !is_in && len > 0 {
                        let at = m.len();
                        m.resize(at + len as usize, 0);
                        lr.read_exact(&mut m[at..]).await?;
                    }
                    if c.number_of_packets > 0 {
                        let at = m.len();
                        m.resize(at + c.number_of_packets as usize * 16, 0);
                        lr.read_exact(&mut m[at..]).await?;
                    }
                    let seq = basic.seqnum;
                    let decision = {
                        let mut s = st1.lock().unwrap();
                        let payload = if !is_in && len > 0 { &m[URB_HEADER_LEN..URB_HEADER_LEN + len as usize] } else { &[][..] };
                        let d = s.accel.submit(seq, c.basic.ep as u8, is_in, len as usize, payload, c.number_of_packets > 0);
                        if !matches!(d, Submit::Csw(_)) {
                            s.inflight.insert(seq, Inflight { is_in });
                        }
                        d
                    };
                    match decision {
                        Submit::Forward => rtx1.send(m).ok(),
                        Submit::Ghost => {
                            let ack = ret_submit(seq, 0, len as usize, &[]);
                            rtx1.send(m).ok().and(ltx1.send(ack).ok())
                        }
                        Submit::Csw(csw) => ltx1.send(ret_submit(seq, 0, CSW_LEN, &csw)).ok(),
                    }
                    .ok_or_else(|| std::io::Error::from(ErrorKind::BrokenPipe))?;
                }
                USBIP_CMD_UNLINK => {
                    let u = CmdUnlink::decode(&h).map_err(std::io::Error::other)?;
                    st1.lock().unwrap().unlinks.insert(u.basic.seqnum, u.unlink_seqnum);
                    rtx1.send(h.to_vec()).map_err(|_| std::io::Error::from(ErrorKind::BrokenPipe))?;
                }
                c => return Err(std::io::Error::other(format!("unexpected command {c:#x} from the USB stack"))),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };

    // exporter -> host
    let st2 = st.clone();
    let ltx2 = ltx.clone();
    let to_local = async move {
        loop {
            let mut h = [0u8; URB_HEADER_LEN];
            rr.read_exact(&mut h).await?;
            let basic = peek_basic(&h).map_err(std::io::Error::other)?;
            match basic.command {
                USBIP_RET_SUBMIT => {
                    let r = RetSubmit::decode(&h).map_err(std::io::Error::other)?;
                    let seq = basic.seqnum;
                    let (inf, ghost) = {
                        let mut s = st2.lock().unwrap();
                        let inf = s.inflight.remove(&seq);
                        let ghost = inf.is_some() && s.accel.ret(seq);
                        (inf, ghost)
                    };
                    let inf = inf.ok_or_else(|| std::io::Error::other(format!("reply for unknown URB {seq}")))?;
                    let mut body = vec![];
                    let n = if inf.is_in && r.status == 0 { r.actual_length.max(0) as usize } else { 0 };
                    if n > MAX_URB_BYTES as usize {
                        return Err(std::io::Error::other("reply too large"));
                    }
                    body.resize(n, 0);
                    rr.read_exact(&mut body).await?;
                    let mark = csw_mark_ep(&h);
                    let mut csw = [0u8; CSW_LEN];
                    if mark.is_some() {
                        rr.read_exact(&mut csw).await?;
                    }
                    if r.number_of_packets > 0 {
                        let at = body.len();
                        body.resize(at + r.number_of_packets as usize * 16, 0);
                        rr.read_exact(&mut body[at..]).await?;
                    }
                    rx.fetch_add((URB_HEADER_LEN + body.len() + if mark.is_some() { CSW_LEN } else { 0 }) as u64, Ordering::Relaxed);
                    if let Some(ep) = mark {
                        st2.lock().unwrap().accel.store_csw(ep, csw);
                        h[40..44].fill(0); // the operating system never sees the marker
                    }
                    if ghost {
                        if r.status != 0 {
                            crate::logln!("the exporter could not deliver a storage command block (status {}); the next transfer will fail so the host recovers", r.status);
                        }
                        continue; // the host was told already; the real result of a command block only matters if it failed
                    }
                    let mut m = h.to_vec();
                    m.extend_from_slice(&body);
                    ltx2.send(m).map_err(|_| std::io::Error::from(ErrorKind::BrokenPipe))?;
                }
                USBIP_RET_UNLINK => {
                    let status = i32::from_be_bytes([h[20], h[21], h[22], h[23]]);
                    {
                        let mut s = st2.lock().unwrap();
                        if let Some(target) = s.unlinks.remove(&basic.seqnum) {
                            if status != 0 {
                                // cancelled: no reply for the target will ever come
                                s.inflight.remove(&target);
                                s.accel.forget(target);
                            }
                        }
                    }
                    rx.fetch_add(URB_HEADER_LEN as u64, Ordering::Relaxed);
                    ltx2.send(h.to_vec()).map_err(|_| std::io::Error::from(ErrorKind::BrokenPipe))?;
                }
                c => return Err(std::io::Error::other(format!("unexpected command {c:#x} from the exporter"))),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };

    // `ltx`/`rtx` clones live in the reader futures; drop ours so a closed channel ends the writers.
    drop((ltx, rtx));
    tokio::select! {
        r = to_remote => Ended::Local(r.err().and_then(eof_ok)),
        r = to_local => Ended::Remote(r.err().and_then(eof_ok)),
        r = local_writer => Ended::Local(r.err()),
        r = remote_writer => Ended::Remote(r.err()),
    }
}

/// Whether to ask for the round-trip savings. `PERIWIRE_ACCEL=0` turns them off (for comparison or as a safety valve).
pub fn wanted() -> bool {
    !matches!(std::env::var("PERIWIRE_ACCEL").as_deref(), Ok("0") | Ok("off") | Ok("false"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbw() -> Vec<u8> {
        let mut b = vec![0u8; 31];
        b[..4].copy_from_slice(b"USBC");
        b
    }

    #[test]
    fn command_block_is_completed_early_and_its_reply_swallowed() {
        let mut a = Accel::new(true);
        assert_eq!(a.submit(1, 2, false, 31, &cbw(), false), Submit::Ghost);
        assert!(a.ret(1), "the exporter's reply to the command block is swallowed");
        assert!(!a.ret(1), "only once");
    }

    #[test]
    fn a_status_read_with_a_larger_buffer_is_answered_too() {
        // Windows reads the status block into a full 512-byte packet; the answer is still the 13 bytes the device would send.
        let mut a = Accel::new(true);
        a.store_csw(1, [7; CSW_LEN]);
        assert_eq!(a.submit(5, 1, true, 512, &[], false), Submit::Csw([7; CSW_LEN]));
        assert_eq!(a.submit(6, 1, true, 512, &[], false), Submit::Forward, "only once");
        a.store_csw(1, [7; CSW_LEN]);
        assert_eq!(a.submit(7, 1, true, 12, &[], false), Submit::Forward, "too small to be a status block");
    }

    #[test]
    fn off_means_everything_is_forwarded() {
        let mut a = Accel::new(false);
        assert_eq!(a.submit(1, 2, false, 31, &cbw(), false), Submit::Forward);
        assert!(!a.ret(1));
    }

    #[test]
    fn delivered_status_answers_the_next_status_read_once() {
        let mut a = Accel::new(true);
        a.store_csw(1, [7; CSW_LEN]);
        assert_eq!(a.submit(5, 1, true, 13, &[], false), Submit::Csw([7; CSW_LEN]));
        assert_eq!(a.submit(6, 1, true, 13, &[], false), Submit::Forward, "used up");
    }

    #[test]
    fn other_traffic_drops_a_stale_status() {
        for (ep, is_in, len, why) in [(0u8, true, 8usize, "control transfer (reset recovery)"), (1, true, 4, "a read too small to be the status"), (2, false, 512, "a data write")] {
            let mut a = Accel::new(true);
            a.submit(1, 2, false, 31, &cbw(), false); // learns the OUT endpoint
            a.store_csw(1, [7; CSW_LEN]);
            assert_eq!(a.submit(2, ep, is_in, len, &vec![0; if is_in { 0 } else { len }], false), Submit::Forward);
            assert_eq!(a.submit(3, 1, true, 13, &[], false), Submit::Forward, "{why}: the stale status is gone");
        }
    }

    #[test]
    fn unrelated_endpoints_do_not_disturb_it() {
        let mut a = Accel::new(true);
        a.submit(1, 2, false, 31, &cbw(), false);
        a.store_csw(1, [7; CSW_LEN]);
        assert_eq!(a.submit(2, 3, true, 8, &[], false), Submit::Forward, "an interrupt poll on another endpoint");
        assert_eq!(a.submit(3, 1, true, 13, &[], false), Submit::Csw([7; CSW_LEN]));
    }

    #[test]
    fn a_new_command_discards_a_leftover_status() {
        let mut a = Accel::new(true);
        a.store_csw(1, [7; CSW_LEN]);
        assert_eq!(a.submit(9, 2, false, 31, &cbw(), false), Submit::Ghost);
        assert_eq!(a.submit(10, 1, true, 13, &[], false), Submit::Forward);
    }

    #[test]
    fn isochronous_and_odd_sizes_are_never_touched() {
        let mut a = Accel::new(true);
        assert_eq!(a.submit(1, 2, false, 31, &cbw(), true), Submit::Forward);
        assert_eq!(a.submit(2, 2, false, 30, &cbw()[..30], false), Submit::Forward);
        assert_eq!(a.submit(3, 2, false, 31, &vec![0u8; 31], false), Submit::Forward, "not a command block");
    }


    #[test]
    fn exporter_attaches_the_status_only_after_a_full_data_stage() {
        let mut cb = cbw();
        cb[8..12].copy_from_slice(&1024u32.to_le_bytes());
        cb[12] = 0x80; // read, 1024 bytes
        let mut s = Seq::default();
        let st = s.begin(false, 2, &cb, 1, 2);
        assert_eq!(st, Step::Cbw { data_len: 1024, dir_in: true });
        assert!(!s.finish(st, true, 31, 31));
        // the data stage arrives in two pieces: the status is attached to the last one only
        let st = s.begin(true, 1, &[], 1, 2);
        assert_eq!(st, Step::Data);
        assert!(!s.finish(st, true, 512, 512));
        let st = s.begin(true, 1, &[], 1, 2);
        assert_eq!(st, Step::Data);
        assert!(s.finish(st, true, 512, 512));
        // a later status read is an ordinary transfer (the host would not send it: the importer answers it)
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other);
    }

    #[test]
    fn exporter_leaves_short_or_failed_data_to_the_ordinary_path() {
        let mut cb = cbw();
        cb[8..12].copy_from_slice(&512u32.to_le_bytes());
        cb[12] = 0x80;
        for (ok, moved) in [(true, 100usize), (false, 0)] {
            let mut s = Seq::default();
            let st = s.begin(false, 2, &cb, 1, 2);
            s.finish(st, true, 31, 31);
            let st = s.begin(true, 1, &[], 1, 2);
            assert!(!s.finish(st, ok, moved, 512), "ok={ok} moved={moved}");
            assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other, "the host reads the status itself afterwards");
        }
    }

    #[test]
    fn only_reset_recovery_clears_a_lost_command_block() {
        let mut s = Seq::default();
        let st = s.begin(false, 2, &cbw(), 1, 2);
        s.finish(st, false, 0, 31);
        s.control(&[0x80, 0x06, 0, 1, 0, 0, 0x12, 0]); // a descriptor read: nothing to do with recovery
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Fail, "still failing the next transfer");
        let st = s.begin(false, 2, &cbw(), 1, 2);
        s.finish(st, false, 0, 31);
        s.control(&[0x02, 0x01, 0, 0, 0x81, 0, 0, 0]); // clear halt on an endpoint
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other, "recovery done");
        let st = s.begin(false, 2, &cbw(), 1, 2);
        s.finish(st, false, 0, 31);
        s.control(&[0x21, 0xff, 0, 0, 0, 0, 0, 0]); // class reset
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other);
    }

    #[test]
    fn writes_get_the_status_with_the_last_data_ack() {
        let mut cb = cbw();
        cb[8..12].copy_from_slice(&512u32.to_le_bytes());
        cb[12] = 0; // write
        let mut s = Seq::default();
        let st = s.begin(false, 2, &cb, 1, 2);
        s.finish(st, true, 31, 31);
        let st = s.begin(false, 2, &[0; 512], 1, 2);
        assert_eq!(st, Step::Data);
        assert!(s.finish(st, true, 512, 512));
    }

    #[test]
    fn commands_without_data_never_carry_a_status() {
        let mut s = Seq::default();
        let st = s.begin(false, 2, &cbw(), 1, 2); // TEST UNIT READY: no data stage
        s.finish(st, true, 31, 31);
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other);
    }

    #[test]
    fn a_failed_command_block_fails_the_next_transfer_once() {
        let mut s = Seq::default();
        let st = s.begin(false, 2, &cbw(), 1, 2);
        s.finish(st, false, 0, 31);
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Fail);
        assert_eq!(s.begin(true, 1, &[], 1, 2), Step::Other, "then back to normal");
    }

    /// Drive `relay` with an in-memory host and exporter: a read command costs one reply from the exporter.
    #[tokio::test]
    async fn relay_reads_with_one_exporter_reply() {
        use tokio::io::duplex;
        let (host, host_far) = duplex(1 << 16);
        let (exp, exp_far) = duplex(1 << 16);
        let (hr, hw) = tokio::io::split(host_far);
        let (er, ew) = tokio::io::split(exp_far);
        let (tx, rx) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (t2, r2) = (tx.clone(), rx.clone());
        let task = tokio::spawn(async move { relay(hr, hw, er, ew, true, false, &t2, &r2).await });
        let (mut host_r, mut host_w) = tokio::io::split(host);
        let (mut exp_r, mut exp_w) = tokio::io::split(exp);

        // host: command block (seq 1), data read (seq 2)
        let cmd = |seq: u32, ep: u32, dir: u32, len: i32| CmdSubmit { basic: UrbBasic { command: USBIP_CMD_SUBMIT, seqnum: seq, devid: 1, direction: dir, ep }, transfer_flags: 0, transfer_buffer_length: len, start_frame: 0, number_of_packets: 0, interval: 0, setup: [0; 8] };
        let mut m = cmd(1, 2, DIR_OUT, 31).encode();
        m.extend_from_slice(&cbw());
        host_w.write_all(&m).await.unwrap();
        host_w.write_all(&cmd(2, 1, DIR_IN, 512).encode()).await.unwrap();

        // the host gets the command block's completion at once, with no exporter reply yet
        let mut h = [0u8; URB_HEADER_LEN];
        host_r.read_exact(&mut h).await.unwrap();
        let r = RetSubmit::decode(&h).unwrap();
        assert_eq!((r.basic.seqnum, r.status, r.actual_length), (1, 0, 31));

        // the exporter saw both transfers, in order
        let mut buf = [0u8; URB_HEADER_LEN + 31];
        exp_r.read_exact(&mut buf).await.unwrap();
        assert_eq!(CmdSubmit::decode(&buf[..48]).unwrap().basic.seqnum, 1);
        exp_r.read_exact(&mut h).await.unwrap();
        assert_eq!(CmdSubmit::decode(&h).unwrap().basic.seqnum, 2);

        // exporter: reply to the command block, then data with the status block attached
        exp_w.write_all(&ret_submit(1, 0, 31, &[])).await.unwrap();
        let mut reply = ret_submit(2, 0, 512, &[]);
        reply[40..44].copy_from_slice(&csw_mark(1));
        reply.extend_from_slice(&[0xAB; 512]);
        reply.extend_from_slice(&[9; CSW_LEN]);
        exp_w.write_all(&reply).await.unwrap();

        // the host gets the data (marker removed, status block not appended)...
        host_r.read_exact(&mut h).await.unwrap();
        assert_eq!(csw_mark_ep(&h), None);
        let r = RetSubmit::decode(&h).unwrap();
        assert_eq!((r.basic.seqnum, r.actual_length), (2, 512));
        let mut data = vec![0u8; 512];
        host_r.read_exact(&mut data).await.unwrap();
        assert!(data.iter().all(|&b| b == 0xAB));

        // ...and its status read is answered from memory: nothing more goes to the exporter
        host_w.write_all(&cmd(3, 1, DIR_IN, 13).encode()).await.unwrap();
        host_r.read_exact(&mut h).await.unwrap();
        let r = RetSubmit::decode(&h).unwrap();
        assert_eq!((r.basic.seqnum, r.actual_length), (3, 13));
        let mut c = [0u8; 13];
        host_r.read_exact(&mut c).await.unwrap();
        assert_eq!(c, [9; 13]);
        let mut extra = [0u8; 1];
        let nothing = tokio::time::timeout(std::time::Duration::from_millis(100), exp_r.read(&mut extra)).await;
        assert!(nothing.is_err(), "the status read must not reach the exporter");

        // closing needs both halves of each pipe gone; then the relay must end, and promptly
        drop((host_w, host_r, exp_w, exp_r));
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), task).await.expect("the relay ends when both sides are closed");
        assert!(ended.is_ok());
    }
}
