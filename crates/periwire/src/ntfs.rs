//! NTFS journal handling for read-only sharing.
//!
//! The problem: Windows will not mount an NTFS volume on a write-protected disk if the volume's journal (`$LogFile`) has
//! been *emptied* (filled with 0xFF), which is what Linux's `ntfs3` driver leaves behind after any read-write mount.
//! Windows has to write a fresh restart area to start logging, the write-protected disk refuses it, and the volume stays
//! unmounted ("The media is write protected").
//!
//! The fix keeps the disk read-only the whole time: when a session asks for it, the exporter shows Windows a valid,
//! clean restart area in place of the emptied one, in what it *reads*. Nothing is ever written to the drive, and there is
//! no writable overlay a user could lose files in. This module finds the journal and judges its state (`scan`) and builds
//! the pages Windows would itself write (`clean_head`). It is pure logic over a `SectorSource`, so it is tested without
//! hardware. Anything it does not understand means "no change": it never guesses.
//!
//! The page layout and the way Windows picks its values were taken from volumes formatted by Windows Server 2025 at 17
//! sizes from 8 MB to 1 TB (see `docs/TESTING.md`).

/// The drive is read in 512-byte sectors; other sector sizes are left alone.
pub const SECTOR: usize = 512;
/// Both restart pages: the journal's first 8 KiB.
pub const HEAD: usize = 8192;
pub const HEAD_SECTORS: u32 = (HEAD / SECTOR) as u32;

/// Where the volume's sectors come from. `read` returns exactly `sectors * 512` bytes, or `None` on any failure.
pub trait SectorSource {
    fn read(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>>;
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum State {
    /// A restart area marked clean: Windows mounts it read-only as it is.
    Clean,
    /// A restart area with unfinished work. Windows must replay it (a write), so a read-only mount fails, and it cannot
    /// be faked: say so instead.
    Unfinished,
    /// Both restart pages are all 0xFF: the journal was emptied. This is the case the fix is for.
    Emptied,
    /// Anything else; left alone.
    Unknown,
}

/// One NTFS volume found on the drive.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Volume {
    /// First sector of the partition.
    pub start_lba: u64,
    /// First sector of the journal file and its size in bytes.
    pub log_lba: u64,
    pub log_size: u64,
    pub state: State,
}

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Partition start sectors on the drive: GPT if the protective entry says so, else the MBR's four entries, else the
/// drive itself (a volume with no partition table). At most 16 candidates, each checked by `volume`.
fn partition_starts(src: &mut dyn SectorSource) -> Vec<u64> {
    let mut out = vec![];
    let Some(lba0) = src.read(0, 1) else { return out };
    if lba0.len() != SECTOR {
        return out;
    }
    if lba0[510] == 0x55 && lba0[511] == 0xaa {
        let mut gpt = false;
        for i in 0..4 {
            let e = 446 + i * 16;
            let (kind, start) = (lba0[e + 4], u32le(&lba0, e + 8).unwrap_or(0) as u64);
            if kind == 0xee {
                gpt = true;
            } else if kind != 0 && !matches!(kind, 0x05 | 0x0f | 0x85) && start > 0 {
                out.push(start);
            }
        }
        if gpt {
            out.clear();
            if let Some(h) = src.read(1, 1) {
                if h.len() == SECTOR && &h[0..8] == b"EFI PART" {
                    let entries_lba = u64le(&h, 72).unwrap_or(0);
                    let count = u32le(&h, 80).unwrap_or(0).min(128) as u64;
                    let size = u32le(&h, 84).unwrap_or(0) as u64;
                    if entries_lba >= 2 && size >= 128 && size <= 512 && size % 8 == 0 {
                        let sectors = ((count * size + 511) / 512).clamp(1, 32) as u32;
                        if let Some(t) = src.read(entries_lba, sectors) {
                            for i in 0..count as usize {
                                let e = i * size as usize;
                                let Some(ent) = t.get(e..e + 128) else { break };
                                if ent[..16].iter().any(|&b| b != 0) {
                                    let first = u64le(ent, 32).unwrap_or(0);
                                    if first > 0 {
                                        out.push(first);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out.push(0); // a volume with no partition table at all
    out.sort_unstable();
    out.dedup();
    out.truncate(16);
    out
}

/// Parse the journal's location out of MFT record 2 (`$LogFile`): the first run of its unnamed `$DATA`, and its size.
/// `rec` is the raw record (`recsz` bytes). Returns (first cluster, clusters in the first run, data size in bytes).
fn log_extent(rec: &[u8], recsz: usize) -> Option<(u64, u64, u64)> {
    if rec.len() != recsz || &rec[0..4] != b"FILE" {
        return None;
    }
    // update-sequence fixups: the last two bytes of every sector hold the sequence number, the real bytes are in the array
    let (usa_ofs, usa_cnt) = (u16le(rec, 4)? as usize, u16le(rec, 6)? as usize);
    if usa_cnt != recsz / SECTOR + 1 || usa_ofs + 2 * usa_cnt > recsz {
        return None;
    }
    let mut r = rec.to_vec();
    let usn = u16le(&r, usa_ofs)?;
    for i in 1..usa_cnt {
        let end = i * SECTOR - 2;
        if u16le(&r, end)? != usn {
            return None; // torn or corrupt record
        }
        let real = [r[usa_ofs + 2 * i], r[usa_ofs + 2 * i + 1]];
        r[end..end + 2].copy_from_slice(&real);
    }
    if u16le(&r, 0x16)? & 1 == 0 {
        return None; // not in use
    }
    let mut p = u16le(&r, 0x14)? as usize;
    let mut found = None;
    let mut name_ok = true;
    for _ in 0..32 {
        let t = u32le(&r, p)?;
        if t == 0xffff_ffff {
            break;
        }
        let len = u32le(&r, p + 4)? as usize;
        if len < 24 || len % 8 != 0 || p + len > recsz {
            return None;
        }
        let (resident, name_len) = (*r.get(p + 8)? == 0, *r.get(p + 9)?);
        if t == 0x30 && resident {
            // $FILE_NAME: insist this is $LogFile
            let content = p + u16le(&r, p + 20)? as usize;
            let n = *r.get(content + 64)? as usize;
            let name: Vec<u16> = (0..n).map(|i| u16le(&r, content + 66 + 2 * i)).collect::<Option<_>>()?;
            if String::from_utf16_lossy(&name) != "$LogFile" {
                name_ok = false;
            }
        }
        if t == 0x80 && !resident && name_len == 0 {
            let runs = p + u16le(&r, p + 32)? as usize;
            let size = u64le(&r, p + 48)?;
            let h = *r.get(runs)?;
            let (ln, on) = ((h & 0x0f) as usize, (h >> 4) as usize);
            if ln == 0 || ln > 8 || on == 0 || on > 8 {
                return None; // empty or sparse first run
            }
            let mut length = 0u64;
            for k in 0..ln {
                length |= (*r.get(runs + 1 + k)? as u64) << (8 * k);
            }
            let mut off = 0u64;
            for k in 0..on {
                off |= (*r.get(runs + 1 + ln + k)? as u64) << (8 * k);
            }
            if off & (1 << (8 * on - 1)) != 0 {
                return None; // the first run's offset is absolute: a negative one is nonsense
            }
            found = Some((off, length, size));
        }
        p += len;
    }
    if !name_ok {
        return None;
    }
    found
}

/// Examine one candidate volume. `None` when it is not a plain, understood NTFS volume.
fn volume(src: &mut dyn SectorSource, start_lba: u64) -> Option<Volume> {
    let bs = src.read(start_lba, 1)?;
    if bs.len() != SECTOR || &bs[3..11] != b"NTFS    " || bs[510] != 0x55 || bs[511] != 0xaa {
        return None;
    }
    if u16le(&bs, 11)? as usize != SECTOR {
        return None; // 4 KiB-sector drives are left alone
    }
    let spc = bs[13] as u64;
    if spc == 0 || !spc.is_power_of_two() || spc > 128 {
        return None;
    }
    let cluster = spc * SECTOR as u64;
    let mft = u64le(&bs, 48)?;
    let cpr = bs[64] as i8;
    let recsz = if cpr > 0 { cpr as u64 * cluster } else { 1u64.checked_shl((-(cpr as i32)) as u32)? };
    if !(SECTOR as u64..=4096).contains(&recsz) || recsz % SECTOR as u64 != 0 || mft == 0 || mft > (1 << 40) {
        return None;
    }
    // MFT record 2 is inside the MFT's first extent (the first records are always contiguous)
    let rec_lba = start_lba.checked_add(mft.checked_mul(spc)?)?.checked_add(2 * recsz / SECTOR as u64)?;
    let rec = src.read(rec_lba, (recsz / SECTOR as u64) as u32)?;
    let (lcn, run_clusters, size) = log_extent(&rec, recsz as usize)?;
    if lcn == 0 || run_clusters.checked_mul(cluster)? < HEAD as u64 || size < 2 * 4096 || size > (1 << 36) {
        return None;
    }
    let log_lba = start_lba.checked_add(lcn.checked_mul(spc)?)?;
    let head = src.read(log_lba, HEAD_SECTORS)?;
    if head.len() != HEAD {
        return None;
    }
    let state = journal_state(&head);
    Some(Volume { start_lba, log_lba, log_size: size, state })
}

/// Judge the journal from its first 8 KiB.
pub fn journal_state(head: &[u8]) -> State {
    if head.len() != HEAD {
        return State::Unknown;
    }
    if head.iter().all(|&b| b == 0xff) {
        return State::Emptied;
    }
    if &head[0..4] == b"RSTR" {
        // restart area flags: bit 1 (RESTART_VOLUME_IS_CLEAN)
        let ra = u16le(head, 0x18).unwrap_or(0) as usize;
        if let Some(flags) = u16le(head, ra + 0x0e) {
            if ra >= 0x1e && ra + 0x30 <= 4096 {
                return if flags & 2 != 0 { State::Clean } else { State::Unfinished };
            }
        }
    }
    State::Unknown
}

/// Find every NTFS volume on the drive and the state of its journal. Reads at most a few dozen sectors; any read that
/// fails simply ends the search for that volume.
pub fn scan(src: &mut dyn SectorSource) -> Vec<Volume> {
    let starts = partition_starts(src);
    starts.into_iter().filter_map(|s| volume(src, s)).collect()
}

/// `66 - floor(log2(size))`: the number of bits Windows reserves for the log sequence number, from the journal size.
/// (Checked against volumes formatted at 17 sizes from 8 MB to 1 TB.)
fn seq_number_bits(size: u64) -> u32 {
    66 - (63 - size.max(1).leading_zeros())
}

/// One restart page, as Windows writes it when it marks a journal clean. `stamp` is the open count (Windows uses the low
/// 32 bits of the current time), `usn` the page's update sequence number.
fn restart_page(log_size: u64, stamp: u32, usn: u16) -> [u8; 4096] {
    let mut p = [0u8; 4096];
    let put = |p: &mut [u8; 4096], o: usize, v: &[u8]| p[o..o + v.len()].copy_from_slice(v);
    put(&mut p, 0, b"RSTR");
    put(&mut p, 4, &0x1eu16.to_le_bytes()); // update sequence array offset
    put(&mut p, 6, &9u16.to_le_bytes()); // entries: the number itself plus eight sectors
    put(&mut p, 8, &0u64.to_le_bytes()); // chkdsk LSN
    put(&mut p, 16, &4096u32.to_le_bytes()); // system page size
    put(&mut p, 20, &4096u32.to_le_bytes()); // log page size
    put(&mut p, 24, &0x30u16.to_le_bytes()); // restart area offset
    put(&mut p, 26, &1u16.to_le_bytes()); // minor version
    put(&mut p, 28, &1u16.to_le_bytes()); // major version
    let bits = seq_number_bits(log_size);
    // Windows' own pattern right after it starts a journal: sequence number 2, and an offset of 0x6111 eight-byte units
    let lsn = (2u64 << (64 - bits)) | 0x6111;
    let ra = 0x30;
    put(&mut p, ra, &lsn.to_le_bytes()); // current LSN
    put(&mut p, ra + 8, &1u16.to_le_bytes()); // log clients
    put(&mut p, ra + 10, &0xffffu16.to_le_bytes()); // client free list (none)
    put(&mut p, ra + 12, &0u16.to_le_bytes()); // client in-use list: client 0
    put(&mut p, ra + 14, &2u16.to_le_bytes()); // flags: RESTART_VOLUME_IS_CLEAN
    put(&mut p, ra + 16, &bits.to_le_bytes());
    put(&mut p, ra + 20, &224u16.to_le_bytes()); // restart area length
    put(&mut p, ra + 22, &64u16.to_le_bytes()); // client array offset
    put(&mut p, ra + 24, &log_size.to_le_bytes());
    put(&mut p, ra + 32, &112u32.to_le_bytes()); // last LSN data length
    put(&mut p, ra + 36, &48u16.to_le_bytes()); // log record header length
    put(&mut p, ra + 38, &64u16.to_le_bytes()); // log page data offset
    put(&mut p, ra + 40, &stamp.to_le_bytes()); // restart log open count
    let c = ra + 64; // the one client record, "NTFS"
    put(&mut p, c, &(lsn - 13).to_le_bytes()); // oldest LSN
    put(&mut p, c + 8, &lsn.to_le_bytes()); // client restart LSN
    put(&mut p, c + 16, &0xffffu16.to_le_bytes()); // previous client
    put(&mut p, c + 18, &0xffffu16.to_le_bytes()); // next client
    put(&mut p, c + 20, &0u16.to_le_bytes()); // sequence number
    put(&mut p, c + 28, &8u32.to_le_bytes()); // client name length in bytes
    for (i, ch) in "NTFS".encode_utf16().enumerate() {
        put(&mut p, c + 32 + 2 * i, &ch.to_le_bytes());
    }
    // update sequence array: the sequence number, then the original last two bytes of each sector (all zero here), which
    // are replaced in the page itself by the sequence number
    put(&mut p, 0x1e, &usn.to_le_bytes());
    for s in 1..=8 {
        put(&mut p, s * SECTOR - 2, &usn.to_le_bytes());
    }
    p
}

/// The journal's first 8 KiB as Windows would leave it after a clean dismount: two restart pages. `log_size` is the
/// journal file's size in bytes; `stamp` any 32-bit value (use the low 32 bits of the current time, as Windows does).
pub fn clean_head(log_size: u64, stamp: u32) -> [u8; HEAD] {
    let mut out = [0u8; HEAD];
    out[..4096].copy_from_slice(&restart_page(log_size, stamp, 9));
    out[4096..].copy_from_slice(&restart_page(log_size, stamp, 8));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An in-memory drive made of extents; anything unmapped reads as zeros.
    #[derive(Default)]
    struct Img {
        ext: Vec<(u64, Vec<u8>)>,
        reads: u32,
        fail_at: Option<u64>,
    }
    impl Img {
        fn put(&mut self, byte_off: u64, data: &[u8]) {
            self.ext.push((byte_off, data.to_vec()));
        }
    }
    impl SectorSource for Img {
        fn read(&mut self, lba: u64, sectors: u32) -> Option<Vec<u8>> {
            self.reads += 1;
            if self.fail_at == Some(lba) {
                return None;
            }
            let (start, len) = (lba * 512, sectors as usize * 512);
            let mut out = vec![0u8; len];
            for (o, d) in &self.ext {
                let (a, b) = (start.max(*o), (start + len as u64).min(*o + d.len() as u64));
                if a < b {
                    out[(a - start) as usize..(b - start) as usize].copy_from_slice(&d[(a - o) as usize..(b - o) as usize]);
                }
            }
            Some(out)
        }
    }

    /// A boot sector for a volume with 4 KiB clusters and 1 KiB MFT records, MFT at cluster `mft`.
    fn boot(mft: u64) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[3..11].copy_from_slice(b"NTFS    ");
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 8;
        b[48..56].copy_from_slice(&mft.to_le_bytes());
        b[64] = 0xf6; // -10: 2^10 = 1024-byte records
        b[510] = 0x55;
        b[511] = 0xaa;
        b
    }

    /// MFT record 2 ($LogFile) with its journal at cluster `lcn`, `clusters` long, `size` bytes of data.
    fn record2(lcn: u64, clusters: u64, size: u64) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes()); // update sequence array offset
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // 1 + 2 sectors
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // first attribute
        r[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // in use
        // $FILE_NAME (resident), content 66 + 16 bytes of name
        let a = 0x38;
        r[a..a + 4].copy_from_slice(&0x30u32.to_le_bytes());
        r[a + 4..a + 8].copy_from_slice(&112u32.to_le_bytes());
        r[a + 16..a + 20].copy_from_slice(&82u32.to_le_bytes()); // content length
        r[a + 20..a + 22].copy_from_slice(&24u16.to_le_bytes()); // content offset
        let c = a + 24;
        r[c + 64] = 8;
        for (i, ch) in "$LogFile".encode_utf16().enumerate() {
            r[c + 66 + 2 * i..c + 68 + 2 * i].copy_from_slice(&ch.to_le_bytes());
        }
        // $DATA (non-resident), runlist: one run
        let d = a + 112;
        r[d..d + 4].copy_from_slice(&0x80u32.to_le_bytes());
        r[d + 4..d + 8].copy_from_slice(&80u32.to_le_bytes());
        r[d + 8] = 1; // non-resident
        r[d + 32..d + 34].copy_from_slice(&64u16.to_le_bytes()); // runlist offset
        r[d + 48..d + 56].copy_from_slice(&size.to_le_bytes());
        let mut runs = vec![];
        let ln = (8 - (clusters.leading_zeros() as usize) / 8).max(1);
        let on = (8 - (lcn.leading_zeros() as usize) / 8).max(1) + 1; // a spare byte keeps the offset positive
        runs.push(((on << 4) | ln) as u8);
        runs.extend_from_slice(&clusters.to_le_bytes()[..ln]);
        runs.extend_from_slice(&lcn.to_le_bytes()[..on]);
        r[d + 64..d + 64 + runs.len()].copy_from_slice(&runs);
        let e = d + 80;
        r[e..e + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        // update sequence: number 7, real sector ends are zero here
        r[0x30..0x32].copy_from_slice(&7u16.to_le_bytes());
        r[510..512].copy_from_slice(&7u16.to_le_bytes());
        r[1022..1024].copy_from_slice(&7u16.to_le_bytes());
        r
    }

    /// A drive with one NTFS volume at sector `start`, journal at cluster 78, holding `head` in its first 8 KiB.
    fn drive(start: u64, head: &[u8]) -> Img {
        let mut img = Img::default();
        img.put(start * 512, &boot(4));
        img.put(start * 512 + 4 * 4096 + 2 * 1024, &record2(78, 512, 2 * 1024 * 1024));
        img.put(start * 512 + 78 * 4096, head);
        img
    }

    fn mbr(start: u32) -> Vec<u8> {
        let mut m = vec![0u8; 512];
        m[446 + 4] = 7;
        m[446 + 8..446 + 12].copy_from_slice(&start.to_le_bytes());
        m[446 + 12..446 + 16].copy_from_slice(&100_000u32.to_le_bytes());
        m[510] = 0x55;
        m[511] = 0xaa;
        m
    }

    #[test]
    fn the_sequence_bits_follow_windows_for_every_size_it_was_seen_to_use() {
        // (journal size, seq_number_bits) as written by Windows Server 2025 for volumes of 8 MB .. 1 TB
        for (size, bits) in [(2097152u64, 45), (2686976, 45), (4341760, 44), (5013504, 44), (6356992, 44), (9043968, 43), (14417920, 43), (25149440, 42), (42467328, 41), (67108864, 40)] {
            assert_eq!(seq_number_bits(size), bits, "{size}");
        }
    }

    /// The first bytes of a restart page Windows wrote for the 8 MB and the 29,601 MB volume (the open count, LSNs and
    /// sequence numbers vary and are masked out below).
    const WINDOWS_8MB: &str = "52535452 1e000900 00000000 00000000 00100000 00100000 30000100 01000900 00000000 00000000 00000000 00000000";
    #[test]
    fn the_fixed_part_of_the_page_matches_what_windows_wrote() {
        let h: Vec<u8> = WINDOWS_8MB.split(' ').flat_map(|w| (0..4).map(move |i| u8::from_str_radix(&w[2 * i..2 * i + 2], 16).unwrap())).collect();
        let p = restart_page(2097152, 0, 9);
        assert_eq!(&p[..0x30], &h[..0x30], "header and update sequence array");
        // restart area (Windows' bytes for the 8 MB volume: 155c1000 00000000 0100ffff 00000200 2d000000 e0004000 00002000 00000000 70000000 30004000 ...)
        assert_eq!(&p[0x38..0x40], &[1, 0, 0xff, 0xff, 0, 0, 2, 0]);
        assert_eq!(u32le(&p, 0x40), Some(45));
        assert_eq!((u16le(&p, 0x44), u16le(&p, 0x46)), (Some(224), Some(64)));
        assert_eq!(u64le(&p, 0x48), Some(2097152));
        assert_eq!((u32le(&p, 0x50), u16le(&p, 0x54), u16le(&p, 0x56)), (Some(112), Some(48), Some(64)));
        // client record
        assert_eq!(&p[0x80..0x86], &[0xff, 0xff, 0xff, 0xff, 0, 0]);
        assert_eq!(u32le(&p, 0x8c), Some(8));
        assert_eq!(&p[0x90..0x98], &[b'N', 0, b'T', 0, b'F', 0, b'S', 0]);
        // nothing else is set
        let known = |i: usize| i < 0xa0 || (i % 512 == 510 || i % 512 == 511);
        assert!(p.iter().enumerate().all(|(i, &b)| known(i) || b == 0));
    }

    #[test]
    fn the_lsns_follow_windows_pattern_and_the_pages_differ_only_in_their_sequence_number() {
        let h = clean_head(42467328, 0x1234_5678);
        let (p0, p1) = (&h[..4096], &h[4096..]);
        let lsn = u64le(p0, 0x30).unwrap();
        assert_eq!(lsn, (2u64 << 23) | 0x6111, "41 sequence bits leave 23 for the offset: 0x1006111, as Windows wrote");
        assert_eq!(u64le(p0, 0x70), Some(lsn - 13));
        assert_eq!(u64le(p0, 0x78), Some(lsn));
        assert_eq!(u32le(p0, 0x58), Some(0x1234_5678));
        let diff: Vec<usize> = (0..4096).filter(|&i| p0[i] != p1[i]).collect();
        assert_eq!(diff, vec![0x1e, 0x1fe, 0x3fe, 0x5fe, 0x7fe, 0x9fe, 0xbfe, 0xdfe, 0xffe]);
        for pg in [p0, p1] {
            let usn = u16le(pg, 0x1e).unwrap();
            assert!((1..=8).all(|s| u16le(pg, s * 512 - 2) == Some(usn)), "every sector ends with the sequence number");
        }
    }

    #[test]
    fn a_built_head_reads_back_as_clean_and_an_emptied_one_as_emptied() {
        assert_eq!(journal_state(&clean_head(2097152, 1)), State::Clean);
        assert_eq!(journal_state(&[0xff; HEAD]), State::Emptied);
        assert_eq!(journal_state(&[0u8; HEAD]), State::Unknown);
        // one valid restart page is enough for Windows, so a clean first page means clean whatever the second holds
        let mut first_only = [0xffu8; HEAD];
        first_only[..4096].copy_from_slice(&clean_head(2097152, 1)[..4096]);
        assert_eq!(journal_state(&first_only), State::Clean);
        // only the first page emptied: not the case we know how to fix, so it is left alone
        let mut second_only = [0xffu8; HEAD];
        second_only[4096..].copy_from_slice(&clean_head(2097152, 1)[4096..]);
        assert_eq!(journal_state(&second_only), State::Unknown);
        let mut dirty = clean_head(2097152, 1);
        dirty[0x30 + 14] = 0; // the clean flag
        assert_eq!(journal_state(&dirty), State::Unfinished);
        assert_eq!(journal_state(&[0xff; 100]), State::Unknown);
    }

    #[test]
    fn it_finds_an_emptied_journal_through_an_mbr() {
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        let v = scan(&mut img);
        assert_eq!(v, vec![Volume { start_lba: 2048, log_lba: 2048 + 78 * 8, log_size: 2 * 1024 * 1024, state: State::Emptied }]);
    }

    #[test]
    fn it_reports_clean_and_unfinished_journals_without_touching_them() {
        let mut img = drive(2048, &clean_head(2 * 1024 * 1024, 5));
        img.put(0, &mbr(2048));
        assert_eq!(scan(&mut img)[0].state, State::Clean);
        let mut dirty = clean_head(2 * 1024 * 1024, 5);
        dirty[0x30 + 14] = 0;
        let mut img = drive(2048, &dirty);
        img.put(0, &mbr(2048));
        assert_eq!(scan(&mut img)[0].state, State::Unfinished);
    }

    #[test]
    fn it_finds_volumes_through_gpt_and_without_a_table() {
        // protective MBR + GPT header + one entry, volume at sector 2048
        let mut img = drive(2048, &[0xff; HEAD]);
        let mut m = vec![0u8; 512];
        m[446 + 4] = 0xee;
        m[446 + 8] = 1;
        m[510] = 0x55;
        m[511] = 0xaa;
        img.put(0, &m);
        let mut h = vec![0u8; 512];
        h[0..8].copy_from_slice(b"EFI PART");
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        img.put(512, &h);
        let mut e = vec![0u8; 128];
        e[..16].copy_from_slice(&[0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7]);
        e[32..40].copy_from_slice(&2048u64.to_le_bytes());
        e[40..48].copy_from_slice(&200_000u64.to_le_bytes());
        img.put(1024, &e);
        let v = scan(&mut img);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].start_lba, v[0].state), (2048, State::Emptied));
        // no partition table at all: the volume starts at sector 0
        let mut img = drive(0, &[0xff; HEAD]);
        assert_eq!(scan(&mut img)[0].start_lba, 0);
    }

    #[test]
    fn anything_it_does_not_understand_is_left_alone() {
        let mut ok = drive(2048, &[0xff; HEAD]);
        ok.put(0, &mbr(2048));
        assert_eq!(scan(&mut ok).len(), 1);
        // a failed read anywhere
        for lba in [0, 2048, 2048 + 4 * 8 + 2 * 2, 2048 + 78 * 8] {
            let mut img = drive(2048, &[0xff; HEAD]);
            img.put(0, &mbr(2048));
            img.fail_at = Some(lba);
            assert!(scan(&mut img).is_empty(), "read failure at {lba}");
        }
        // not NTFS
        let mut img = Img::default();
        img.put(0, &mbr(2048));
        assert!(scan(&mut img).is_empty());
        // 4 KiB sectors
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        let mut b = boot(4);
        b[11..13].copy_from_slice(&4096u16.to_le_bytes());
        img.put(2048 * 512, &b);
        assert!(scan(&mut img).is_empty());
        // a torn MFT record (a sector end that does not match the update sequence number)
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        let mut r = record2(78, 512, 2 * 1024 * 1024);
        r[1022] = 0x99;
        img.put(2048 * 512 + 4 * 4096 + 2 * 1024, &r);
        assert!(scan(&mut img).is_empty());
        // record 2 that is not called $LogFile
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        let mut r = record2(78, 512, 2 * 1024 * 1024);
        r[0x38 + 24 + 66] = b'X';
        r[0x38 + 24 + 67] = 0;
        img.put(2048 * 512 + 4 * 4096 + 2 * 1024, &r);
        assert!(scan(&mut img).is_empty());
        // a journal that is too short to hold two restart pages, or sparse
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        img.put(2048 * 512 + 4 * 4096 + 2 * 1024, &record2(78, 1, 2 * 1024 * 1024));
        assert!(scan(&mut img).is_empty(), "first run shorter than 8 KiB");
        // an absurd MFT location
        let mut img = Img::default();
        img.put(0, &mbr(2048));
        let mut b = boot(4);
        b[48..56].copy_from_slice(&u64::MAX.to_le_bytes());
        img.put(2048 * 512, &b);
        assert!(scan(&mut img).is_empty());
    }

    #[test]
    fn reads_are_few_and_bounded() {
        let mut img = drive(2048, &[0xff; HEAD]);
        img.put(0, &mbr(2048));
        let _ = scan(&mut img);
        assert!(img.reads <= 8, "{} reads", img.reads);
    }

    /// Writes the pages for the sizes Windows chose (and a few more) so a real Windows machine can judge them:
    /// `PERIWIRE_DUMP_DIR=/some/dir cargo test --release dump_pages -- --ignored`
    #[test]
    #[ignore]
    fn dump_pages() {
        let dir = std::env::var("PERIWIRE_DUMP_DIR").expect("set PERIWIRE_DUMP_DIR");
        let mut seen: HashMap<u64, ()> = HashMap::new();
        for size in [2097152u64, 2686976, 4341760, 5013504, 6356992, 9043968, 14417920, 25149440, 42467328, 67108864] {
            seen.insert(size, ());
            std::fs::write(format!("{dir}/head-{size}.bin"), clean_head(size, 0x4000_0000 | size as u32)).unwrap();
        }
    }
}
