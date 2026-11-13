//! Importer plumbing: connecting to exporters, listing their devices, and (Linux) the vhci_hcd interface.
//! On Linux the device is handed to the kernel's `vhci_hcd` virtual host controller (OS-native);
//! we relay its USB/IP byte stream over mutual TLS via a loopback socket.
use crate::{identity, tls};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use usbip_proto::*;

pub async fn connect(addr: &str) -> Result<TlsStream<TcpStream>> {
    let id = identity::load_or_create()?;
    let cfg = tls::client_config(&id, tls::Pinned::new(identity::load_trusted()?, "server"))?;
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connecting to {addr}: timed out"))?
        .with_context(|| format!("connecting to {addr}"))?;
    tcp.set_nodelay(true)?;
    let name = rustls::pki_types::ServerName::try_from("periwire")?;
    let s = tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(cfg).connect(name, tcp))
        .await
        .map_err(|_| anyhow::anyhow!("TLS handshake with {addr} timed out"))?
        .context("TLS handshake failed (is that machine's fingerprint trusted? see `periwire trust --help`)")?;
    Ok(s)
}

/// Tell the exporter how this session should behave before the import request (PeriWire extension): write-protect it
/// (`FLAG_READ_ONLY`) and/or use the storage round-trip savings (`FLAG_ACCEL`). Returns the optional features the exporter
/// agreed to (`FLAG_ACCEL`, `FLAG_NTFS_JOURNAL`, `FLAG_RESUME`). Does nothing when `flags` is 0, so plain USB/IP exporters keep working.
pub async fn request_flags<S: AsyncReadExt + AsyncWriteExt + Unpin>(s: &mut S, flags: u32) -> Result<u32> {
    if flags == 0 {
        return Ok(0);
    }
    s.write_all(&encode_flags_request(flags)).await?;
    let mut h = [0u8; 8];
    s.read_exact(&mut h).await.map_err(|_| anyhow::anyhow!("the other computer's PeriWire is too old for this session's options (update it there, or set PERIWIRE_ACCEL=0 here)"))?;
    let rep = OpHeader::decode(&h)?;
    if rep.code != OP_REP_FLAGS || rep.status & !(FLAG_ACCEL | FLAG_NTFS_JOURNAL | FLAG_RESUME) != 0 {
        bail!("the other computer did not accept the session options");
    }
    Ok(rep.status & (FLAG_ACCEL | FLAG_NTFS_JOURNAL | FLAG_RESUME))
}

/// Whether to ask the exporter to show a clean NTFS journal header in place of an emptied one (read-only sessions only; see
/// `ntfs`). Windows needs it, Linux and macOS do not, so it is on by default only on Windows. `PERIWIRE_NTFS_JOURNAL=1`
/// or `=0` overrides that.
pub fn journal_fix_wanted() -> bool {
    match std::env::var("PERIWIRE_NTFS_JOURNAL").as_deref() {
        Ok("1") | Ok("on") | Ok("true") => true,
        Ok("0") | Ok("off") | Ok("false") => false,
        _ => cfg!(windows),
    }
}

/// The session options this computer asks for: `read_only`, the round-trip savings unless `PERIWIRE_ACCEL=0`, the
/// NTFS journal fix where it is wanted, and, for a connection that reconnects by itself (`resume`), to be waited for
/// after a lost connection. Only ask for `resume` if this importer closes the connection cleanly when it detaches on purpose.
pub fn session_flags(read_only: bool, resume: bool) -> u32 {
    (if resume { FLAG_RESUME } else { 0 })
        | (if read_only { FLAG_READ_ONLY } else { 0 })
        | (if crate::accel::wanted() { FLAG_ACCEL } else { 0 })
        | (if journal_fix_wanted() { FLAG_NTFS_JOURNAL } else { 0 })
}

/// A readable reason for a refused import (`status` from `OP_REP_IMPORT`).
pub fn refusal(addr: &str, busid: &str, status: u32) -> anyhow::Error {
    if status == IMPORT_READ_ONLY_UNSUPPORTED {
        anyhow::anyhow!("{busid} on {addr} is not plain USB storage, so it cannot be write-protected; turn read-only off to use it, or leave it alone")
    } else {
        anyhow::anyhow!("{addr} refused to export {busid} (not shared, in use by someone else, or the device could not be claimed; see the exporter's log)")
    }
}

/// Connect to `addr` and return the fingerprint it presents, WITHOUT trusting it.
/// The caller must have a human compare it with `periwire id` on that machine before calling `identity::add_trusted`.
pub async fn probe_fingerprint(addr: &str) -> Result<String> {
    let id = identity::load_or_create()?;
    let slot = Arc::new(Mutex::new(None));
    let cfg = tls::client_config(&id, tls::Pinned::learning(slot.clone()))?;
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connecting to {addr}: timed out"))?
        .with_context(|| format!("connecting to {addr}"))?;
    let name = rustls::pki_types::ServerName::try_from("periwire")?;
    let _ = tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(cfg).connect(name, tcp)).await;
    let fp = slot.lock().unwrap().clone().context("server presented no certificate (is it a periwire exporter?)")?;
    Ok(fp)
}

/// Devices a remote exporter shares with us, with human-readable info (name, kind, label) when the exporter
/// supports it (PeriWire exporters do); plain USB/IP exporters get best-effort kinds from the descriptors.
pub async fn list_remote(addr: &str) -> Result<Vec<Value>> {
    match list_devinfo(addr).await {
        Ok(v) => Ok(v),
        Err(_) => list_devlist(addr).await,
    }
}

async fn list_devinfo(addr: &str) -> Result<Vec<Value>> {
    let mut s = connect(addr).await?;
    s.write_all(&OpHeader::new(OP_REQ_DEVINFO, 0).encode()).await?;
    let mut h = [0u8; 8];
    s.read_exact(&mut h).await?;
    let rep = OpHeader::decode(&h)?;
    if rep.code != OP_REP_DEVINFO || rep.status != 0 {
        bail!("exporter does not support device info");
    }
    let mut n = [0u8; 4];
    s.read_exact(&mut n).await?;
    let len = u32::from_be_bytes(n) as usize;
    if len > 4 * 1024 * 1024 {
        bail!("device info too large");
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    let _ = s.shutdown().await;
    let mut v: Vec<Value> = serde_json::from_slice(&body)?;
    for d in v.iter_mut() {
        // These describe the exporter's local state, not ours.
        if let Some(o) = d.as_object_mut() {
            o.remove("mounted");
            o.remove("shared");
        }
    }
    Ok(v)
}

async fn list_devlist(addr: &str) -> Result<Vec<Value>> {
    let mut s = connect(addr).await?;
    s.write_all(&OpHeader::new(OP_REQ_DEVLIST, 0).encode()).await?;
    let mut h = [0u8; 8];
    s.read_exact(&mut h).await?;
    let rep = OpHeader::decode(&h)?;
    if rep.status != 0 {
        bail!("server refused (status {})", rep.status);
    }
    let mut n = [0u8; 4];
    s.read_exact(&mut n).await?;
    let n = u32::from_be_bytes(n);
    let mut out = vec![];
    for _ in 0..n {
        let mut b = [0u8; DEVICE_LEN];
        s.read_exact(&mut b).await?;
        let d = UsbDevice::decode(&b)?;
        let mut ifs = vec![];
        for _ in 0..d.num_interfaces {
            let mut i = [0u8; 4];
            s.read_exact(&mut i).await?;
            ifs.push((i[0], i[1], i[2]));
        }
        let kind = crate::classify::classify(d.id_vendor, d.id_product, d.device_class, &ifs, "");
        let first = ifs.first().map(|i| i.0).unwrap_or(0);
        out.push(json!({
            "busid": d.busid, "vid": format!("{:04x}", d.id_vendor), "pid": format!("{:04x}", d.id_product),
            "manufacturer": null, "product": null, "serial": null, "label": null,
            "kind": kind.id, "kind_label": kind.label, "display": kind.label,
            "speed": crate::export::speed_name(d.speed), "interfaces": ifs.iter().map(|i| crate::export::class_name(i.0)).collect::<Vec<_>>(),
            "class": crate::export::class_name(if d.device_class == 0 { first } else { d.device_class }),
        }));
    }
    let _ = s.shutdown().await; // TLS close_notify so the server doesn't log an abrupt close
    Ok(out)
}

#[cfg(target_os = "linux")]
pub mod vhci {
    use super::*;
    use std::path::PathBuf;

    fn base() -> PathBuf {
        PathBuf::from("/sys/devices/platform/vhci_hcd.0")
    }

    pub fn loaded() -> bool {
        base().exists()
    }

    /// Whether the module exists on disk (so `modprobe` can load it).
    pub fn available() -> bool {
        if loaded() {
            return true;
        }
        let rel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let dir = format!("/lib/modules/{}/kernel/drivers/usb/usbip", rel.trim());
        std::fs::read_dir(dir).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with("vhci-hcd"))).unwrap_or(false)
    }

    pub fn ensure_loaded() -> Result<()> {
        if loaded() {
            return Ok(());
        }
        let ok = std::process::Command::new("modprobe").arg("vhci-hcd").status().map(|s| s.success()).unwrap_or(false);
        if !ok || !loaded() {
            bail!("the kernel's vhci_hcd module is not available (root is needed to load it: `modprobe vhci-hcd`)");
        }
        Ok(())
    }

    /// (hub, port, status, rest-of-line) for every vhci port across controllers.
    pub fn ports() -> Result<Vec<(String, u32, u32, String)>> {
        let mut out = vec![];
        let mut files: Vec<PathBuf> = vec![base().join("status")];
        for i in 1..8 {
            let p = base().join(format!("status.{i}"));
            if p.exists() {
                files.push(p);
            }
        }
        for f in files {
            for line in std::fs::read_to_string(&f)?.lines().skip(1) {
                let c: Vec<&str> = line.split_whitespace().collect();
                if c.len() < 3 {
                    continue;
                }
                out.push((c[0].to_string(), c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0), c[3..].join(" ")));
            }
        }
        Ok(out)
    }

    pub fn free_port(speed: u32) -> Result<u32> {
        let want = if speed >= 5 { "ss" } else { "hs" };
        ports()?.into_iter().find(|(hub, _, sta, _)| hub == want && *sta == 4).map(|(_, p, _, _)| p).with_context(|| format!("no free {want} vhci port"))
    }

    /// The device's busid on this machine (e.g. `3-1`) once attached to `port`.
    pub fn local_busid(port: u32) -> Option<String> {
        ports().ok()?.into_iter().find(|(_, p, sta, _)| *p == port && *sta != 4).and_then(|(_, _, _, rest)| rest.split_whitespace().nth(3).map(str::to_string)).filter(|b| b != "0-0")
    }

    pub fn attach(port: u32, sockfd: i32, devid: u32, speed: u32) -> Result<()> {
        std::fs::write(base().join("attach"), format!("{port} {sockfd} {devid} {speed}")).context("writing vhci attach (needs root)")
    }

    pub fn detach(port: u32) -> Result<()> {
        std::fs::write(base().join("detach"), format!("{port}")).context("writing vhci detach (needs root)")
    }

    pub fn port_free(port: u32) -> bool {
        ports().map(|p| p.iter().any(|(_, n, sta, _)| *n == port && *sta == 4)).unwrap_or(true)
    }

    /// Flush and unmount every filesystem on the attached device. Without `force` a busy mount aborts the eject
    /// (nothing is detached); with `force` busy mounts are lazily detached.
    pub fn unmount(local_busid: &str, force: bool) -> Result<Vec<String>> {
        let mounts = crate::export::mounts_for(local_busid);
        if mounts.is_empty() {
            return Ok(vec![]);
        }
        unsafe { libc::sync() };
        let mut done = vec![];
        for (_dev, mp) in mounts {
            let c = std::ffi::CString::new(mp.clone())?;
            let mut r = unsafe { libc::umount2(c.as_ptr(), 0) };
            if r != 0 && force {
                r = unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
            }
            if r != 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EBUSY) {
                    bail!("{mp} is busy (files are open on it). Close them and retry, or force the eject.");
                }
                bail!("could not unmount {mp}: {e}");
            }
            done.push(mp);
        }
        Ok(done)
    }
}
