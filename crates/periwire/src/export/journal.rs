//! Reading the NTFS journal through the device to protect it while shared read-only.
use super::*;

/// Reads whole sectors from a mass-storage drive with plain Bulk-Only READ commands, before a session starts and while
/// nothing else uses the device. Used only to look at the drive's structure (see `ntfs`); it never writes. Every read is
/// bounded, and any failure resets the Bulk-Only state so the drive is left exactly as it was found.
pub(super) struct DriveReader<'a> {
    pub(super) dev: &'a Dev,
    pub(super) tag: u32,
    pub(super) started: std::time::Instant,
}
impl DriveReader<'_> {
    pub(super) fn attempt(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>> {
        let b = self.dev.bot?;
        let len = sectors as usize * 512;
        self.tag = self.tag.wrapping_add(1);
        let mut cbw = [0u8; 31];
        cbw[..4].copy_from_slice(b"USBC");
        cbw[4..8].copy_from_slice(&self.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        cbw[12] = 0x80; // data in
        if lba <= u32::MAX as u64 && sectors <= 0xffff {
            cbw[14] = 10; // READ(10): understood by every stick
            cbw[15] = 0x28;
            cbw[17..21].copy_from_slice(&(lba as u32).to_be_bytes());
            cbw[22..24].copy_from_slice(&(sectors as u16).to_be_bytes());
        } else {
            cbw[14] = 16;
            cbw[15] = 0x88;
            cbw[17..25].copy_from_slice(&lba.to_be_bytes());
            cbw[25..29].copy_from_slice(&sectors.to_be_bytes());
        }
        let h = &self.dev.h;
        if h.write_bulk(b.ep_out, &cbw, Duration::from_secs(2)).ok()? != 31 {
            return None;
        }
        let mut data = vec![0u8; len];
        let mut got = 0;
        while got < len {
            let n = h.read_bulk(b.ep_in, &mut data[got..], Duration::from_secs(5)).ok()?;
            if n == 0 {
                return None;
            }
            got += n;
        }
        let mut csw = [0u8; CSW_LEN];
        if h.read_bulk(b.ep_in, &mut csw, Duration::from_secs(2)).ok()? != CSW_LEN || !csw.starts_with(b"USBS") || csw[4..8] != self.tag.to_le_bytes() || csw[12] != 0 {
            return None;
        }
        Some(data)
    }
}
impl crate::ntfs::SectorSource for DriveReader<'_> {
    fn read(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>> {
        if sectors == 0 || sectors > 64 || self.started.elapsed() > Duration::from_secs(8) {
            return None;
        }
        let r = self.attempt(lba, sectors);
        if r.is_none() {
            self.dev.bot_recover(); // leave the drive in a clean Bulk-Only state whatever went wrong
        }
        r
    }
}

/// For a read-only session that asked for it: look at the drive's NTFS volumes and decide what to show instead of an
/// emptied journal. Returns the patches for the guard. Anything unclear means no patch.
pub(super) fn journal_patches(dev: &Dev, busid: &str, peer: &str) -> Vec<Patch> {
    use crate::ntfs::{clean_head, scan, State};
    let mut src = DriveReader { dev, tag: 0x5057_0000, started: std::time::Instant::now() };
    let stamp = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(1)) as u32;
    let mut patches = vec![];
    for v in scan(&mut src) {
        match v.state {
            State::Emptied => {
                crate::logln!("[{peer}] {busid}: the NTFS journal of the volume at sector {} was emptied by another operating system; showing a valid clean journal header so Windows can mount it read-only (nothing on the drive is changed)", v.start_lba);
                patches.push(Patch { lba: v.log_lba, bytes: clean_head(v.log_size, stamp).to_vec() });
            }
            State::Unfinished => crate::logln!("[{peer}] {busid}: the NTFS volume at sector {} has unfinished changes in its journal; Windows may refuse to mount it read-only until it has been ejected safely from a Windows computer", v.start_lba),
            State::Clean | State::Unknown => {}
        }
    }
    patches
}
