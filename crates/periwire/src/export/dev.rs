//! An opened device being exported: claiming interfaces, resetting, mass-storage recovery and error mapping.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Bot {
    pub(super) iface: u8,
    pub(super) ep_in: u8,
    pub(super) ep_out: u8,
}

/// True when any interface of the device's first configuration is USB mass storage (class 8).
pub(super) fn is_mass_storage(dev: &Device<Context>) -> bool {
    dev.config_descriptor(0).map(|c| c.interfaces().any(|i| i.descriptors().any(|a| a.class_code() == 0x08))).unwrap_or(false)
}

/// After a reset the device may still be re-enumerating; control requests sent too early time out (5 s each),
/// which stalls an importer's first enumeration. Wait (bounded) until it answers a device-descriptor read.
pub(super) fn settle(h: &DeviceHandle<Context>) {
    let start = std::time::Instant::now();
    let mut buf = [0u8; 18];
    while start.elapsed() < Duration::from_secs(4) {
        if h.read_control(0x80, 6, 0x0100, 0, &mut buf, Duration::from_millis(250)).is_ok() {
            trace!("device answered after {:?}", start.elapsed());
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    trace!("device still silent after reset (continuing)");
}

/// An opened, fully claimed device. Dropping it releases the interfaces, which re-attaches the
/// original kernel driver (so the device returns to the local machine).
pub(super) struct Dev {
    pub(super) h: DeviceHandle<Context>,
    pub(super) dev: Device<Context>,
    pub(super) claimed: Vec<u8>,
    /// Endpoint address -> (transfer type, max packet size).
    pub(super) eps: HashMap<u8, (TransferType, usize)>,
    /// The mass-storage Bulk-Only interface, if any (used for reset recovery).
    pub(super) bot: Option<Bot>,
    /// Set by every bulk transfer; cleared by recovery. A device-descriptor read while set means the importer re-enumerated.
    pub(super) dirty: std::sync::atomic::AtomicBool,
    /// Set when libusb says the device itself is gone (unplugged, or it dropped off the bus and came back under a new address,
    /// which Apple devices do after the host's mode-switch request). This handle can never work again.
    pub(super) gone: std::sync::atomic::AtomicBool,
    /// The "unlock the device" hint has been logged for this session (see `config_refused`).
    pub(super) hinted: std::sync::atomic::AtomicBool,
    pub(super) config: u8,
}

impl Dev {
    pub(super) fn open(dev: Device<Context>) -> Result<Self> {
        let (bus, ports) = (dev.bus_number(), dev.port_numbers().unwrap_or_default());
        let storage = is_mass_storage(&dev);
        let h = dev.open()?;
        let _ = h.set_auto_detach_kernel_driver(true);
        let mut me = Dev {
            h,
            dev,
            claimed: vec![],
            eps: HashMap::new(),
            config: 0,
            bot: None,
            dirty: std::sync::atomic::AtomicBool::new(false),
            gone: std::sync::atomic::AtomicBool::new(false),
            hinted: std::sync::atomic::AtomicBool::new(false),
        };
        if storage {
            // Reset first, then claim: this order is the one that works on real sticks. Claiming a stick that the kernel's
            // storage driver is still bound to, without a reset first, makes some of them stop answering within 100 ms.
            // (Only mass storage is reset: other devices, Apple's for one, re-enumerate and have no stuck-stick problem.)
            match me.h.reset() {
                Ok(()) => trace!("device reset ok"),
                Err(e @ (rusb::Error::NoDevice | rusb::Error::NotFound)) => {
                    trace!("device re-enumerated after the reset ({e}); reopening it");
                    me.reopen(bus, &ports)?;
                }
                Err(e) => trace!("device reset: {e} (continuing)"),
            }
            settle(&me.h);
        } else {
            // Other devices are left alone: many (Apple's, for one) drop off the bus and come back when reset, which
            // would invalidate this handle, and they have no stuck-stick problem to cure.
            trace!("not mass storage: no reset");
        }
        if me.dev.active_config_descriptor().is_err() {
            let first = me.dev.config_descriptor(0)?.number();
            me.h.set_active_configuration(first)?;
        }
        me.claim_all()?;
        Ok(me)
    }

    /// The device came back under a new address after a reset: find it again by its bus and port path and reopen it.
    pub(super) fn reopen(&mut self, bus: u8, ports: &[u8]) -> Result<()> {
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            let ctx = Context::new()?;
            for d in ctx.devices()?.iter() {
                if d.bus_number() == bus && d.port_numbers().unwrap_or_default() == ports {
                    if let Ok(h) = d.open() {
                        let _ = h.set_auto_detach_kernel_driver(true);
                        self.h = h;
                        self.dev = d;
                        return Ok(());
                    }
                }
            }
        }
        bail!("the device did not come back after being reset")
    }

    pub(super) fn claim_all(&mut self) -> Result<()> {
        let cfg = self.dev.active_config_descriptor()?;
        self.config = cfg.number();
        self.eps.clear();
        self.bot = None;
        self.claimed.clear(); // claiming an already-claimed interface is a no-op; the list is rebuilt
        for i in cfg.interfaces() {
            for alt in i.descriptors() {
                for ep in alt.endpoint_descriptors() {
                    self.eps.entry(ep.address()).or_insert((ep.transfer_type(), (ep.max_packet_size() as usize & 0x7ff).max(1)));
                }
                if self.bot.is_none() && alt.class_code() == 0x08 && alt.sub_class_code() == 0x06 && alt.protocol_code() == 0x50 {
                    let bulk = |d: rusb::Direction| alt.endpoint_descriptors().find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == d).map(|e| e.address());
                    if let (Some(ep_in), Some(ep_out)) = (bulk(rusb::Direction::In), bulk(rusb::Direction::Out)) {
                        self.bot = Some(Bot { iface: alt.interface_number(), ep_in, ep_out });
                    }
                }
            }
            self.h.claim_interface(i.number()).map_err(|e| anyhow!("claim interface {}: {e}", i.number()))?;
            self.claimed.push(i.number());
        }
        Ok(())
    }

    /// Bulk-Only Transport "reset recovery" (USB Mass Storage BOT spec 5.3.4): Mass Storage Reset plus clearing both
    /// bulk halts. A host does this itself when a stick stops answering, but an importer whose USB stack never sees the
    /// real endpoint state (usbip-win2) does not, so a stick wedged by one bad exchange would refuse commands forever.
    pub(super) fn bot_recover(&self) {
        if let Some(b) = self.bot {
            let r = self.h.write_control(0x21, 0xFF, 0, b.iface as u16, &[], Duration::from_millis(1000));
            let a = self.h.clear_halt(b.ep_in);
            let c = self.h.clear_halt(b.ep_out);
            // Some sticks keep an unread status block (CSW) through the reset; the next command would then read that
            // stale block instead of its own data (an overflow). Drain whatever is still queued on the IN pipe.
            let mut drained = 0;
            let mut junk = [0u8; 1024];
            while drained < 8 && self.h.read_bulk(b.ep_in, &mut junk, Duration::from_millis(80)).is_ok() {
                drained += 1;
            }
            trace!("BOT reset recovery on interface {}: reset={:?} clear_in={:?} clear_out={:?} drained={}", b.iface, r, a, c, drained);
        }
    }

    /// The device refused a configuration change. Apple devices do that while locked (iOS blocks USB data on a locked device), and the
    /// importer then retries forever with nothing else in the log, so say what is going on, once per session.
    pub(super) fn config_refused(&self, val: u8, e: &rusb::Error) {
        trace!("set configuration {val} refused: {e}");
        let apple = self.dev.device_descriptor().map(|d| d.vendor_id() == 0x05ac).unwrap_or(false);
        if apple && !self.hinted.swap(true, Ordering::SeqCst) {
            crate::logln!("the device refused configuration {val} ({e}). An iPhone or iPad does this while it is locked: unlock it, keep the screen on, and tap Trust if it asks");
        }
    }

    /// A libusb error as a USB/IP status. `NoDevice` also records that the device is gone (see `gone`).
    pub(super) fn err(&self, e: rusb::Error) -> i32 {
        if device_vanished(&e) {
            self.gone.store(true, Ordering::SeqCst);
        }
        map_err(e)
    }

    pub(super) fn release_all(&mut self) {
        for n in self.claimed.drain(..) {
            let _ = self.h.release_interface(n);
        }
    }
}

/// libusb reports a disconnected device as `NoDevice`. (`NotFound` is a missing endpoint or interface, not a vanished device.)
pub(super) fn device_vanished(e: &rusb::Error) -> bool {
    matches!(e, rusb::Error::NoDevice)
}

pub(super) fn map_err(e: rusb::Error) -> i32 {
    match e {
        rusb::Error::Pipe => EPIPE,
        rusb::Error::NoDevice | rusb::Error::NotFound => ENODEV,
        rusb::Error::Timeout => ETIMEDOUT,
        rusb::Error::Overflow => -75,
        rusb::Error::Io => EIO,
        _ => EPROTO,
    }
}
