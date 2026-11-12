#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
//! USB/IP client session: imports one device from an exporter and carries URBs to it.
//! Used by the macOS importer, which implements the host controller itself (on Linux the kernel's
//! `vhci_hcd` speaks USB/IP directly). Replies are matched to requests by sequence number, so any
//! number of URBs may be in flight.
use crate::accel::{Accel, Submit};
use crate::import::connect;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use usbip_proto::*;

const ENODEV: i32 = -19;

#[derive(Debug, Clone)]
pub struct SubmitRequest {
    /// Endpoint address including the direction bit (0x80 = IN). Endpoint 0 is the control pipe.
    pub ep: u8,
    pub setup: [u8; 8],
    /// Transfer length (for IN transfers, the buffer size to request).
    pub length: u32,
    /// Payload for OUT transfers.
    pub data: Vec<u8>,
    pub interval: u32,
}

#[derive(Debug, Clone)]
pub struct UrbResult {
    /// 0 on success, otherwise a negative Linux errno.
    pub status: i32,
    /// Bytes received (IN transfers).
    pub data: Vec<u8>,
    /// Bytes actually transferred (also valid for OUT).
    pub actual: u32,
}

struct Pending {
    is_in: bool,
    reply: oneshot::Sender<UrbResult>,
}

pub struct UrbClient {
    pub device: UsbDevice,
    devid: u32,
    next: AtomicU32,
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<u32, Pending>>>,
    unlinks: Arc<Mutex<HashMap<u32, u32>>>,
    closed: watch::Receiver<bool>,
    accel: Arc<Mutex<Accel>>,
    /// Set by `leave_cleanly`: this side means to detach, so the connection is closed properly (see `FLAG_RESUME`).
    leave: Arc<AtomicBool>,
    /// Bytes sent to / received from the exporter (shown as the attachment's traffic).
    sent: Arc<std::sync::atomic::AtomicU64>,
    recvd: Arc<std::sync::atomic::AtomicU64>,
}

fn fail(p: Pending, status: i32) {
    let _ = p.reply.send(UrbResult { status, data: vec![], actual: 0 });
}

impl UrbClient {
    /// Connects, imports `busid` and starts the reader/writer tasks. With `resume` (a connection that reconnects by itself)
    /// the exporter is asked to keep the device for this computer if the connection is lost; in that case the connection
    /// is only closed properly after `leave_cleanly`, so that a loss and a deliberate detach can be told apart.
    pub async fn import(addr: &str, busid: &str, read_only: bool, resume: bool) -> Result<Arc<Self>> {
        let mut s = connect(addr).await?;
        let accel_on = crate::import::request_flags(&mut s, crate::import::session_flags(read_only, resume)).await? & FLAG_ACCEL != 0;
        s.write_all(&encode_import_request(busid)).await?;
        let mut h = [0u8; 8];
        s.read_exact(&mut h).await?;
        let status = OpHeader::decode(&h)?.status;
        if status != 0 {
            return Err(crate::import::refusal(addr, busid, status));
        }
        let mut b = [0u8; DEVICE_LEN];
        s.read_exact(&mut b).await?;
        let device = UsbDevice::decode(&b)?;
        let devid = (device.busnum << 16) | device.devnum;

        let (mut rd, mut wr) = tokio::io::split(s);
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (closed_tx, closed) = watch::channel(false);
        let pending: Arc<Mutex<HashMap<u32, Pending>>> = Arc::default();
        let unlinks: Arc<Mutex<HashMap<u32, u32>>> = Arc::default();
        let accel = Arc::new(Mutex::new(Accel::new(accel_on)));
        let leave = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let recvd = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let (leave2, sent2) = (leave.clone(), sent.clone());
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                sent2.fetch_add(m.len() as u64, Ordering::Relaxed);
                if wr.write_all(&m).await.is_err() || wr.flush().await.is_err() {
                    break;
                }
            }
            if !resume || leave2.load(Ordering::SeqCst) {
                let _ = wr.shutdown().await; // TLS close_notify
            }
        });

        let (p2, u2, a2, r2) = (pending.clone(), unlinks.clone(), accel.clone(), recvd.clone());
        tokio::spawn(async move {
            let res: Result<()> = async {
                loop {
                    let mut hdr = [0u8; URB_HEADER_LEN];
                    rd.read_exact(&mut hdr).await?;
                    r2.fetch_add(URB_HEADER_LEN as u64, Ordering::Relaxed);
                    let basic = peek_basic(&hdr)?;
                    match basic.command {
                        USBIP_RET_SUBMIT => {
                            let r = RetSubmit::decode(&hdr)?;
                            if a2.lock().unwrap().ret(basic.seqnum) {
                                continue; // a command block the caller was told about already (it has no payload)
                            }
                            let p = p2.lock().unwrap().remove(&basic.seqnum);
                            let n = r.actual_length.max(0) as usize;
                            let want_data = p.as_ref().map(|p| p.is_in).unwrap_or(true) && r.status == 0;
                            let mut data = vec![];
                            if want_data && n > 0 {
                                data = vec![0u8; n];
                                rd.read_exact(&mut data).await?;
                                r2.fetch_add(data.len() as u64, Ordering::Relaxed);
                            }
                            if let Some(ep) = csw_mark_ep(&hdr) {
                                let mut csw = [0u8; CSW_LEN];
                                rd.read_exact(&mut csw).await?;
                                r2.fetch_add(CSW_LEN as u64, Ordering::Relaxed);
                                a2.lock().unwrap().store_csw(ep, csw);
                            }
                            if let Some(p) = p {
                                let _ = p.reply.send(UrbResult { status: r.status, data, actual: n as u32 });
                            }
                        }
                        USBIP_RET_UNLINK => {
                            let status = i32::from_be_bytes([hdr[20], hdr[21], hdr[22], hdr[23]]);
                            if let Some(target) = u2.lock().unwrap().remove(&basic.seqnum) {
                                if let Some(p) = p2.lock().unwrap().remove(&target) {
                                    fail(p, if status == 0 { ENODEV } else { status });
                                }
                            }
                        }
                        c => bail!("unexpected command {c:#x} from exporter"),
                    }
                }
            }
            .await;
            if let Err(e) = res {
                eprintln!("exporter connection closed: {e}");
            }
            for (_, p) in p2.lock().unwrap().drain() {
                fail(p, ENODEV);
            }
            let _ = closed_tx.send(true);
        });

        Ok(Arc::new(Self { device, devid, next: AtomicU32::new(1), out, pending, unlinks, closed, accel, leave, sent, recvd }))
    }

    /// Bytes sent to and received from the exporter so far.
    pub fn traffic(&self) -> (u64, u64) {
        (self.sent.load(Ordering::Relaxed), self.recvd.load(Ordering::Relaxed))
    }

    /// This side is detaching on purpose: close the connection properly when the session is dropped.
    pub fn leave_cleanly(&self) {
        self.leave.store(true, Ordering::SeqCst);
    }

    /// Queues a URB; returns its sequence number (for `unlink`) and the receiver for its result.
    pub fn submit(&self, req: SubmitRequest) -> (u32, oneshot::Receiver<UrbResult>) {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let is_in = req.ep & 0x80 != 0 || (req.ep & 0x7f == 0 && req.setup[0] & 0x80 != 0);
        if *self.closed.borrow() {
            fail(Pending { is_in, reply: tx }, ENODEV);
            return (seq, rx);
        }
        let decision = self.accel.lock().unwrap().submit(seq, req.ep & 0x7f, is_in, if is_in { req.length as usize } else { req.data.len() }, &req.data, false);
        match decision {
            Submit::Csw(csw) => {
                let _ = tx.send(UrbResult { status: 0, data: csw.to_vec(), actual: CSW_LEN as u32 });
                return (seq, rx);
            }
            Submit::Ghost => {
                let _ = tx.send(UrbResult { status: 0, data: vec![], actual: req.data.len() as u32 });
            }
            Submit::Forward => {
                self.pending.lock().unwrap().insert(seq, Pending { is_in, reply: tx });
            }
        }
        let cmd = CmdSubmit {
            basic: UrbBasic {
                command: USBIP_CMD_SUBMIT,
                seqnum: seq,
                devid: self.devid,
                direction: if is_in { DIR_IN } else { DIR_OUT },
                ep: (req.ep & 0x7f) as u32,
            },
            transfer_flags: 0,
            transfer_buffer_length: req.length as i32,
            start_frame: 0,
            number_of_packets: -1,
            interval: req.interval as i32,
            setup: req.setup,
        };
        let mut m = cmd.encode();
        if !is_in {
            m.extend_from_slice(&req.data);
        }
        if self.out.send(m).is_err() {
            if let Some(p) = self.pending.lock().unwrap().remove(&seq) {
                fail(p, ENODEV);
            }
        }
        (seq, rx)
    }

    /// Asks the exporter to cancel an in-flight URB; its result then arrives as -ECONNRESET.
    pub fn unlink(&self, target: u32) {
        if !self.pending.lock().unwrap().contains_key(&target) {
            return;
        }
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        self.unlinks.lock().unwrap().insert(seq, target);
        let mut m = Vec::with_capacity(URB_HEADER_LEN);
        for v in [USBIP_CMD_UNLINK, seq, self.devid, 0, 0, target] {
            m.extend_from_slice(&v.to_be_bytes());
        }
        m.resize(URB_HEADER_LEN, 0);
        let _ = self.out.send(m);
    }

    /// Resolves when the connection to the exporter is gone.
    pub async fn closed(&self) {
        let mut c = self.closed.clone();
        let _ = c.wait_for(|v| *v).await;
    }
}

/// Convenience used by tests and `periwire urbtest`: read the device descriptor through the session.
pub async fn read_device_descriptor(c: &UrbClient) -> Result<Vec<u8>> {
    let (_, rx) = c.submit(SubmitRequest { ep: 0, setup: [0x80, 6, 0, 1, 0, 0, 18, 0], length: 18, data: vec![], interval: 0 });
    let r = rx.await.context("session dropped")?;
    if r.status != 0 {
        bail!("GET_DESCRIPTOR failed with status {}", r.status);
    }
    Ok(r.data)
}
