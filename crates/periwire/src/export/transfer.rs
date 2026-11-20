//! Carrying out one URB on a device: control requests with their special cases, bulk and interrupt transfers.
use super::*;

pub(super) type Pending = Arc<Mutex<HashMap<u32, Arc<AtomicBool>>>>;

/// After a transfer: if the device vanished under it, end the session so the importer sees a real disconnect.
pub(super) fn note_gone(dev: &RwLock<Dev>, sess: &Session) {
    if dev.read().unwrap().gone.load(Ordering::SeqCst) && !sess.gone.swap(true, Ordering::SeqCst) {
        let _ = sess.kick.send(true);
    }
}

/// Run one URB to completion (or cancellation). Returns (status, data for IN transfers).
pub(super) fn run_urb(dev: &RwLock<Dev>, c: &CmdSubmit, out: Vec<u8>, cancel: &AtomicBool) -> (i32, Vec<u8>) {
    let len = c.transfer_buffer_length.max(0) as usize;
    let is_in = c.basic.direction == DIR_IN;
    if c.basic.ep == 0 {
        let s = c.setup;
        let (rt, req) = (s[0], s[1]);
        let val = u16::from_le_bytes([s[2], s[3]]);
        let idx = u16::from_le_bytes([s[4], s[5]]);
        // Requests the kernel's stub driver handles itself must be applied through libusb.
        match (rt, req) {
            (0x00, 0x09) => {
                let mut g = dev.write().unwrap();
                if g.config == val as u8 {
                    return (0, vec![]);
                }
                g.release_all();
                let r = g.h.set_active_configuration(val as u8).map_err(|e| {
                    g.config_refused(val as u8, &e);
                    g.err(e)
                });
                return match r {
                    Ok(()) => match g.claim_all() {
                        Ok(()) => (0, vec![]),
                        Err(_) => (EIO, vec![]),
                    },
                    Err(e) => {
                        let _ = g.claim_all();
                        (e, vec![])
                    }
                };
            }
            (0x01, 0x0b) => {
                let g = dev.read().unwrap();
                return match g.h.set_alternate_setting(idx as u8, val as u8) {
                    Ok(()) => (0, vec![]),
                    Err(e) => (g.err(e), vec![]),
                };
            }
            // vhci's port-reset: a SET_FEATURE(PORT_RESET) aimed at the (virtual) hub port.
            (0x23, 0x03) if val == 4 => {
                let g = dev.read().unwrap();
                return match g.h.reset() {
                    Ok(()) => (0, vec![]),
                    Err(e) => (g.err(e), vec![]),
                };
            }
            (0x02, 0x01) if val == 0 => {
                let g = dev.read().unwrap();
                return match g.h.clear_halt(idx as u8) {
                    Ok(()) => (0, vec![]),
                    Err(e) => (g.err(e), vec![]),
                };
            }
            _ => {}
        }
        let t = Duration::from_secs(5);
        let g = dev.read().unwrap();
        // A host that re-reads the device descriptor is re-enumerating the device (what a port reset leads to). Some
        // importers (usbip-win2) only reset their *virtual* device, so a stick left mid-command (an aborted URB, an
        // unread status block) would stay out of sync with the new host state. Do the device-side half ourselves.
        if rt == 0x80 && req == 0x06 && (val >> 8) == 0x01 && g.dirty.swap(false, Ordering::Relaxed) {
            trace!("importer re-enumerated after bulk traffic: resetting the device's mass-storage state");
            g.bot_recover();
        }
        return if rt & 0x80 != 0 {
            let mut buf = vec![0u8; len];
            match g.h.read_control(rt, req, val, idx, &mut buf, t) {
                Ok(n) => {
                    buf.truncate(n);
                    (0, buf)
                }
                Err(e) => (g.err(e), vec![]),
            }
        } else {
            match g.h.write_control(rt, req, val, idx, &out, t) {
                Ok(_) => (0, vec![]),
                Err(e) => (g.err(e), vec![]),
            }
        };
    }
    let addr = (c.basic.ep as u8) | if is_in { 0x80 } else { 0x00 };
    let (ttype, mps) = {
        let g = dev.read().unwrap();
        g.dirty.store(true, Ordering::Relaxed);
        g.eps.get(&addr).copied().unwrap_or((TransferType::Bulk, 512))
    };
    if is_in {
        let mut buf = vec![0u8; len];
        let step = if ttype == TransferType::Interrupt { Duration::from_millis(100) } else { Duration::from_millis(1000) };
        let mut got = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return (ECONNRESET, vec![]);
            }
            let g = dev.read().unwrap();
            let began = std::time::Instant::now();
            let r = if ttype == TransferType::Interrupt { g.h.read_interrupt(addr, &mut buf[got..], step) } else { g.h.read_bulk(addr, &mut buf[got..], step) };
            drop(g);
            match r {
                Ok(n) => {
                    got += n;
                    // libusb reports bytes received before a timeout as a success. A transfer that took the whole step, is not full and
                    // ended on a whole packet was cut off mid-flight (a slow flash drive pausing), not finished: a real short transfer
                    // ends on a packet smaller than the endpoint's. Keep reading, or the rest would be taken for the next command's data.
                    if got < len && n > 0 && n % mps == 0 && began.elapsed() >= step {
                        continue;
                    }
                    buf.truncate(got);
                    return (0, buf);
                }
                Err(rusb::Error::Timeout) if got > 0 => continue,
                Err(rusb::Error::Timeout) => continue,
                Err(e) => return (dev.read().unwrap().err(e), vec![]), // `g` was dropped before the transfer result was looked at
            }
        }
    } else {
        let g = dev.read().unwrap();
        let t = Duration::from_secs(10);
        let r = if ttype == TransferType::Interrupt { g.h.write_interrupt(addr, &out, t) } else { g.h.write_bulk(addr, &out, t) };
        // For OUT transfers a non-negative status is the number of bytes written.
        match r {
            Ok(n) => (n as i32, vec![]),
            Err(e) => {
                if matches!(e, rusb::Error::Timeout) && g.bot.map(|b| b.ep_out == addr).unwrap_or(false) {
                    g.bot_recover(); // the stick refused a command block for 10 s: it is wedged, un-wedge it
                }
                (g.err(e), vec![])
            }
        }
    }
}
