//! One importer's connection: the device list, device info and the import request, ending in a session.
use super::*;

/// Device info for one busid (used to describe sessions).
pub(super) fn info_for(shared: &Shared, busid: &str) -> Value {
    device_views(shared).ok().and_then(|v| v.into_iter().find(|d| d["busid"] == busid)).unwrap_or(Value::Null)
}

pub(super) async fn handle(stream: tokio_rustls::server::TlsStream<TcpStream>, shared: Arc<Shared>, peer: String, shutdown: watch::Receiver<bool>) -> Result<()> {
    let peer_fp = stream.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| identity::fingerprint(c.as_ref())).unwrap_or_default();
    let peer_name = identity::load_trusted_named().unwrap_or_default().into_iter().find(|(f, _)| *f == peer_fp).map(|(_, n)| n).unwrap_or_default();
    let (mut r, mut w) = tokio::io::split(stream);
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if w.write_all(&m).await.is_err() || w.flush().await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });

    let mut hdr = [0u8; 8];
    let mut flags = 0u32;
    let mut accel = false;
    let mut journal_fix = false;
    let mut resume = false;
    let op = loop {
        r.read_exact(&mut hdr).await?;
        let op = OpHeader::decode(&hdr)?;
        if op.code == OP_REQ_FLAGS {
            // PeriWire extension: session options sent before the import request (see usbip-proto)
            let mut b = [0u8; 4];
            r.read_exact(&mut b).await?;
            flags = u32::from_be_bytes(b);
            accel = flags & FLAG_ACCEL != 0 && crate::accel::wanted();
            journal_fix = flags & FLAG_NTFS_JOURNAL != 0;
            resume = flags & FLAG_RESUME != 0 && !crate::resume::grace().is_zero() && !peer_fp.is_empty();
            send(&tx, OpHeader::new(OP_REP_FLAGS, (if accel { FLAG_ACCEL } else { 0 }) | (if journal_fix { FLAG_NTFS_JOURNAL } else { 0 }) | (if resume { FLAG_RESUME } else { 0 })).encode().to_vec());
            continue;
        }
        break op;
    };
    let read_only = flags & FLAG_READ_ONLY != 0; // asked for by the importer
    match op.code {
        OP_REQ_DEVLIST => {
            let s = shared.specs();
            let devs = tokio::task::spawn_blocking(move || shared_devices(&s)).await??;
            send(&tx, encode_devlist_reply(&devs));
            drop(tx);
            let _ = writer.await;
            Ok(())
        }
        OP_REQ_DEVINFO => {
            let sh = shared.clone();
            let mut views = tokio::task::spawn_blocking(move || shared_views(&sh)).await??;
            for v in views.iter_mut() {
                if let Some(o) = v.as_object_mut() {
                    o.remove("serial"); // peers don't need hardware serial numbers
                    o.remove("key");
                    o.remove("in_use_by");
                }
            }
            let body = serde_json::to_vec(&views)?;
            let mut m = OpHeader::new(OP_REP_DEVINFO, 0).encode().to_vec();
            m.extend_from_slice(&(body.len() as u32).to_be_bytes());
            m.extend_from_slice(&body);
            send(&tx, m);
            drop(tx);
            let _ = writer.await;
            Ok(())
        }
        OP_REQ_IMPORT => {
            let mut body = [0u8; 32];
            r.read_exact(&mut body).await?;
            let want = decode_import_request_busid(&body)?;
            let s = shared.specs();
            let ro_specs = shared.ro_specs.read().unwrap().clone();
            // The same importer reconnecting on a new connection while the old one has not been noticed as dead yet
            // (a silent network outage): that connection is gone, so end it and let the device be parked for this one.
            if let Some(old) = shared.session_for(&want) {
                if resume && old.resume && old.fp == peer_fp {
                    crate::logln!("[{peer}] {want}: the same importer is reconnecting; taking over from its old connection");
                    old.takeover.store(true, Ordering::SeqCst);
                    let _ = old.kick.send(true);
                    for _ in 0..100 {
                        if shared.session_for(&want).is_none() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
            if shared.session_for(&want).is_some() {
                crate::logln!("[{peer}] import of {want} refused: already in use by another peer");
                send(&tx, OpHeader::new(OP_REP_IMPORT, 1).encode().to_vec());
                drop(tx);
                let _ = writer.await;
                return Ok(());
            }
            let peer2 = peer.clone();
            let (shared2, fp2) = (shared.clone(), peer_fp.clone());
            let held_at = Instant::now();
            let opened = tokio::task::spawn_blocking(move || -> Result<(Dev, UsbDevice, bool, Vec<Patch>, u32, bool)> {
                let ctx = Context::new()?;
                for dev in ctx.devices()?.iter() {
                    if busid(&dev) == want && s.iter().any(|x| matches_spec(&dev, x)) {
                        let ro = read_only || ro_specs.iter().any(|x| matches_spec(&dev, x)); // the importer's request or the owner's rule
                        let options = (ro as u32) | ((journal_fix as u32) << 1);
                        // A device kept for this importer after it lost its connection is picked up as it is (still claimed, so
                        // nothing mounted it); one kept for somebody else is not ours to take.
                        let taken = shared2.parked.lock().unwrap().take(&want, &fp2, options, Instant::now());
                        let (d, resumed) = match taken {
                            crate::resume::Taken::Resumed(d) => {
                                d.dirty.store(false, Ordering::Relaxed);
                                d.bot_recover(); // the connection died mid-command: put the stick back in a known state
                                (d, true)
                            }
                            crate::resume::Taken::Reserved => bail!("{want} is being held for another computer that lost its connection; try again in a minute"),
                            other => {
                                drop(other); // a stale hold (different options, or expired) is released first, then opened the normal way
                                let mounted = mounted_filesystems(&want);
                                if !mounted.is_empty() {
                                    bail!("refusing to share {want}: still mounted locally ({}); unmount it first", mounted.join(", "));
                                }
                                (Dev::open(dev)?, false)
                            }
                        };
                        if ro && d.bot.is_none() {
                            bail!("read-only unsupported: {want} is not plain USB mass storage, so it cannot be write-protected; refusing rather than leaving it writable");
                        }
                        let patches = if ro && journal_fix { journal_patches(&d, &want, &peer2) } else { vec![] };
                        let (mut u, _) = describe(&d.dev)?;
                        u.configuration_value = d.config;
                        return Ok((d, u, ro, patches, options, resumed));
                    }
                }
                bail!("device {want} is not shared or not present")
            })
            .await?;
            match opened {
                Err(e) => {
                    crate::logln!("[{peer}] import refused: {e:#}");
                    let code = if format!("{e:#}").starts_with("read-only unsupported") { IMPORT_READ_ONLY_UNSUPPORTED } else { 1 };
                    send(&tx, OpHeader::new(OP_REP_IMPORT, code).encode().to_vec());
                    drop(tx);
                    let _ = writer.await;
                    Ok(())
                }
                Ok((dev, udev, read_only, patches, options, resumed)) => {
                    crate::logln!(
                        "[{peer}] {} {} {:04x}:{:04x}{}",
                        if resumed { "resumed" } else { "imported" },
                        udev.busid,
                        udev.id_vendor,
                        udev.id_product,
                        if read_only { " (read-only)" } else { "" }
                    );
                    if resumed {
                        crate::logln!("[{peer}] {} came back after {} s", udev.busid, held_at.elapsed().as_secs());
                    }
                    let mut rep = OpHeader::new(OP_REP_IMPORT, 0).encode().to_vec();
                    udev.encode(&mut rep);
                    send(&tx, rep);
                    let (kick_tx, kick_rx) = watch::channel(false);
                    let sess = Arc::new(Session {
                        id: shared.next.fetch_add(1, Ordering::SeqCst),
                        peer: peer.clone(),
                        peer_name,
                        busid: udev.busid.clone(),
                        vid: udev.id_vendor,
                        pid: udev.id_product,
                        info: info_for(&shared, &udev.busid),
                        since: crate::events::now(),
                        urbs: AtomicU64::new(0),
                        read_only,
                        fp: peer_fp.clone(),
                        resume,
                        options,
                        takeover: AtomicBool::new(false),
                        gone: AtomicBool::new(false),
                        rx: AtomicU64::new(0),
                        tx: AtomicU64::new(0),
                        kick: kick_tx,
                    });
                    shared.sessions.lock().unwrap().insert(sess.id, sess.clone());
                    crate::events::emit("session.opened", sess.view());
                    crate::events::emit("state", json!({}));
                    let end = urb_loop(&mut r, tx, Arc::new(RwLock::new(dev)), shutdown, kick_rx, sess.clone(), accel, patches).await;
                    let res = end.result;
                    let _ = writer.await;
                    shared.sessions.lock().unwrap().remove(&sess.id);
                    let grace = crate::resume::grace();
                    match end.dev {
                        Some(dev) if crate::resume::should_park(end.cause, sess.resume, grace) => {
                            let deadline = Instant::now() + grace;
                            let (id, displaced) = shared.parked.lock().unwrap().park(&sess.busid, &sess.fp, sess.options, dev, deadline);
                            drop(displaced);
                            crate::logln!("[{peer}] connection lost: keeping {} for {} s so the same computer can reconnect", sess.busid, grace.as_secs());
                            let (sh, busid) = (shared.clone(), sess.busid.clone());
                            tokio::spawn(async move {
                                tokio::time::sleep(grace).await;
                                let expired = sh.parked.lock().unwrap().expire(&busid, id);
                                if let Some(d) = expired {
                                    let _ = tokio::task::spawn_blocking(move || drop(d)).await;
                                    crate::logln!("{busid}: nobody reconnected in time; released back to local host");
                                    crate::events::emit("state", json!({}));
                                }
                            });
                        }
                        other => {
                            drop(other); // dropping the device hands it back to this machine
                            crate::logln!("[{peer}] device {} released back to local host", sess.busid);
                        }
                    }
                    crate::events::emit("session.closed", sess.view());
                    crate::events::emit("state", json!({}));
                    res
                }
            }
        }
        c => bail!("unknown op code {c:#x}"),
    }
}
