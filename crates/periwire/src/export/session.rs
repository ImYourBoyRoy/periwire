//! One session's URB loop: reading submits from the importer, dispatching them, and handing the device back at the end.
use super::*;

/// How `urb_loop` ended: the outcome, why the connection ended (see `resume`), and the device if it is free to be reused.
pub(super) struct LoopEnd {
    pub(super) result: Result<()>,
    pub(super) cause: crate::resume::Cause,
    pub(super) dev: Option<Dev>,
}

/// One URB header from the importer. A properly closed connection (TLS close_notify) reads as end-of-stream before the
/// first byte; anything else that stops the stream is a loss.
pub(super) enum Hdr {
    Got,
    Closed,
    Lost(std::io::Error),
}

pub(super) async fn read_header<R: AsyncReadExt + Unpin>(r: &mut R, h: &mut [u8; URB_HEADER_LEN]) -> Hdr {
    match r.read(&mut h[..1]).await {
        Ok(0) => Hdr::Closed,
        Ok(_) => match r.read_exact(&mut h[1..]).await {
            Ok(_) => Hdr::Got,
            Err(e) => Hdr::Lost(e),
        },
        Err(e) => Hdr::Lost(e),
    }
}

#[allow(clippy::too_many_arguments)] // the session's handles (stream, writer queue, device, three watch channels, session, options): one call site
pub(super) async fn urb_loop<R: AsyncReadExt + Unpin>(
    r: &mut R,
    tx: UnboundedSender<Vec<u8>>,
    dev: Arc<RwLock<Dev>>,
    mut shutdown: watch::Receiver<bool>,
    mut kick: watch::Receiver<bool>,
    sess: Arc<Session>,
    accel: bool,
    patches: Vec<Patch>,
) -> LoopEnd {
    use crate::resume::Cause;
    let mut cause = Cause::Failed; // until the loop says otherwise: errors with `?` below are protocol errors
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    // Read-only mode: write-protect USB storage on this session (the caller already refused non-storage devices).
    let guard: Option<Arc<Mutex<Guard>>> = if sess.read_only {
        dev.read().unwrap().bot.map(|b| {
            let mut g = Guard::new(b.ep_out, b.ep_in);
            g.set_patches(patches);
            Arc::new(Mutex::new(g))
        })
    } else {
        None
    };
    // Round-trip savings for storage (FLAG_ACCEL): its bulk transfers run one at a time, in order, on a worker that
    // can attach the status block to the last data reply. Everything else stays concurrent.
    let bot_q: Option<(tokio::sync::mpsc::UnboundedSender<BotJob>, Bot)> = match (accel, dev.read().unwrap().bot) {
        (true, Some(b)) => {
            let (qtx, qrx) = tokio::sync::mpsc::unbounded_channel::<BotJob>();
            let (tx2, dev2, pend2, sess2, guard2) = (tx.clone(), dev.clone(), pending.clone(), sess.clone(), guard.clone());
            tokio::task::spawn_blocking(move || bot_worker(qrx, b, dev2, tx2, pend2, sess2, guard2));
            Some((qtx, b))
        }
        _ => None,
    };
    // Everything that is not a storage command runs here. Control transfers run at once and side by side; the transfers of a data
    // endpoint run in the order they arrived (see `Ordered`).
    let run: Arc<dyn Fn(UrbJob) + Send + Sync> = {
        let (tx2, dev2, pend2, sess2, guard2) = (tx.clone(), dev.clone(), pending.clone(), sess.clone(), guard.clone());
        Arc::new(move |UrbJob { c, out, cancel }| {
            if cancel.load(Ordering::Relaxed) {
                pend2.lock().unwrap().remove(&c.basic.seqnum); // unlinked while it waited its turn: never send it to the device
                return;
            }
            let is_in = c.basic.direction == DIR_IN;
            let (status, mut data) = run_urb(&dev2, &c, out, &cancel);
            note_gone(&dev2, &sess2);
            if let Some(g) = &guard2 {
                g.lock().unwrap().on_complete(c.basic.ep as u8, is_in, &mut data);
            }
            let live = pend2.lock().unwrap().remove(&c.basic.seqnum).is_some();
            trace!("done   seq={} status={} bytes={} live={}", c.basic.seqnum, status, data.len(), live);
            if cancel.load(Ordering::Relaxed) || !live {
                return;
            }
            sess2.tx.fetch_add(data.len() as u64, Ordering::Relaxed);
            let m = make_ret(&c, status, &data);
            send(&tx2, m);
        })
    };
    let mut ordered: Ordered<(u8, bool), UrbJob> = Ordered::new(run.clone());
    let result: Result<()> = async {
        loop {
            let mut h = [0u8; URB_HEADER_LEN];
            let got = tokio::select! {
                r = read_header(r, &mut h) => r,
                _ = shutdown.changed() => { cause = Cause::Clean; return Ok(()) },
                _ = kick.changed() => {
                    if sess.takeover.load(Ordering::SeqCst) {
                        cause = Cause::Lost; // the importer is back on a new connection: this one is dead
                    } else if sess.gone.load(Ordering::SeqCst) {
                        // The device dropped off the bus (unplugged, or an Apple device re-enumerating after the host's mode switch).
                        // Close properly, like a real disconnect: the importer detaches it from its host and, if it asked to be kept
                        // attached, reconnects and presents the device that came back. Nothing to park: the handle is dead.
                        crate::logln!("session {} on {}: the device disconnected itself (unplugged or re-enumerated); ending the session so it can be attached again", sess.id, sess.busid);
                        cause = Cause::Clean;
                    } else {
                        crate::logln!("session {} on {} disconnected by operator", sess.id, sess.busid);
                        cause = Cause::Clean;
                    }
                    return Ok(())
                },
            };
            match got {
                Hdr::Got => {}
                Hdr::Closed => {
                    cause = Cause::Clean;
                    return Ok(());
                }
                Hdr::Lost(e) => {
                    cause = Cause::Lost;
                    return if e.kind() == std::io::ErrorKind::UnexpectedEof { Ok(()) } else { Err(e.into()) };
                }
            }
            let basic = peek_basic(&h)?;
            match basic.command {
                USBIP_CMD_SUBMIT => {
                    let c = CmdSubmit::decode(&h)?;
                    c.validate()?; // before anything is allocated for what the header announces
                    let len = c.transfer_buffer_length;
                    let mut out = vec![];
                    if c.basic.direction == DIR_OUT && len > 0 {
                        out = vec![0u8; len as usize];
                        r.read_exact(&mut out).await?;
                    }
                    let np = c.number_of_packets;
                    if np > 0 {
                        // Isochronous transfers are not supported yet: drain descriptors, fail the URB.
                        let mut iso = vec![0u8; np as usize * 16];
                        r.read_exact(&mut iso).await?;
                        let ret = RetSubmit {
                            basic: UrbBasic { command: USBIP_RET_SUBMIT, devid: 0, direction: 0, ep: 0, ..c.basic },
                            status: EPIPE,
                            actual_length: 0,
                            start_frame: 0,
                            number_of_packets: np,
                            error_count: np,
                        };
                        let mut m = ret.encode();
                        m.extend_from_slice(&iso);
                        send(&tx, m);
                        continue;
                    }
                    sess.urbs.fetch_add(1, Ordering::Relaxed);
                    sess.rx.fetch_add(out.len() as u64, Ordering::Relaxed);
                    trace!("submit seq={} ep={} dir={} len={} setup={:02x?}", basic.seqnum, c.basic.ep, c.basic.direction, len, c.setup);
                    if trace_on() && out.len() == 31 && out.starts_with(b"USBC") {
                        // Mass-storage command block wrapper: show what SCSI command the importer's OS is sending.
                        trace!(
                            "   CBW tag={:08x} data_len={} flags={:02x} lun={} cdb_len={} cdb={:02x?}",
                            u32::from_le_bytes([out[4], out[5], out[6], out[7]]),
                            u32::from_le_bytes([out[8], out[9], out[10], out[11]]),
                            out[12],
                            out[13],
                            out[14],
                            &out[15..15 + (out[14] as usize).min(16)]
                        );
                    }
                    if let Some(g) = &guard {
                        let verdict = g.lock().unwrap().on_submit(c.basic.ep as u8, c.basic.direction == DIR_IN, len as usize, &out);
                        if let Verdict::Complete { status, data } = verdict {
                            trace!("guard seq={} answered locally status={} bytes={}", basic.seqnum, status, data.len());
                            sess.tx.fetch_add(data.len() as u64, Ordering::Relaxed);
                            send(&tx, make_ret(&c, status, &data));
                            continue;
                        }
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    pending.lock().unwrap().insert(basic.seqnum, cancel.clone());
                    if let Some((q, b)) = &bot_q {
                        let is_in = c.basic.direction == DIR_IN;
                        let ep = c.basic.ep as u8;
                        // Control transfers join the queue: a host clears a stalled endpoint and retries the read at once, and the
                        // two must run in that order (on separate threads the read could overtake the clear and stall again).
                        if ep == 0 || (is_in && ep == b.ep_in & 0x0f) || (!is_in && ep == b.ep_out & 0x0f) {
                            if q.send(BotJob { c, out, cancel }).is_err() {
                                bail!("storage worker stopped");
                            }
                            continue;
                        }
                    }
                    let job = UrbJob { c, out, cancel };
                    if job.c.basic.ep == 0 {
                        let run = run.clone();
                        tokio::task::spawn_blocking(move || run(job));
                    } else if !ordered.send((job.c.basic.ep as u8, job.c.basic.direction == DIR_IN), job) {
                        bail!("endpoint worker stopped");
                    }
                }
                USBIP_CMD_UNLINK => {
                    let u = CmdUnlink::decode(&h)?;
                    trace!("unlink seq={} target={}", u.basic.seqnum, u.unlink_seqnum);
                    let found = pending.lock().unwrap().remove(&u.unlink_seqnum);
                    let status = match found {
                        Some(flag) => {
                            flag.store(true, Ordering::Relaxed);
                            ECONNRESET
                        }
                        None => 0,
                    };
                    send(&tx, encode_ret_unlink(u.basic, status));
                }
                c => bail!("unexpected URB command {c:#x}"),
            }
        }
    }
    .await;
    // Cancel everything still in flight; dropping `dev` afterwards releases the device.
    for (_, f) in pending.lock().unwrap().drain() {
        f.store(true, Ordering::Relaxed);
    }
    drop(bot_q); // ends the storage worker once it has drained
    drop(ordered); // and the endpoint workers
    drop(run); // the closure holds a handle on the device too: without this the count below never reaches 1
               // Wait for in-flight workers so the device is really free before we report it.
    for _ in 0..100 {
        if Arc::strong_count(&dev) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Hand the device back to the caller, which either releases it (the usual case) or parks it for a reconnect.
    // If a worker is still stuck inside it, it is dropped when that worker finishes.
    let dev = Arc::try_unwrap(dev).ok().and_then(|l| l.into_inner().ok());
    LoopEnd { result, cause, dev }
}
